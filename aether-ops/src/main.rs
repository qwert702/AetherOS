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

/// aether-init 服务控制 socket。
///
/// 值来自协议 crate（`aether_ipc::INIT_SOCKET_PATH`）：监听侧（aether-init）与
/// 所有连接侧（aetherd / aether-ops）共用同一个常量，避免三处字面量漂开
/// （2026-10-02 批次 I 收拢）。
#[cfg(target_os = "linux")]
pub const INIT_SOCKET: &str = aether_ipc::INIT_SOCKET_PATH;

/// 巡检间隔（秒）。
#[cfg(target_os = "linux")]
const INTERVAL_SECS: u64 = 15;
/// 同一单元两次自修复重启之间的冷却轮数（20 轮 × 15s = 5min）。
#[cfg(target_os = "linux")]
const COOLDOWN_ROUNDS: u64 = 20;
/// 同一单元的**日志告警报告**两次之间至少隔多少轮（20 轮 × 15s = 5min）。
///
/// 为什么需要（2026-10-06 审计 HA）：`logwatch.scan_dir` 返回**自上次扫描以来的
/// 新增异常行**，原来**每一行**都出一份报告 —— 而报告写到 `/var/diag`（**持久化分区**）。
/// 一个持续打异常日志的服务会每 15 秒写 N 个文件，`/var` 会被慢慢填满。
/// 同文件里"内存压力"那段早就用了"只在状态翻转时出一次"的模式，日志告警这一路漏了。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 仅 Linux 构建会调用（附录 HA）
const LOG_ALERT_COOLDOWN_ROUNDS: u64 = 20;

/// 该不该为这个单元出日志告警报告（**纯函数**，便于单测）。
///
/// `last_round` 是上次为**同一个单元**出报告的轮次；`None` 表示还没出过。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 仅 Linux 构建会调用（附录 HA）
fn log_alert_due(last_round: Option<u64>, round: u64) -> bool {
    match last_round {
        None => true,
        Some(last) => round.saturating_sub(last) >= LOG_ALERT_COOLDOWN_ROUNDS,
    }
}

/// 诊断报告目录最多保留多少个文件。
///
/// 为什么需要（2026-10-06 审计 HB）：`/var/diag` 是**持久化分区**上的目录，
/// 每份报告几 KB。附录 HA 已经给"写报告"加了速率节流（同一单元 5 分钟一份），
/// 但**目录本身没有上限** —— 服务多、故障多的时候仍会慢慢堆。
/// 200 份 ≈ 几 MB，足够回看，又不会撑满分区。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 仅 Linux 构建会调用（附录 HB）
const MAX_DIAG_FILES: usize = 200;

/// 该删哪些文件，才能把目录压到上限以内（**纯函数**，便于单测）。
///
/// 入参是 `(文件名, 修改时间秒)`；返回要删的文件名（最旧的先删）。
/// 只保留最新的 `max` 个；不超过上限时返回空。
/// 时间相同时按文件名兜底排序，保证**同一份输入永远得到同一份输出**（确定性）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 仅 Linux 构建会调用（附录 HB）
fn files_to_prune(mut entries: Vec<(String, u64)>, max: usize) -> Vec<String> {
    if entries.len() <= max {
        return Vec::new();
    }
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let drop_n = entries.len() - max;
    entries.drain(..drop_n).map(|(name, _)| name).collect()
}

/// 按上限清理 `/var/diag`。
///
/// 读目录失败、或读不到修改时间时**跳过**（不阻断本次报告写入）——
/// 与 `perm.rs::rotate_if_needed` 的取舍一致：轮转失败不该让主流程失败。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // 仅 Linux 构建会调用（附录 HB）
fn prune_diag_dir(dir: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let entries: Vec<(String, u64)> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ts = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (name, ts)
        })
        .collect();
    for name in files_to_prune(entries, MAX_DIAG_FILES) {
        let _ = std::fs::remove_file(std::path::Path::new(dir).join(name));
    }
}

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
    // 目录上限（附录 HB）：不设上限的话，故障多的机器会慢慢堆满 /var。
    prune_diag_dir(DIAG_DIR);
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        // §5.7 一键诊断包：**不必等故障**，用户想让别人看的时候就能立刻拿到一份。
        // 放在巡检循环之前：一次性动作，做完就退出（不进入常驻循环）。
        if std::env::args().any(|a| a == "--bundle") {
            bundle();
            return;
        }
        if std::env::args().any(|a| a == "--help" || a == "-h") {
            println!("用法：aether-ops [--bundle]");
            println!("  --bundle  立即导出一份诊断报告（含服务状态/内存/日志尾部），打印文件路径");
            println!("  无参数    以巡检 agent 常驻运行");
            return;
        }
        if let Err(e) = run() {
            eprintln!("aether-ops: 巡检循环异常退出: {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("aether-ops: 决策核心见 monitor.rs（cargo test 覆盖）；系统内以巡检 agent 常驻");
}

/// 一次性导出诊断包（§5.7）：打印**确切路径**，方便直接贴到 issue 里。
#[cfg(target_os = "linux")]
fn bundle() {
    let services = monitor::query_services().unwrap_or_else(|e| {
        // 查不到服务状态也要出报告（缺的那部分如实写"查询失败"），
        // 而不是因为一个子查询失败就什么都不给 —— 排障时"什么都没有"最没用。
        println!("aether-ops: 服务状态查询失败（{e}），报告里会标注");
        Vec::new()
    });
    let metrics = monitor::read_metrics();
    emit_report(
        diagnose::Incident::Manual,
        &services,
        &metrics,
        "手动导出（aether-ops --bundle）",
        "ops",
    );
    println!("aether-ops: 把上面那个 .txt 的内容贴到 issue 即可（不含任何凭据/密钥）");
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
    // 每个单元上次出"日志告警报告"的轮次（附录 HA 的节流）。
    let mut log_alert_last: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
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
            // 节流（附录 HA）：同一单元 5 分钟内只出一份报告 ——
            // 否则"每一行异常"都会写一个文件到 /var/diag。
            if log_alert_due(log_alert_last.get(&unit).copied(), round) {
                emit_report(
                    diagnose::Incident::LogAlert { unit: unit.clone(), line },
                    &services, &metrics, "", &unit,
                );
                log_alert_last.insert(unit.clone(), round);
            }
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
            // 用 match 而不是 `if let`：`HealAction` 目前只有一个变体，`if let` 会因为
            // "不可反驳"而报警告；更要紧的是 —— **将来加变体时 match 会强制编译报错**，
            // 而 `if let` 会把新动作静默忽略。对自修复链路来说，静默忽略是最坏的失败方式。
            match action {
                HealAction::RestartService(unit) => {
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

#[cfg(test)]
mod diag_prune_tests {
    use super::{files_to_prune, MAX_DIAG_FILES};

    fn e(name: &str, ts: u64) -> (String, u64) {
        (name.to_string(), ts)
    }

    #[test]
    fn 不超过上限时什么都不删() {
        let v = vec![e("a", 1), e("b", 2), e("c", 3)];
        assert_eq!(files_to_prune(v, MAX_DIAG_FILES), Vec::<String>::new());
    }

    #[test]
    fn 刚好等于上限时什么都不删() {
        let v: Vec<_> = (0..10).map(|i| e(&format!("f{i}"), i)).collect();
        assert_eq!(files_to_prune(v, 10), Vec::<String>::new());
    }

    #[test]
    fn 超过上限时删最旧的_保留最新的() {
        let v: Vec<_> = (0..15).map(|i| e(&format!("f{i}"), i)).collect();
        assert_eq!(files_to_prune(v, 10), vec!["f0", "f1", "f2", "f3", "f4"]);
    }

    #[test]
    fn 时间相同时按文件名兜底_结果确定() {
        let a = vec![e("b", 7), e("a", 7), e("c", 7)];
        let b = vec![e("c", 7), e("b", 7), e("a", 7)];
        assert_eq!(files_to_prune(a, 1), files_to_prune(b, 1));
        assert_eq!(files_to_prune(vec![e("b", 7), e("a", 7), e("c", 7)], 1), vec!["a", "b"]);
    }

    #[test]
    fn prune_diag_dir_真的把目录删到上限() {
        // 集成测试：真的建目录、真的删（照 logtee.rs 的临时目录模式）。
        // 文件名用零填充的递增序号：同一秒内 mtime 会相同，
        // 而 files_to_prune 在时间相同时按**文件名**兜底 —— 所以保留的是序号最大的那些。
        let dir = std::env::temp_dir().join(format!("aether-diag-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..(MAX_DIAG_FILES + 5) {
            std::fs::write(dir.join(format!("{i:06}.txt")), b"x").unwrap();
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), MAX_DIAG_FILES + 5);

        super::prune_diag_dir(dir.to_str().unwrap());

        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), MAX_DIAG_FILES, "应删到上限");
        assert!(left.contains(&format!("{:06}.txt", MAX_DIAG_FILES + 4)));
        assert!(!left.contains(&"000000.txt".to_string()), "最旧的应被删掉");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_diag_dir_目录不存在时跳过() {
        // 读目录失败必须**跳过**，不能 panic（宁可不删也不乱删）
        super::prune_diag_dir("/definitely/not/a/real/dir/for-aether-tests");
    }

    #[test]
    fn 空目录不_panic() {
        assert_eq!(files_to_prune(Vec::new(), 10), Vec::<String>::new());
        assert_eq!(files_to_prune(Vec::new(), 0), Vec::<String>::new());
    }
}

#[cfg(test)]
mod log_alert_tests {
    use super::{log_alert_due, LOG_ALERT_COOLDOWN_ROUNDS};

    #[test]
    fn 首次一定出报告() {
        assert!(log_alert_due(None, 1));
    }

    #[test]
    fn 冷却期内不出() {
        assert!(!log_alert_due(Some(10), 10));
        assert!(!log_alert_due(Some(10), 10 + LOG_ALERT_COOLDOWN_ROUNDS - 1));
    }

    #[test]
    fn 冷却期满出() {
        assert!(log_alert_due(Some(10), 10 + LOG_ALERT_COOLDOWN_ROUNDS));
    }

    #[test]
    fn 轮次倒退也不_panic_且不误放行() {
        assert!(!log_alert_due(Some(100), 5));
    }
}
