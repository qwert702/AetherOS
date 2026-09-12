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
}

pub struct Service {
    pub spec: ServiceSpec,
    pub state: SvcState,
    pub child: Option<Child>,
    pub restarts: u32,
    /// 上次退出时刻，用于重启退避
    pub last_exit: Option<Instant>,
}

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
        let mut command = svc
            .spec
            .validate()?
            .spawn_command();
        use std::process::Stdio;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("启动 {name} 失败: {e}"))?;
        crate::logtee::tee_child(name, &mut child);
        svc.state = SvcState::Running;
        svc.child = Some(child);
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
                    let mut command = match svc.spec.validate() {
                        Ok(k) => k.spawn_command(),
                        Err(e) => {
                            svc.state = SvcState::Failed { reason: e.to_string() };
                            continue;
                        }
                    };
                    match command.spawn() {
                        Ok(child) => {
                            svc.restarts += 1;
                            svc.state = SvcState::Running;
                            svc.child = Some(child);
                            events.push(format!("{} 已重启 (第 {} 次)", svc.spec.name, svc.restarts));
                        }
                        Err(e) => {
                            svc.state = SvcState::Failed { reason: e.to_string() };
                            events.push(format!("{} 重启失败: {e}", svc.spec.name));
                        }
                    }
                }
            }
        }
        events
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
        Self { spec, state: SvcState::Stopped, child: None, restarts: 0, last_exit: None }
    }

    fn status(&self) -> SvcStatus {
        SvcStatus {
            name: self.spec.name.clone(),
            state: self.state.clone(),
            pid: self.child.as_ref().map(|c| c.id()),
            restarts: self.restarts,
        }
    }
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
        std::thread::sleep(Duration::from_millis(150));
        m.tick();
        assert!(matches!(m.status_all()[0].state, SvcState::Exited { code: 0 }));
        // 退避窗口内不重启
        m.tick();
        assert!(matches!(m.status_all()[0].state, SvcState::Exited { code: 0 }));
        // 退避过后重启
        std::thread::sleep(Duration::from_millis(600));
        m.tick();
        assert_eq!(m.status_all()[0].state, SvcState::Running);
        assert_eq!(m.status_all()[0].restarts, 1);
        m.stop("noop").unwrap();
        assert_eq!(m.status_all()[0].state, SvcState::Stopped);
        std::thread::sleep(Duration::from_millis(600));
        m.tick();
        assert_eq!(m.status_all()[0].state, SvcState::Stopped);
    }
}
