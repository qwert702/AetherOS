//! aether-init — AetherOS PID 1。
//!
//! 启动序列（Linux 真机）：
//!   内核 → /init(本程序) → 挂载 /proc /sys /dev → 加载服务定义
//!   → 拓扑排序自启动 → 监督循环（收割/退避重启）+ 服务控制 IPC(7312)
//!
//! 设计：小而美。不做 socket 激活/定时器/cgroup，先做对、做小。

mod ipc;
mod logtee;
mod manager;
mod persist;
mod unit;

use manager::Manager;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--pid1") => run_pid1(),
        Some("--dry-run") | None => run_dry_run(args.get(2).map(|s| s.as_str())),
        Some(other) => {
            eprintln!("aether-init — AetherOS PID 1\n\n用法:\n  aether-init --pid1            真机 PID 1 模式（仅 Linux）\n  aether-init --dry-run [DIR]   开发自检：加载服务定义并跑监督循环\n\n服务定义目录默认 /etc/aether/services");
            if other != "--help" {
                std::process::exit(2);
            }
            Ok(())
        }
    }
}

/// Linux PID 1：挂载伪文件系统后进入正常运行。
fn run_pid1() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        for (src, dst, fs) in [
            ("proc", "/proc", "proc"),
            ("sysfs", "/sys", "sysfs"),
            ("devtmpfs", "/dev", "devtmpfs"),
        ] {
            if std::fs::metadata(dst).is_err() {
                let _ = std::fs::create_dir_all(dst);
            }
            let ok = std::process::Command::new("/bin/mount")
                .arg("-t")
                .arg(fs)
                .arg(src)
                .arg(dst)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            eprintln!("[aether-init] mount {fs} -> {dst}: {ok}");
        }
    }
    eprintln!("[aether-init] PID 1 启动");
    // 持久化分区在服务之前挂载：logtee 写 /var/log/aether 时已是真磁盘
    #[cfg(target_os = "linux")]
    {
        persist::mount_persist();
    }
    run_loop("/etc/aether/services", true)
}

/// 开发自检：任意平台可跑，加载服务定义、启动、监督。
fn run_dry_run(dir: Option<&str>) -> anyhow::Result<()> {
    eprintln!("[aether-init] dry-run 模式（服务目录 {}）", dir.unwrap_or("./services"));
    run_loop(dir.unwrap_or("./services"), false)
}

fn run_loop(services_dir: &str, pid1: bool) -> anyhow::Result<()> {
    let (specs, warnings) = unit::load_dir(&PathBuf::from(services_dir)).unwrap_or_else(|e| {
        eprintln!("[aether-init] 错误: {e:#}");
        (Vec::new(), Vec::new())
    });
    for w in &warnings {
        eprintln!("[aether-init] 服务定义告警（已跳过）: {w}");
    }
    if specs.is_empty() {
        let msg = format!("服务目录 {services_dir} 无可用服务定义");
        if pid1 {
            // 救援模式：PID 1 不能退出（退出即内核 panic），保持存活等待人工修复
            eprintln!("[aether-init] 致命: {msg} —— 进入救援模式（不启动任何服务与 IPC）");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        anyhow::bail!("{msg}");
    }
    let mut mgr = Manager::new(specs)?;
    let boot = mgr.boot_sequence()?;
    eprintln!("[aether-init] 自启动序列: {boot:?}");
    for name in &boot {
        if let Err(e) = mgr.start(name) {
            eprintln!("[aether-init] 启动 {name} 失败: {e}");
        }
    }

    let mgr = Arc::new(Mutex::new(mgr));
    ipc::spawn(mgr.clone())?;

    // 监督主循环
    loop {
        // 毒锁恢复：持锁线程 panic 不应带崩 PID 1（Mutex 数据仍可用）。
        // 孤儿进程收割在 Manager::tick 内完成（须与受管子进程的收割互斥，
        // 避免 waitpid(-1) 把受管进程"偷收"导致状态失真）。
        let events = mgr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tick();
        for e in events {
            eprintln!("[aether-init] {e}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
