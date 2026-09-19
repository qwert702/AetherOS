//! 持久化分区：把安装器建好的 ext4 分区（卷标 AETHER，通常是 /dev/<盘>2）
//! 在启动时挂到 /var，使日志、诊断报告、审计跨重启保留。
//!
//! 设计：
//! - 候选设备是编译期常量列表（与 aether-install 的分区约定一致）；
//!   逐个尝试挂载，成功即止——光盘启动（无该分区）时全部失败，静默退回内存态。
//! - 挂载在服务启动之前完成，因此 logtee 写 /var/log/aether 时已经是真磁盘。
//! - 每次成功挂载都往 /var/boot.log 追加一条启动记录（持久化的自证）。
//! - 与平台相关仅限 Linux；其余平台为 no-op（可编译、可单测纯逻辑）。

// 挂载路径在非 Linux 平台不被调用，但代码保留为可读文档与单测载体
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// 持久化分区候选：virtio 优先，其次 SATA/IDE（VMware/VBox 常用），最后 nvme。
pub const PERSIST_CANDIDATES: [&str; 4] =
    ["/dev/vda2", "/dev/sda2", "/dev/hda2", "/dev/nvme0n1p2"];

/// /var 下需要存在的目录（挂载覆盖后可能为空，运行时补齐）。
pub const VAR_DIRS: [&str; 3] = ["/var/log/aether", "/var/diag", "/var/tmp"];

/// 启动记录行：`<unix秒> 启动（第 N 次）`。安装行不计入启动次数。
pub fn boot_record_line(ts: u64, boot_index: usize) -> String {
    format!("{ts} 启动（第 {boot_index} 次）\n")
}

/// 统计已有启动记录条数（只数含"启动"的行，安装行等其它内容不计）。
pub fn count_boot_records(text: &str) -> usize {
    text.lines().filter(|l| l.contains("启动")).count()
}

/// 把持久化分区挂到 /var。返回挂载成功的设备名。
#[cfg(target_os = "linux")]
pub fn mount_persist() -> Option<String> {
    for dev in PERSIST_CANDIDATES {
        if !std::path::Path::new(dev).exists() {
            continue;
        }
        let ok = std::process::Command::new("/bin/mount")
            .arg("-t")
            .arg("ext4")
            .arg(dev)
            .arg("/var")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            continue;
        }
        for d in VAR_DIRS {
            let _ = std::fs::create_dir_all(d);
        }
        println!("[aether-init] 持久化分区已挂载: {dev} -> /var");
        append_boot_record();
        return Some(dev.to_string());
    }
    println!("[aether-init] 未发现持久化分区（内存态运行，重启后状态不保留）");
    None
}

/// 追加一条启动记录到 /var/boot.log（失败只忽略：记录是附加价值，不阻塞启动）。
#[cfg(target_os = "linux")]
fn append_boot_record() {
    use std::io::Write;
    let path = "/var/boot.log";
    let count = std::fs::read_to_string(path)
        .map(|s| count_boot_records(&s) + 1)
        .unwrap_or(1);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(boot_record_line(ts, count).as_bytes());
    }
}

#[cfg(not(target_os = "linux"))]
pub fn mount_persist() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_partition_two_of_common_disks() {
        // 与 aether-install 的约定一致：持久化分区号固定为 2
        for c in PERSIST_CANDIDATES {
            assert!(c.starts_with("/dev/"));
            assert!(
                c.ends_with('2') || c.ends_with("p2"),
                "{c} 应指向第 2 个分区"
            );
        }
        assert_eq!(PERSIST_CANDIDATES[0], "/dev/vda2", "virtio 应优先");
    }

    #[test]
    fn boot_record_is_appended_style() {
        let line = boot_record_line(1_700_000_000, 3);
        assert!(line.ends_with('\n'), "记录行必须以换行结尾（追加友好）");
        assert!(line.contains("1700000000"));
        assert!(line.contains("第 3 次"));
    }

    #[test]
    fn boot_counter_ignores_install_line() {
        // 安装器写的初始内容不含"启动"，首启应记为第 1 次
        let installed = "1700000000 安装完成（分区 /dev/vda2）\n";
        assert_eq!(count_boot_records(installed), 0);
        let after_first = format!("{installed}1700000001 启动（第 1 次）\n");
        assert_eq!(count_boot_records(&after_first), 1);
        // 重启后计数递增
        let next = count_boot_records(&after_first) + 1;
        assert_eq!(next, 2);
    }

    #[test]
    fn var_dirs_cover_logs_and_diag() {
        assert!(VAR_DIRS.contains(&"/var/log/aether"));
        assert!(VAR_DIRS.contains(&"/var/diag"));
    }
}
