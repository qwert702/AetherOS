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
        Some("--rescue") => rescue_console("手动进入救援模式（--rescue）"),
        Some(other) => {
            eprintln!("aether-init — AetherOS PID 1\n\n用法:\n  aether-init --pid1            真机 PID 1 模式（仅 Linux）\n  aether-init --dry-run [DIR]   开发自检：加载服务定义并跑监督循环
  aether-init --rescue          手动进入救援模式（打印原因 + 控制台 shell）\n\n服务定义目录默认 /etc/aether/services");
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

        // devpts：**PTY 的硬前置**。
        //
        // 没有它 `posix_openpt` 无法创建 slave 端（/dev/pts/N 不会出现），
        // 于是"真实终端"这条路根本走不通 —— 这也是此前终端只能画死数据的原因之一。
        // ptmxmode=0666 让非 root 也能打开 /dev/ptmx；gid=5(tty) + mode=620 是发行版惯例。
        let pts = "/dev/pts";
        let ok = std::fs::create_dir_all(pts).map(|_| ()).map_err(|e| e.to_string());
        if let Err(e) = ok {
            eprintln!("[aether-init] 警告：创建 {pts} 失败：{e}（PTY 将不可用）");
        }
        // 已经挂好就不要再挂：重复挂载会返回 EBUSY，那会打出一条**假告警**，
        // 让"终端能用"这件事在日志里看起来像坏了
        if std::path::Path::new("/dev/pts/ptmx").exists() {
            eprintln!("[aether-init] devpts 已就绪（/dev/pts/ptmx 存在）");
        } else {
            let mounted = std::process::Command::new("/bin/mount")
                .args(["-t", "devpts", "devpts", pts, "-o", "gid=5,mode=620,ptmxmode=0666"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if mounted {
                eprintln!("[aether-init] mount devpts -> {pts}: true");
            } else {
                // 显式告警：静默失败会让"终端打不开"变成一个查不出原因的现象
                eprintln!("[aether-init] 警告：mount devpts -> {pts} 失败，PTY（真实终端）将不可用");
            }
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

/// 救援模式：打印**可操作的**原因与出路，并给一个控制台 shell。永不返回。
///
/// 之前的实现是 `sleep(3600)` 死等 —— 那与"黑屏"几乎没有区别：用户既看不到原因，
/// 也没有任何修复手段。救援模式的价值全在"给人一条出路"。
///
/// PID 1 不允许退出（退出即内核 panic），所以这里是个永不结束的循环：
/// shell 退出后重新打印并再来一次。
fn rescue_console(reason: &str) -> ! {
    loop {
        eprintln!("======================================================");
        eprintln!("[aether-init] 救援模式（rescue mode）");
        eprintln!("  原因：{reason}");
        eprintln!("  影响：没有启动任何服务 —— 桌面（compositor）与 AI 中枢（aetherd）都不会起来");
        eprintln!("  排查：看 /etc/aether/services/*.json");
        eprintln!("        · 单个文件写坏只会被跳过（上面有告警），不至于整机不可用");
        eprintln!("        · 全部不可用才会进到这里（目录不存在 / 权限 / 全部解析失败）");
        eprintln!("  修复：改好配置后执行 reboot，或用下面这个 shell 现场处理");
        eprintln!("  PID 1 不会退出；此 shell 退出后会重新进入救援模式");
        eprintln!("======================================================");
        #[cfg(target_os = "linux")]
        {
            // 控制台 shell：stdin/out/err 都指向 /dev/console（串口/本机终端）
            // 开三个独立句柄分别给 stdin/stdout/stderr（/dev/console 是设备文件，多次打开没问题；
            // 用 clone 链容易把同一个 File 移动两次，那是编译器会直接拒绝的写法）
            match std::fs::OpenOptions::new().read(true).write(true).open("/dev/console") {
                Ok(con_in) => {
                    let out = std::fs::OpenOptions::new().write(true).open("/dev/console");
                    let err = std::fs::OpenOptions::new().write(true).open("/dev/console");
                    match (out, err) {
                        (Ok(o), Ok(e)) => {
                            let mut cmd = std::process::Command::new("/bin/sh");
                            cmd.stdin(std::process::Stdio::from(con_in))
                                .stdout(std::process::Stdio::from(o))
                                .stderr(std::process::Stdio::from(e));
                            match cmd.status() {
                                Ok(st) => eprintln!("[aether-init] 救援 shell 退出：{st}"),
                                Err(e) => eprintln!("[aether-init] 无法启动救援 shell：{e}"),
                            }
                        }
                        _ => eprintln!("[aether-init] /dev/console 打开不全：仅保持存活，等待人工处理"),
                    }
                }
                Err(e) => eprintln!("[aether-init] 打不开 /dev/console（{e}）：仅保持存活，等待人工处理"),
            }
        }
        #[cfg(not(target_os = "linux"))]
        eprintln!("[aether-init] 非 Linux 平台：不启动救援 shell，仅保持存活");

        std::thread::sleep(Duration::from_secs(3));
    }
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
            // PID 1 不能退出（退出即内核 panic）→ 进救援模式
            eprintln!("[aether-init] 致命: {msg}");
            rescue_console(&msg);
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
