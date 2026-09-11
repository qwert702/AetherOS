//! 巡检与自修复决策核心（平台无关、可单测）。
//!
//! 设计对应项目"罐头探针"原则：决策函数是纯函数（输入状态 → 输出动作），
//! 动作只映射到白名单 IPC 调用（aether-init 7312 的 ServiceControl），
//! 没有字符串拼命令的注入面。

use aether_ipc::{Request, Response, ServiceAction, ServiceStatus};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
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
///   防止与 aether-init 的监督退避叠加成重启风暴。
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

/// 向 aether-init（127.0.0.1:7312）发一条请求并取回单个响应。
pub fn call_init(req: &Request) -> anyhow::Result<Response> {
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::INIT_PORT).into();
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    stream.write_all(aether_ipc::encode(req).as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(unit: &str, state: &str) -> ServiceStatus {
        ServiceStatus { unit: unit.into(), state: state.into(), pid: None }
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
}
