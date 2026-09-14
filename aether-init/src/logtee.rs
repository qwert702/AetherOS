//! 服务日志管道：子进程 stdout/stderr → /var/log/aether/<unit>.log（追加），
//! 并 tee 一份到控制台（/dev/console，即串口），保证既有诊断通路不回退。
//!
//! /var 在有持久化分区时是真磁盘（见 persist.rs），日志因此跨重启保留；
//! 光盘启动时为内存态，行为不变只是不保留。
//! 每路输出（stdout/stderr）一个读线程：读管道 → 写文件 + 写控制台。
//! 子进程退出后管道 EOF，线程自然结束。
//! 全程容错：文件打不开（如 Windows dry-run）只丢文件侧，不影响服务运行。

use std::io::{Read, Write};
use std::process::Child;

pub const LOG_DIR: &str = "/var/log/aether";

/// 接管 child 的 stdout/stderr（必须在 spawn 后、wait 前调用）。
pub fn tee_child(unit: &str, child: &mut Child) {
    let _ = std::fs::create_dir_all(LOG_DIR);
    let out = child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    let err = child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    for pipe in [out, err] {
        let Some(mut pipe) = pipe else { continue };
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{LOG_DIR}/{unit}.log"))
            .ok();
        let console = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/console")
            .ok();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut log = log;
            let mut console = console;
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Some(f) = log.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
                        if let Some(c) = console.as_mut() {
                            let _ = c.write_all(&buf[..n]);
                        }
                    }
                }
            }
        });
    }
}
