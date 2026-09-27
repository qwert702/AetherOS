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

/// 单服务日志上限：超过即轮转为 .log.1（保留一份旧档），
/// 防止长期运行把持久化分区撑满（P3-5）。
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// 轮转：`<unit>.log` → `<unit>.log.1`（旧档被覆盖，只保留一份）。返回是否发生了轮转。
///
/// 抽成独立函数并带 `dir` 参数，是为了能在开发机上用临时目录单测 ——
/// 轮转是"长期运行才暴露"的逻辑，靠人工跑几周去验证不现实。
fn rotate_in(dir: &str, unit: &str) -> bool {
    let cur = format!("{dir}/{unit}.log");
    let old = format!("{dir}/{unit}.log.1");
    if !std::path::Path::new(&cur).exists() {
        return false;
    }
    let _ = std::fs::remove_file(&old); // 旧档只留一份
    std::fs::rename(&cur, &old).is_ok()
}

/// 打开（必要时先轮转）服务日志文件。全程容错：打不开只丢文件侧。
fn open_log(unit: &str) -> Option<std::fs::File> {
    let path = format!("{LOG_DIR}/{unit}.log");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_LOG_BYTES {
            rotate_in(LOG_DIR, unit);
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

/// 接管 child 的 stdout/stderr（必须在 spawn 后、wait 前调用）。
pub fn tee_child(unit: &str, child: &mut Child) {
    let _ = std::fs::create_dir_all(LOG_DIR);
    let out = child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    let err = child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>);
    for pipe in [out, err] {
        let Some(mut pipe) = pipe else { continue };
        let log = open_log(unit);
        let console = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/console")
            .ok();
        let unit = unit.to_string();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut log = log;
            let mut console = console;
            let mut written: u64 = 0;
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Some(f) = log.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                            written += n as u64;
                        }
                        // **运行期也要轮转**：只在打开时检查的话，一个连续运行数周的
                        // 服务会一路写下去，8MB 上限根本不会生效。
                        if written >= MAX_LOG_BYTES {
                            // 先显式关闭句柄再改名：某些系统上重命名"打开中"的文件会失败
                            drop(log.take());
                            rotate_in(LOG_DIR, &unit);
                            log = open_log(&unit);
                            written = 0;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> String {
        let d = std::env::temp_dir().join(format!("aether_logtee_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        d.to_string_lossy().to_string()
    }

    #[test]
    fn rotate_moves_current_to_archive() {
        let dir = tmpdir("rot1");
        std::fs::write(format!("{dir}/aetherd.log"), b"hello").unwrap();
        assert!(rotate_in(&dir, "aetherd"));
        assert!(!std::path::Path::new(&format!("{dir}/aetherd.log")).exists());
        let archived = std::fs::read(format!("{dir}/aetherd.log.1")).unwrap();
        assert_eq!(archived, b"hello");
    }

    #[test]
    fn rotate_keeps_only_one_archive() {
        let dir = tmpdir("rot2");
        std::fs::write(format!("{dir}/x.log.1"), b"old-archive").unwrap();
        std::fs::write(format!("{dir}/x.log"), b"new").unwrap();
        assert!(rotate_in(&dir, "x"));
        // 旧档被覆盖，只保留最新的一份
        assert_eq!(std::fs::read(format!("{dir}/x.log.1")).unwrap(), b"new");
    }

    #[test]
    fn rotate_missing_file_is_noop() {
        let dir = tmpdir("rot3");
        assert!(!rotate_in(&dir, "nope"), "没有日志时不应报告轮转成功");
    }
}

