//! 服务管理器：状态机 + 依赖排序 + 监督重启。
//!
//! 设计原则（对应 ARCHITECTURE.md ADR"小而美"）：
//! - 依赖只决定启动顺序（拓扑），不做按需激活
//! - 监督策略简单可靠：restart 服务退出后指数退避重启
//! - 进程启动只经 unit::KnownService::spawn_command() 的白名单字面量构造
//! - 全部逻辑与平台无关，核心可在任意平台单元测试

use crate::unit::ServiceSpec;
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::process::Child;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SvcState {
    Stopped,
    Running,
    Exited { code: i32 },
    Failed { reason: String },
}

#[derive(Clone, Debug)]
pub struct SvcStatus {
    pub name: String,
    pub state: SvcState,
    pub pid: Option<u32>,
    pub restarts: u32,
    /// 服务定义的策略标记：ops 巡检据此遵守 init 的重启策略（P1-3）
    pub restart: bool,
    /// 关键服务标记：崩溃时告警升级（P1-2）
    pub essential: bool,
}

pub struct Service {
    pub spec: ServiceSpec,
    pub state: SvcState,
    pub child: Option<Child>,
    pub restarts: u32,
    /// 上次退出时刻，用于重启退避
    pub last_exit: Option<Instant>,
    /// 最近一次启动时刻：连续运行足够久后重启计数归零（P3-1）
    pub last_start: Option<Instant>,
}

/// 进程连续运行超过此时长后的退出视为"新故障"，重启计数重新计。
const STABLE_RUNTIME: Duration = Duration::from_secs(60);

pub struct Manager {
    services: HashMap<String, Service>,
    /// 重启退避上限（2^n * 500ms，n 封顶）
    max_backoff_steps: u32,
}

impl Manager {
    pub fn new(specs: Vec<ServiceSpec>) -> Result<Self> {
        let mut services = HashMap::new();
        for spec in specs {
            spec.validate()?;
            if services.insert(spec.name.clone(), Service::new(spec)).is_some() {
                bail!("服务名重复");
            }
        }
        // 校验依赖都存在
        for s in services.values() {
            for dep in &s.spec.deps {
                if !services.contains_key(dep) {
                    bail!("服务 {} 依赖不存在的 {}", s.spec.name, dep);
                }
            }
        }
        Ok(Self { services, max_backoff_steps: 6 })
    }

    /// 拓扑排序：deps 满足"先依赖后自身"。检测环依赖。
    pub fn start_order(&self, names: &[String]) -> Result<Vec<String>> {
        fn visit(
            name: &str,
            services: &HashMap<String, Service>,
            order: &mut Vec<String>,
            visiting: &mut Vec<String>,
            done: &mut HashMap<String, bool>,
        ) -> Result<()> {
            if done.get(name).copied().unwrap_or(false) {
                return Ok(());
            }
            if visiting.iter().any(|v| v == name) {
                bail!("检测到循环依赖: {} -> {}", visiting.join(" -> "), name);
            }
            visiting.push(name.to_string());
            if let Some(svc) = services.get(name) {
                for dep in &svc.spec.deps {
                    visit(dep, services, order, visiting, done)?;
                }
            }
            visiting.pop();
            done.insert(name.to_string(), true);
            order.push(name.to_string());
            Ok(())
        }

        let mut order = Vec::new();
        let mut visiting = Vec::new();
        let mut done = HashMap::new();
        for n in names {
            visit(n, &self.services, &mut order, &mut visiting, &mut done)?;
        }
        Ok(order)
    }

    /// 开机自启动序列（按依赖拓扑排序）。
    pub fn boot_sequence(&self) -> Result<Vec<String>> {
        let names: Vec<String> = self
            .services
            .values()
            .filter(|s| s.spec.autostart)
            .map(|s| s.spec.name.clone())
            .collect();
        self.start_order(&names)
    }

    /// 启动服务（白名单字面量命令）。stdout/stderr 接日志管道（logtee）。
    pub fn start(&mut self, name: &str) -> Result<SvcStatus> {
        let Some(svc) = self.services.get_mut(name) else {
            bail!("服务 {name} 不存在");
        };
        if svc.state == SvcState::Running {
            return Ok(svc.status());
        }
        // spawn 失败进入 Failed 态：监督循环随后按退避策略接管重试
        let child = match spawn_with_logs(&svc.spec) {
            Ok(c) => c,
            Err(e) => {
                svc.state = SvcState::Failed { reason: e.to_string() };
                return Err(e);
            }
        };
        svc.state = SvcState::Running;
        svc.child = Some(child);
        svc.last_start = Some(Instant::now());
        Ok(svc.status())
    }

    /// 停止服务。
    pub fn stop(&mut self, name: &str) -> Result<SvcStatus> {
        let Some(svc) = self.services.get_mut(name) else {
            bail!("服务 {name} 不存在");
        };
        if let Some(child) = svc.child.as_mut() {
            let _ = child.kill(); // M3 后续版本升级为 SIGTERM 优雅停止
            let _ = child.wait();
        }
        svc.child = None;
        svc.state = SvcState::Stopped;
        Ok(svc.status())
    }

    /// 监督 tick：收割已退出进程、按策略退避重启。
    pub fn tick(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        let now = Instant::now();
        for svc in self.services.values_mut() {
            // 1. 收割退出的子进程
            if let Some(child) = svc.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(code)) => {
                        let code = code.code().unwrap_or(-1);
                        svc.child = None;
                        svc.last_exit = Some(now);
                        // 稳定运行过一段时间后的退出是"新故障"，重启计数归零
                        if svc
                            .last_start
                            .map(|t0| now.duration_since(t0) >= STABLE_RUNTIME)
                            .unwrap_or(false)
                        {
                            svc.restarts = 0;
                        }
                        svc.state = SvcState::Exited { code };
                        events.push(format!(
                            "{} 退出 (code={code}, restarts={})",
                            svc.spec.name, svc.restarts
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        svc.state = SvcState::Failed { reason: e.to_string() };
                        events.push(format!("{} 状态查询失败: {e}", svc.spec.name));
                    }
                }
            }
            // 2. 退避重启（Stopped = 人工停止，不重启）
            if svc.spec.restart && svc.child.is_none() && svc.state != SvcState::Stopped {
                let steps = svc.restarts.min(self.max_backoff_steps);
                let backoff = Duration::from_millis(500u64.saturating_mul(1 << steps));
                let ready = svc
                    .last_exit
                    .map(|t| now.duration_since(t) >= backoff)
                    .unwrap_or(true);
                if ready {
                    // spawn 失败同样推进退避时钟与计数，否则 200ms 后立刻重试形成风暴
                    match spawn_with_logs(&svc.spec) {
                        Ok(child) => {
                            svc.restarts += 1;
                            svc.state = SvcState::Running;
                            svc.child = Some(child);
                            svc.last_start = Some(now);
                            events.push(format!("{} 已重启 (第 {} 次)", svc.spec.name, svc.restarts));
                        }
                        Err(e) => {
                            svc.restarts += 1;
                            svc.last_exit = Some(now);
                            svc.state = SvcState::Failed { reason: e.to_string() };
                            events.push(format!("{} 重启失败（第 {} 次）: {e}", svc.spec.name, svc.restarts));
                        }
                    }
                }
            }
        }
        // PID 1 兜底收割：waitpid(-1) 收走全部已退出子进程（含孤儿）。
        // 收到的是受管服务进程时用 pid 匹配记账，状态不与上面的 try_wait 重复。
        #[cfg(target_os = "linux")]
        self.reap_all(&mut events, now);
        events
    }

    /// Linux 专用：循环 waitpid(-1, WNOHANG) 收割所有待收子进程。
    /// 孤儿（双 fork daemon、服务自己起的子进程）不收割会永久堆积为僵尸；
    /// 受管服务进程若在此处被收走，同样在此处记账（避免 Child::try_wait 撞 ECHILD）。
    #[cfg(target_os = "linux")]
    fn reap_all(&mut self, events: &mut Vec<String>, now: Instant) {
        loop {
            let mut status: i32 = 0;
            let rc = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if rc <= 0 {
                // 0 = 仍有活着的子进程；-1 = 暂无待收者（ECHILD）
                break;
            }
            let pid = rc as u32;
            if let Some(svc) = self
                .services
                .values_mut()
                .find(|s| s.child.as_ref().map(|c| c.id()) == Some(pid))
            {
                // libc 的 WIFEXITED / WEXITSTATUS 在 Linux 上是安全函数，无需 unsafe
                let code = if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else {
                    -1
                };
                svc.child = None;
                svc.last_exit = Some(now);
                if svc
                    .last_start
                    .map(|t0| now.duration_since(t0) >= STABLE_RUNTIME)
                    .unwrap_or(false)
                {
                    svc.restarts = 0;
                }
                svc.state = SvcState::Exited { code };
                events.push(format!(
                    "{} 退出 (code={code}, restarts={})",
                    svc.spec.name, svc.restarts
                ));
            }
            // 非受管孤儿：waitpid 已完成收割，直接丢弃
        }
    }

    pub fn status_all(&self) -> Vec<SvcStatus> {
        let mut v: Vec<SvcStatus> = self.services.values().map(|s| s.status()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn has(&self, name: &str) -> bool {
        self.services.contains_key(name)
    }
}

impl Service {
    fn new(spec: ServiceSpec) -> Self {
        Self {
            spec,
            state: SvcState::Stopped,
            child: None,
            restarts: 0,
            last_exit: None,
            last_start: None,
        }
    }

    fn status(&self) -> SvcStatus {
        SvcStatus {
            name: self.spec.name.clone(),
            state: self.state.clone(),
            pid: self.child.as_ref().map(|c| c.id()),
            restarts: self.restarts,
            restart: self.spec.restart,
            essential: self.spec.essential,
        }
    }
}

/// 统一的启动原语：白名单命令 + 管道化 stdio + 日志 tee。
/// start() 与 tick() 的自动重启共用，保证重启后的日志仍进
/// /var/log/aether/<unit>.log（ops 异常检测依赖它）。
fn spawn_with_logs(spec: &ServiceSpec) -> Result<Child> {
    use std::process::Stdio;
    let name = &spec.name;
    let mut command = spec.validate()?.spawn_command();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // 最小权限：`user` 不是 root 时，在 exec 之前 setgid/setuid（2026-10-02 审计 M-5）。
    //
    // 为什么放在 pre_exec：这必须是"fork 之后、exec 之前"的最后一个动作 ——
    // 在那之前降权会让子进程还没 exec 就失去读日志目录/写管道的权限。
    // 组先于用户设置（setgid 之后再 setuid，否则就没有权限改组了）。
    #[cfg(target_os = "linux")]
    if spec.user != "root" && !spec.user.is_empty() {
        use std::os::unix::process::CommandExt; // pre_exec
        let user = spec.user.clone();
        let groups = spec.groups.clone();
        let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
        let groupdb = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let uid = crate::unit::resolve_uid(&passwd, &user)
            .ok_or_else(|| anyhow::anyhow!("服务 {name} 指定了不存在的用户「{user}」"))?;
        let gid = crate::unit::resolve_gid(&groupdb, &user).unwrap_or(uid);
        unsafe {
            command.pre_exec(move || {
                // 附加组：失败即报错（宁可不启动，也不要以错误的权限跑）
                for g in &groups {
                    if let Some(gid) = crate::unit::resolve_gid(&groupdb, g) {
                        if libc::setgroups(1, &gid) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                }
                if libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("启动 {name} 失败: {e}"))?;
    crate::logtee::tee_child(name, &mut child);
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, deps: &[&str], autostart: bool) -> ServiceSpec {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "deps": deps,
            "autostart": autostart,
            "restart": false,
        }))
        .unwrap()
    }

    #[test]
    fn boot_order_respects_deps() {
        let m = Manager::new(vec![
            spec("aetherd", &["network"], true),
            spec("network", &[], true),
            spec("compositor", &["network", "aetherd"], true),
        ])
        .unwrap();
        let order = m.boot_sequence().unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("network") < pos("aetherd"));
        assert!(pos("aetherd") < pos("compositor"));
    }

    #[test]
    fn cycle_detected() {
        // aetherd 依赖 network、network 依赖 aetherd —— 环依赖必须报错
        let m = Manager::new(vec![
            spec("aetherd", &["network"], true),
            spec("network", &["aetherd"], true),
        ])
        .unwrap();
        assert!(m.boot_sequence().is_err());
    }

    #[test]
    fn unknown_service_name_rejected() {
        // 服务名必须在白名单内（json 里塞任意命令无效）
        assert!(Manager::new(vec![spec("arbitrary-command", &[], true)]).is_err());
    }

    #[test]
    fn missing_dep_rejected() {
        assert!(Manager::new(vec![spec("x", &["ghost"], true)]).is_err());
    }

    /// 带截止时间的轮询：反复 tick 直到状态满足条件或超时。
    /// 不硬编码 sleep —— Windows 上 `cmd /C exit 0` 需 600–850ms 才可回收，
    /// 固定 150ms 等待是确定性竞态（见 CODE-REVIEW P2-11）。
    fn wait_for_state(
        m: &mut Manager,
        pred: impl Fn(&SvcState) -> bool,
        timeout: Duration,
    ) -> SvcState {
        let deadline = Instant::now() + timeout;
        loop {
            m.tick();
            let state = m.status_all()[0].state.clone();
            if pred(&state) || Instant::now() >= deadline {
                return state;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn supervision_restarts_and_backs_off() {
        // noop 服务立即退出 → tick 收割 → 退避后重启 → stop 后不再重启
        let s: ServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "noop", "restart": true
        }))
        .unwrap();
        let mut m = Manager::new(vec![s]).unwrap();
        m.start("noop").unwrap();
        assert_eq!(m.status_all()[0].state, SvcState::Running);
        // 轮询等待子进程退出被收割（跨平台时序安全）
        let st = wait_for_state(
            &mut m,
            |st| matches!(st, SvcState::Exited { .. }),
            Duration::from_secs(5),
        );
        assert!(
            matches!(st, SvcState::Exited { code: 0 }),
            "noop 应以 0 退出，实际: {st:?}"
        );
        // 退避窗口内不重启：刚收割完立刻 tick，距收割只有毫秒级，而退避至少 500ms ——
        // 这一条是稳定的。
        m.tick();
        assert!(
            matches!(m.status_all()[0].state, SvcState::Exited { .. }),
            "退避窗口内不该重启"
        );
        // 退避过后应自动重启。
        //
        // ⚠️ 这里**不能**写成"睡 600ms 再断言 Running"：退避是 2^n × 500ms，而 n 取决于
        // 收割那一刻 restarts 的值（首次退出是 0，但 Linux 上退出收割与 tick 的相对顺序
        // 会让它落到 1 → 退避 1000ms > 600ms）。实测 3 次里失败 2 次。
        // 改成轮询到预期状态、超时才失败：语义（"退避过后会重启"）不变，去掉了时序脆弱性。
        let st = wait_for_state(
            &mut m,
            |st| matches!(st, SvcState::Running),
            Duration::from_secs(5),
        );
        assert_eq!(st, SvcState::Running, "退避过后应自动重启");
        assert!(m.status_all()[0].restarts >= 1, "重启计数应递增");

        m.stop("noop").unwrap();
        assert_eq!(m.status_all()[0].state, SvcState::Stopped);
        // 人工停止是粘住的：给足退避时间再 tick，仍不该被拉起
        std::thread::sleep(Duration::from_millis(1200));
        m.tick();
        assert_eq!(m.status_all()[0].state, SvcState::Stopped);
    }

    #[test]
    fn spawn_failure_backs_off() {
        // 二进制缺失时 spawn 失败必须推进退避时钟，不得形成 200ms 级重试风暴
        let s: ServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "network", "restart": true
        }))
        .unwrap();
        let mut m = Manager::new(vec![s]).unwrap();
        // 非 Linux 白名单占位命令 noop-placeholder-network 不存在 → spawn 失败
        if cfg!(target_os = "linux") {
            return; // Linux 上 /sbin/udhcpc 可能真实存在，跳过本用例
        }
        let _ = m.start("network"); // 首次启动失败 → Failed，交给监督循环
        assert!(matches!(
            m.status_all()[0].state,
            SvcState::Failed { .. }
        ));
        let deadline = Instant::now() + Duration::from_millis(700);
        let mut attempts = 0;
        while Instant::now() < deadline {
            m.tick();
            attempts = m.status_all()[0].restarts as usize;
            std::thread::sleep(Duration::from_millis(50));
        }
        // 退避生效：t=0 首试 + t≈500ms 第二次，700ms 内最多 2 次
        //（修复前每 50ms tick 都会重试，约 13 次）
        assert!(attempts <= 2, "spawn 失败应退避，实际重试 {attempts} 次");
    }
}
