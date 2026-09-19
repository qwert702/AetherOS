//! aether-ops — AetherOS AI 运维与自修复。
//!
//! M5：常驻巡检 + 日志监听。每 15s 采集 /proc 指标 + aether-init（Unix socket
//! /run/aether-init.sock）服务状态 + 各服务日志（/var/log/aether，由 aether-init
//! logtee 落盘）增量扫描：服务异常（Exited/Failed）经 ServiceControl 自修复重启
//! （带冷却，且只对 restart=true 的服务），日志异常行即时告警，全部打串口。
//! 决策核心在 monitor.rs（纯函数、单测覆盖）。后续：自然语言系统设置。

#[cfg(target_os = "linux")]
mod diagnose;
#[cfg(target_os = "linux")]
mod monitor;

#[cfg(target_os = "linux")]
use monitor::HealAction;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// aether-init 服务控制 socket（与 aether-init/ipc.rs 保持一致）。
#[cfg(target_os = "linux")]
pub const INIT_SOCKET: &str = "/run/aether-init.sock";

/// 巡检间隔（秒）。
#[cfg(target_os = "linux")]
const INTERVAL_SECS: u64 = 15;
/// 同一单元两次自修复重启之间的冷却轮数（20 轮 × 15s = 5min）。
#[cfg(target_os = "linux")]
const COOLDOWN_ROUNDS: u64 = 20;
/// 诊断报告目录（有持久化分区时随 /var 落盘保留）。
#[cfg(target_os = "linux")]
const DIAG_DIR: &str = "/var/diag";
/// 日志尾部采样大小（字节）。
#[cfg(target_os = "linux")]
const TAIL_BYTES: u64 = 8192;

/// 读取某服务日志尾部（最新 n 行）。
#[cfg(target_os = "linux")]
fn read_log_tail(unit: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let path = format!("{}/{}.log", monitor::LOG_DIR, unit);
    let Ok(mut f) = std::fs::File::open(&path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    diagnose::tail_lines(&text, 5)
}

/// 生成并落盘诊断报告（同时打串口）。任何一步失败都不影响巡检主循环。
#[cfg(target_os = "linux")]
fn emit_report(
    incident: diagnose::Incident,
    services: &[aether_ipc::ServiceStatus],
    metrics: &monitor::Metrics,
    heal_note: &str,
    unit: &str,
) {
    let report = diagnose::build_report(&incident, services, metrics, heal_note, &read_log_tail(unit));
    println!("aether-ops: {}", report.lines().next().unwrap_or_default());
    if std::fs::create_dir_all(DIAG_DIR).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = format!("{DIAG_DIR}/{ts}-{unit}.txt");
    match std::fs::write(&path, &report) {
        Ok(_) => println!("aether-ops: 诊断报告已保存 {path}"),
        Err(e) => println!("aether-ops: 诊断报告落盘失败（{e}）"),
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = run() {
            eprintln!("aether-ops: 巡检循环异常退出: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("aether-ops: 决策核心见 monitor.rs（cargo test 覆盖）；系统内以巡检 agent 常驻");
}

/// 常驻巡检循环。
#[cfg(target_os = "linux")]
fn run() -> anyhow::Result<()> {
    println!(
        "aether-ops: 巡检 agent 启动（{INTERVAL_SECS}s/轮 · 自修复冷却 {COOLDOWN_ROUNDS} 轮 · 通道 {INIT_SOCKET}）"
    );
    // unit -> (最近一次触发重启的轮次, 累计次数)
    let mut attempts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut logwatch = monitor::LogWatch::new();
    let mut prev_pressure = false;
    let mut round: u64 = 0;
    loop {
        round += 1;
        let metrics = monitor::read_metrics();
        let services = monitor::query_services().unwrap_or_else(|e| {
            println!("aether-ops: 服务状态查询失败（{e}）");
            Vec::new()
        });

        let r = monitor::plan(&services, &metrics, &attempts, round, COOLDOWN_ROUNDS);
        for line in &r.lines {
            println!("aether-ops: {line}");
        }

        // 日志监听：增量扫描各服务日志，异常行即时告警 + 诊断报告
        for (unit, line) in logwatch.scan_dir(monitor::LOG_DIR, "ops") {
            println!("aether-ops: 📢 {unit} 日志异常: {line}");
            emit_report(
                diagnose::Incident::LogAlert { unit: unit.clone(), line },
                &services, &metrics, "", &unit,
            );
        }

        // 内存压力：只在状态翻转时出一次诊断报告（避免每轮刷屏）
        let pressure = metrics.mem_pressure();
        if pressure && !prev_pressure {
            emit_report(
                diagnose::Incident::MemPressure,
                &services, &metrics, "", "aetherd",
            );
        }
        prev_pressure = pressure;

        for action in r.actions {
            if let HealAction::RestartService(unit) = action {
                let count = attempts.get(&unit).map(|(_, c)| c + 1).unwrap_or(1);
                match monitor::service_control(&unit, aether_ipc::ServiceAction::Restart) {
                    Ok(msg) => {
                        println!("aether-ops: 自修复 → 重启 {unit}（第 {count} 次）: {msg}");
                        emit_report(
                            diagnose::Incident::Crash { unit: unit.clone(), exit_code: -1 },
                            &services, &metrics,
                            &format!("自修复 → 重启 {unit}（第 {count} 次）: {msg}"),
                            &unit,
                        );
                    }
                    Err(e) => println!("aether-ops: 自修复 → 重启 {unit} 失败: {e}"),
                }
                attempts.insert(unit, (round, count));
            }
        }

        // 周期心跳：每 4 轮（1 分钟）打一条全量摘要，串口可见
        if round % 4 == 1 {
            println!(
                "aether-ops: 巡检#{} up={:?}s mem={}/{}MB 服务={}",
                round,
                metrics.uptime_secs.unwrap_or(0),
                metrics.mem_total_mb.unwrap_or(0) - metrics.mem_avail_mb.unwrap_or(0),
                metrics.mem_total_mb.unwrap_or(0),
                services
                    .iter()
                    .map(|s| format!("{}={}", s.unit, if s.state == "Running" { "✓" } else { "✗" }))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        std::thread::sleep(Duration::from_secs(INTERVAL_SECS));
    }
}
