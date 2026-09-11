//! aether-init — AetherOS PID 1。
//!
//! 启动序列（Linux 真机）：
//!   内核 → /init(本程序) → 挂载 /proc /sys /dev → 加载服务定义
//!   → 拓扑排序自启动 → 监督循环（收割/退避重启）+ 服务控制 IPC(7312)
//!
//! 设计：小而美。不做 socket 激活/定时器/cgroup，先做对、做小。

mod ipc;
mod manager;
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
    run_loop("/etc/aether/services")
}

/// 开发自检：任意平台可跑，加载服务定义、启动、监督。
fn run_dry_run(dir: Option<&str>) -> anyhow::Result<()> {
    eprintln!("[aether-init] dry-run 模式（服务目录 {}）", dir.unwrap_or("./services"));
    run_loop(dir.unwrap_or("./services"))
}

fn run_loop(services_dir: &str) -> anyhow::Result<()> {
    let specs = unit::load_dir(&PathBuf::from(services_dir)).unwrap_or_else(|e| {
        eprintln!("[aether-init] 警告: 加载服务定义失败（{e}），以空服务集继续");
        Vec::new()
    });
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
        let events = mgr.lock().expect("aether-init: manager 毒锁").tick();
        for e in events {
            eprintln!("[aether-init] {e}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
