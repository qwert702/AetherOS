//! 巡检与自修复决策核心（平台无关、可单测）。
//!
//! 设计对应项目"罐头探针"原则：决策函数是纯函数（输入状态 → 输出动作），
//! 动作只映射到白名单 IPC 调用（aether-init 7312 的 ServiceControl），
//! 没有字符串拼命令的注入面。

use aether_ipc::{Request, Response, ServiceAction, ServiceStatus};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
// 只有非 Linux（开发自检走回环 TCP）才用得到：Linux 走 Unix socket
#[cfg(not(target_os = "linux"))]
use std::net::TcpStream;
use std::time::Duration;

/// 一轮巡采到的系统指标。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metrics {
    pub mem_total_mb: Option<u64>,
    pub mem_avail_mb: Option<u64>,
    pub uptime_secs: Option<u64>,
}

impl Metrics {
    /// 内存压力：可用 < 总量的 10% 视为紧张。
    pub fn mem_pressure(&self) -> bool {
        match (self.mem_avail_mb, self.mem_total_mb) {
            (Some(a), Some(t)) if t > 0 => a * 10 < t,
            _ => false,
        }
    }
}

/// 自修复动作（执行端只映射白名单 IPC）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealAction {
    RestartService(String),
}

/// 一轮巡检结论：汇报行 + 待执行动作。
#[derive(Debug, Default, PartialEq)]
pub struct Round {
    pub lines: Vec<String>,
    pub actions: Vec<HealAction>,
}

/// 巡检决策：
/// - `Running` 健康；`Stopped` 视为人工停止（不干预，避免和操作者打架）；
/// - `Exited`/`Failed` 请求重启，但同一单元要过 `cooldown_rounds` 轮冷却，
///   且只对 `restart: true` 的服务生效 —— 重启策略以服务定义（aether-init）
///   为唯一事实来源，ops 不再对 `restart: false` 的服务擅自冷拉起（P1-3）；
/// - 关键服务（`essential: true`）异常时输出升级告警行（P1-2）。
/// - `attempts`: unit -> (最近一次触发重启的轮次, 累计次数)
pub fn plan(
    services: &[ServiceStatus],
    metrics: &Metrics,
    attempts: &HashMap<String, (u64, u64)>,
    round: u64,
    cooldown_rounds: u64,
) -> Round {
    let mut r = Round::default();

    for s in services {
        if s.unit == "ops" {
            continue; // 不巡检自身，避免自愈风暴
        }
        if s.state == "Running" {
            continue;
        }
        if s.state == "Stopped" {
            r.lines.push(format!("○ {} 已被人工停止（不干预）", s.unit));
            continue;
        }
        // 关键服务异常：升级告警（无论是否可重启）
        if s.essential {
            r.lines
                .push(format!("‼ 关键服务 {} 异常（状态 {}），需优先排查", s.unit, s.state));
        }
        // 重启策略归 aether-init 服务定义：restart=false 的服务 ops 不冷拉起
        if !s.restart {
            r.lines
                .push(format!("○ {} 状态 {}（重启策略为不重启，不干预）", s.unit, s.state));
            continue;
        }
        let cooling = attempts
            .get(&s.unit)
            .map(|(last, _)| round.saturating_sub(*last) < cooldown_rounds)
            .unwrap_or(false);
        if cooling {
            r.lines
                .push(format!("⚠ {} 状态 {}（自修复冷却中）", s.unit, s.state));
            continue;
        }
        r.actions.push(HealAction::RestartService(s.unit.clone()));
    }

    if metrics.mem_pressure() {
        r.lines.push(format!(
            "⚠ 内存紧张：可用 {}MB / {}MB",
            metrics.mem_avail_mb.unwrap_or(0),
            metrics.mem_total_mb.unwrap_or(0)
        ));
    }
    r
}

/// 从 /proc/meminfo 与 /proc/uptime 文本解析指标（入参字符串化以便单测）。
pub fn parse_metrics(meminfo: &str, uptime: &str) -> Metrics {
    let mut m = Metrics::default();
    for line in meminfo.lines() {
        let mut it = line.split_whitespace();
        let key = it.next().unwrap_or("");
        let val: Option<u64> = it.next().and_then(|v| v.parse().ok());
        // /proc/meminfo 数值单位 kB
        match key {
            "MemTotal:" => m.mem_total_mb = val.map(|kv| kv / 1024),
            "MemAvailable:" => m.mem_avail_mb = val.map(|kv| kv / 1024),
            _ => {}
        }
    }
    m.uptime_secs = uptime
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|f| f as u64);
    m
}

#[cfg(target_os = "linux")]
/// 读取本机 /proc 指标。
pub fn read_metrics() -> Metrics {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let uptime = std::fs::read_to_string("/proc/uptime").unwrap_or_default();
    parse_metrics(&meminfo, &uptime)
}

/// 向 aether-init 发一条请求并取回单个响应。
/// Linux 走 Unix socket（与 aether-init 的 0600 服务控制通道一致）；
/// 非 Linux（开发自检）走回环 TCP。
pub fn call_init(req: &Request) -> anyhow::Result<Response> {
    #[cfg(target_os = "linux")]
    let mut writer = {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(crate::INIT_SOCKET)?;
        stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
        stream
    };
    #[cfg(not(target_os = "linux"))]
    let mut writer = {
        let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::INIT_PORT).into();
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
        stream
    };
    writer.write_all(aether_ipc::encode(req).as_bytes())?;
    let mut line = String::new();
    BufReader::new(writer).read_line(&mut line)?;
    Ok(aether_ipc::decode::<Response>(&line)?)
}

/// 查询全部服务状态（SysInfo → aether-init）。
pub fn query_services() -> anyhow::Result<Vec<ServiceStatus>> {
    match call_init(&Request::SysInfo {
        scope: aether_ipc::SysInfoScope::Services,
    })? {
        Response::SysInfo(report) => Ok(report.services),
        Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
        _ => Err(anyhow::anyhow!("aether-init 返回了意外响应")),
    }
}

/// 执行服务控制动作。
pub fn service_control(unit: &str, action: ServiceAction) -> anyhow::Result<String> {
    match call_init(&Request::ServiceControl {
        unit: unit.to_string(),
        action,
    })? {
        Response::ServiceAck { ok, message, .. } if ok => Ok(message),
        Response::ServiceAck { message, .. } => Err(anyhow::anyhow!(message)),
        Response::Error { message, .. } => Err(anyhow::anyhow!(message)),
        _ => Err(anyhow::anyhow!("aether-init 返回了意外响应")),
    }
}

/// 服务日志目录（与 aether-init logtee 保持一致；持久化时在真磁盘上）。
pub const LOG_DIR: &str = "/var/log/aether";

/// 日志异常模式（字面量罐头；命中即告警）。
pub const ALERT_PATTERNS: [&str; 5] =
    ["panic", "Segmentation fault", "ERROR", "查询失败", "无法连接"];

/// 增量扫描一段新增日志文本，返回命中的告警行（每行截断到 120 字符）。
pub fn scan_new_lines(new_text: &str) -> Vec<String> {
    new_text
        .lines()
        .filter(|l| ALERT_PATTERNS.iter().any(|p| l.contains(p)))
        .map(|l| l.chars().take(120).collect())
        .collect()
}

/// 日志监听器：按文件维护读取偏移，每轮只扫新增内容（文件变小视为轮转/截断，从头重扫）。
pub struct LogWatch {
    offsets: std::collections::HashMap<String, u64>,
}

impl Default for LogWatch {
    fn default() -> Self {
        Self { offsets: std::collections::HashMap::new() }
    }
}

impl LogWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 扫描目录下所有 .log 文件的新增内容；`skip` 为要跳过的单元名（ops 自身，防自告警循环）。
    /// 返回 (单元名, 告警行) 列表。
    pub fn scan_dir(&mut self, dir: &str, skip: &str) -> Vec<(String, String)> {
        let mut alerts = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return alerts;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // 不跟随符号链接（2026-10-02 审计 L-2）：日志目录里放一个
            // `aetherd.log -> /etc/shadow`，就会让 ops 把目标文件的内容读进
            // 诊断报告并落到 /var/diag。用 symlink_metadata 判"路径本身"的类型。
            //
            // 取不到元数据时**也跳过**（代码审查指出原来这里是 fail-open）：
            // "读不到类型"与"不是符号链接"是两件事，而这一层的取舍是宁可漏读一个日志，
            // 也不要冒读进任意文件的风险。
            match std::fs::symlink_metadata(&path) {
                Ok(md) if !md.file_type().is_symlink() => {}
                _ => continue,
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if path.extension().and_then(|e| e.to_str()) != Some("log") || name == skip {
                continue;
            }
            let Ok(mut file) = std::fs::File::open(&path) else { continue };
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let offset = self.offsets.entry(name.to_string()).or_insert(0);
            if len < *offset {
                *offset = 0; // 截断/轮转
            }
            if len == *offset {
                continue;
            }
            use std::io::{Read, Seek, SeekFrom};
            // 复用上面已打开的句柄：失败降级为跳过本轮，绝不 panic
            if file.seek(SeekFrom::Start(*offset)).is_err() {
                continue;
            }
            // 按**字节**读、再按 UTF-8 有损解码（2026-10-02 审计 M-6）。
            //
            // 原来用 `read_to_string`：遇到非 UTF-8 就 `continue`，而**偏移不推进** ——
            // 于是日志里只要有一个坏字节（终端输出、崩溃转储、被截断的多字节字符），
            // 这个文件就**从此永远扫不到**，后面的告警全部丢失。有损解码把坏字节
            // 变成 U+FFFD，字面量匹配不受影响，而偏移照常前进。
            let mut buf = Vec::new();
            if file.read_to_end(&mut buf).is_err() {
                continue; // 只有真的读失败才跳过（偏移不动，下一轮重试）
            }
            // 偏移推进按实际读取字节数：metadata 与读取之间文件增长时不会重复扫描
            *offset += buf.len() as u64;
            let text = String::from_utf8_lossy(&buf);
            for line in scan_new_lines(&text) {
                alerts.push((name.to_string(), line));
            }
        }
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(unit: &str, state: &str) -> ServiceStatus {
        ServiceStatus {
            unit: unit.into(),
            state: state.into(),
            pid: None,
            restart: true,
            essential: false,
        }
    }

    /// M-6 回归：日志里出现非 UTF-8 字节时，**偏移必须照常推进**。
    ///
    /// 修复前的行为：`read_to_string` 失败 → `continue` 且偏移不动 ⇒ 这个文件
    /// 从此永远扫不到（一个坏字节让后面的所有告警消失）。这里在坏字节**之后**
    /// 写一行告警，它必须被扫出来。
    #[test]
    fn non_utf8_log_still_advances_and_finds_later_alerts() {
        let dir = std::env::temp_dir().join("aether-ops-m6");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("aetherd.log");

        // 坏字节 + 换行 + 一行会命中的告警
        let mut data = vec![0xFFu8, 0xFE, b'\n'];
        data.extend_from_slice("panic: 故意构造的告警行\n".as_bytes());
        std::fs::write(&log, &data).unwrap();

        let mut w = LogWatch::new();
        let first = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert!(
            first.iter().any(|(_, l)| l.contains("panic")),
            "坏字节之后的那行告警必须被扫到，实得 {first:?}"
        );
        // 第二轮：没有新增内容 ⇒ 不应重复上报（偏移确实推进了）
        let second = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert!(second.is_empty(), "偏移未推进会导致重复上报，实得 {second:?}");

        // 追加一行再扫：只报新增的那行
        let mut more = std::fs::read(&log).unwrap();
        more.extend_from_slice("ERROR 又一条告警\n".as_bytes());
        std::fs::write(&log, &more).unwrap();
        let third = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert_eq!(third.len(), 1, "应只报新增的一行，实得 {third:?}");
        assert!(third[0].1.contains("又一条告警"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_desktop_is_noop() {
        let services = vec![
            svc("network", "Running"),
            svc("aetherd", "Running"),
            svc("compositor", "Running"),
        ];
        let r = plan(&services, &Metrics::default(), &HashMap::new(), 1, 20);
        assert!(r.actions.is_empty());
        assert!(r.lines.is_empty());
    }

    #[test]
    fn crashed_service_triggers_restart() {
        let services = vec![svc("aetherd", "Exited { code: 101 }")];
        let r = plan(&services, &Metrics::default(), &HashMap::new(), 1, 20);
        assert_eq!(r.actions, vec![HealAction::RestartService("aetherd".into())]);
    }

    #[test]
    fn cooldown_suppresses_repeat_restart() {
        let services = vec![svc("aetherd", "Failed { reason: boom }")];
        let mut attempts = HashMap::new();
        attempts.insert("aetherd".into(), (0, 1));
        // 轮次 5，冷却 20 轮 → 抑制
        let r = plan(&services, &Metrics::default(), &attempts, 5, 20);
        assert!(r.actions.is_empty());
        // 轮次 25 → 冷却已过，再次触发
        let r = plan(&services, &Metrics::default(), &attempts, 25, 20);
        assert_eq!(r.actions.len(), 1);
    }

    #[test]
    fn manually_stopped_is_left_alone() {
        let services = vec![svc("getty", "Stopped")];
        let r = plan(&services, &Metrics::default(), &HashMap::new(), 1, 20);
        assert!(r.actions.is_empty());
        assert!(r.lines[0].contains("人工停止"));
    }

    #[test]
    fn ops_never_self_heals() {
        let services = vec![svc("ops", "Exited { code: 1 }")];
        let r = plan(&services, &Metrics::default(), &HashMap::new(), 1, 20);
        assert!(r.actions.is_empty());
    }

    #[test]
    fn no_restart_policy_is_respected() {
        // 服务定义 restart=false：ops 不得冷拉起（P1-3）
        let mut s = svc("compositor", "Exited { code: 101 }");
        s.restart = false;
        let r = plan(&[s], &Metrics::default(), &HashMap::new(), 1, 20);
        assert!(r.actions.is_empty());
        assert!(r.lines.iter().any(|l| l.contains("不重启")));
    }

    #[test]
    fn essential_crash_escalates() {
        // 关键服务崩溃：输出升级告警行（P1-2）
        let mut s = svc("aetherd", "Exited { code: 101 }");
        s.essential = true;
        let r = plan(&[s], &Metrics::default(), &HashMap::new(), 1, 20);
        assert!(r.lines.iter().any(|l| l.contains("关键服务")));
        assert_eq!(r.actions.len(), 1); // 仍然重启（restart 默认 true）
    }

    #[test]
    fn mem_pressure_detected() {
        let m = parse_metrics(
            "MemTotal:        524288 kB\nMemFree:          102400 kB\nMemAvailable:      40960 kB\n",
            "123.45 678.90",
        );
        assert_eq!(m.mem_total_mb, Some(512));
        assert_eq!(m.mem_avail_mb, Some(40));
        assert_eq!(m.uptime_secs, Some(123));
        assert!(m.mem_pressure());
        let ok = Metrics { mem_total_mb: Some(512), mem_avail_mb: Some(200), uptime_secs: None };
        assert!(!ok.mem_pressure());
    }

    #[test]
    fn log_scan_hits_literals_only() {
        // 模式是**字面量罐头**（见 ALERT_PATTERNS），不猜语义：
        // 实机上那行 `[aetherd] ERROR LLM 请求失败（…）` 之所以被捕获，
        // 是因为它含 "ERROR"，不是因为 "请求失败"（模式里没有这个词）
        let alerts = scan_new_lines(
            "starting ok\nthread 'main' panicked at foo\n[aetherd] ERROR LLM 请求失败: timeout\nnothing wrong here\n",
        );
        assert_eq!(alerts.len(), 2);
        assert!(alerts[0].contains("panicked"));
        assert!(alerts[1].contains("ERROR"));
        assert!(scan_new_lines("all good\n").is_empty());
        // 只有"请求失败"、没有 ERROR 的行**不该**告警 —— 这正是 literals_only 的含义
        assert!(
            scan_new_lines("LLM 请求失败: timeout\n").is_empty(),
            "模式里没有「请求失败」，不该命中"
        );
        // 其余字面量也要能命中
        assert_eq!(scan_new_lines("x Segmentation fault y\n").len(), 1);
        assert_eq!(scan_new_lines("aether-ops: 服务状态查询失败\n").len(), 1);
    }

    #[test]
    fn log_watch_incremental_and_skip_self() {
        let dir = std::env::temp_dir().join(format!("aether-ops-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aetherd.log"), "[aetherd] 通道: Local\n").unwrap();
        std::fs::write(dir.join("ops.log"), "ops: 自修复 → 重启 x 失败\n").unwrap();

        let mut w = LogWatch::new();
        let alerts = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert!(alerts.is_empty(), "正常行不告警");

        // 追加异常行 → 下一轮扫到
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("aetherd.log")).unwrap();
        f.write_all("[aetherd] LLM 无法连接: refused\n".as_bytes()).unwrap();
        let alerts = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].0, "aetherd");
        assert!(alerts[0].1.contains("无法连接"));

        // 已扫过的不再重复告警
        let alerts = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert!(alerts.is_empty());

        // 文件截断 → 从头重扫
        std::fs::write(dir.join("aetherd.log"), "panic: boom\n").unwrap();
        let alerts = w.scan_dir(dir.to_str().unwrap(), "ops");
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].1.contains("panic"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
