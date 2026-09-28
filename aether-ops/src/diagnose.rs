//! 故障诊断报告（M5）：异常事件发生时，把"发生了什么 + 现场数据 + 已采取的行动"
//! 组装成结构化报告，落盘 /tmp/diag/ 并打串口。
//!
//! build_report 是纯函数（输入现场 → 输出报告文本），单测覆盖；
//! 涉及文件的操作只在 main 侧执行。

use crate::monitor::Metrics;
use aether_ipc::ServiceStatus;

/// 诊断事件类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incident {
    /// 服务退出且已触发/计划自愈
    Crash { unit: String, exit_code: i32 },
    /// 日志异常行
    LogAlert { unit: String, line: String },
    /// 内存压力告警
    MemPressure,
}

/// 组装诊断报告。
/// `heal_note`: 本次已采取的行动描述（可为空）；`log_tail`: 相关服务日志尾部（最新在后）。
pub fn build_report(
    incident: &Incident,
    services: &[ServiceStatus],
    metrics: &Metrics,
    heal_note: &str,
    log_tail: &str,
) -> String {
    let (title, unit) = match incident {
        Incident::Crash { unit, exit_code } => {
            (format!("服务「{unit}」异常退出 (code={exit_code})"), unit.clone())
        }
        Incident::LogAlert { unit, .. } => {
            (format!("服务「{unit}」日志异常"), unit.clone())
        }
        Incident::MemPressure => ("内存压力告警".into(), String::new()),
    };

    let mut r = String::new();
    r.push_str("=== AetherOS 诊断报告 ===\n");
    r.push_str(&format!("[事件] {title}\n"));
    // 触发告警的**那一行原文**：报告里最有价值的就是它 ——
    // 只说"日志异常"等于没说，得让人看到到底是哪一行、错在哪。
    // （这里原来漏了，`line` 参数根本没被使用，是测试在 Linux 下跑起来才发现的）
    if let Incident::LogAlert { line, .. } = incident {
        r.push_str(&format!("[日志] {line}\n"));
    }
    r.push_str(&format!("[时间] 运行 {}s\n", metrics.uptime_secs.unwrap_or(0)));
    match (metrics.mem_avail_mb, metrics.mem_total_mb) {
        (Some(a), Some(t)) => r.push_str(&format!("[内存] 可用 {a}/{t}MB（{:.0}%）\n", a as f32 / t.max(1) as f32 * 100.0)),
        _ => r.push_str("[内存] 未知\n"),
    }
    r.push_str("[服务] ");
    if services.is_empty() {
        r.push_str("状态不可得\n");
    } else {
        r.push_str(
            &services
                .iter()
                .map(|s| format!("{}={}", s.unit, if s.state == "Running" { "✓" } else { &s.state }))
                .collect::<Vec<_>>()
                .join(" "),
        );
        r.push('\n');
    }
    if !heal_note.is_empty() {
        r.push_str(&format!("[行动] {heal_note}\n"));
    }
    // 相关日志尾部：Crash/LogAlert 取涉事服务，MemPressure 取 aetherd（最可能的相关方）
    let target = match incident {
        Incident::MemPressure => "aetherd".to_string(),
        _ => unit,
    };
    let tail = log_tail.trim();
    if !tail.is_empty() {
        r.push_str(&format!("[日志] {target} 尾部：\n"));
        for line in tail.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
            r.push_str(&format!("  | {line}\n"));
        }
    } else {
        r.push_str(&format!("[日志] {target} 无记录\n"));
    }
    if metrics.mem_pressure() {
        r.push_str("[附注] 内存紧张可能与本事件相关（可用 <10%），建议排查泄漏或增加内存。\n");
    }
    r
}

/// 从一段日志文本取尾部 n 行（保持原顺序）。
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(unit: &str, state: &str) -> ServiceStatus {
        // 字段要与 `aether_ipc::ServiceStatus` 同步 —— 这个文件是
        // `#[cfg(target_os = "linux")]`，Windows 上不编译，所以本机 `cargo test`
        // 发现不了漏字段，**只有构建机（Linux）会报错**（2026-09-28 实测）
        ServiceStatus {
            unit: unit.into(),
            state: state.into(),
            pid: None,
            restart: true,
            essential: false,
        }
    }

    #[test]
    fn crash_report_contains_context() {
        let services = vec![svc("aetherd", "Running"), svc("compositor", "Exited { code: -1 }")];
        let m = Metrics { mem_total_mb: Some(512), mem_avail_mb: Some(40), uptime_secs: Some(300) };
        let r = build_report(
            &Incident::Crash { unit: "compositor".into(), exit_code: -1 },
            &services,
            &m,
            "已自愈重启 (pid=140)",
            "frame 100\nframe 101\nquery failed",
        );
        assert!(r.contains("compositor」异常退出 (code=-1)"));
        assert!(r.contains("运行 300s"));
        assert!(r.contains("可用 40/512MB"));
        assert!(r.contains("已自愈重启"));
        assert!(r.contains("| query failed")); // 尾部 5 行保留最后一行
        assert!(r.contains("内存紧张")); // 40/512 < 10%
    }

    #[test]
    fn log_alert_report_targets_unit() {
        let r = build_report(
            &Incident::LogAlert { unit: "aetherd".into(), line: "ERROR LLM 请求失败".into() },
            &[svc("aetherd", "Running")],
            &Metrics::default(),
            "",
            "",
        );
        assert!(r.contains("aetherd」日志异常"));
        assert!(r.contains("ERROR LLM 请求失败"));
        assert!(r.contains("无记录"));
        assert!(!r.contains("[行动]"));
    }

    #[test]
    fn tail_lines_keeps_order() {
        let t = tail_lines("1\n2\n3\n4\n5\n6", 3);
        assert_eq!(t, "4\n5\n6");
        assert_eq!(tail_lines("", 3), "");
    }
}
