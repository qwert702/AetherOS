//! aether-ops — AetherOS AI 运维与自修复。
//!
//! M5 v0.1：常驻巡检 agent。每 15s 采集 /proc 指标 + aether-init(7312) 服务状态，
//! 发现服务异常（Exited/Failed）经 7312 ServiceControl 自修复重启（带冷却），
//! 巡检报告打串口日志。决策核心在 monitor.rs（纯函数、单测覆盖）。
//! 后续：日志监听预警、故障诊断报告、自然语言系统设置。

#[cfg(target_os = "linux")]
mod monitor;

#[cfg(target_os = "linux")]
use monitor::HealAction;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// 巡检间隔（秒）。
#[cfg(target_os = "linux")]
const INTERVAL_SECS: u64 = 15;
/// 同一单元两次自修复重启之间的冷却轮数（20 轮 × 15s = 5min）。
#[cfg(target_os = "linux")]
const COOLDOWN_ROUNDS: u64 = 20;

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
        "aether-ops: 巡检 agent 启动（{INTERVAL_SECS}s/轮 · 自修复冷却 {COOLDOWN_ROUNDS} 轮 · 通道 aether-init:7312）"
    );
    // unit -> (最近一次触发重启的轮次, 累计次数)
    let mut attempts: HashMap<String, (u64, u64)> = HashMap::new();
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

        for action in r.actions {
            if let HealAction::RestartService(unit) = action {
                let count = attempts.get(&unit).map(|(_, c)| c + 1).unwrap_or(1);
                match monitor::service_control(&unit, aether_ipc::ServiceAction::Restart) {
                    Ok(msg) => println!("aether-ops: 自修复 → 重启 {unit}（第 {count} 次）: {msg}"),
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
