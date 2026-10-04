//! 可移动存储的检测与只读挂载（FEATURE-GAPS §3.7 的最小切片）。
//!
//! ## 范围
//!
//! 只做**可移动存储**：识别（读 `/sys/block`）→ 只读挂载 → 卸载。
//! **蓝牙与打印不在范围内** —— 那需要各自完整的协议栈（BlueZ / CUPS 或等价实现），
//! 属于"接一整套子系统"，不是"补一个功能"。这条边界写在文档里，避免被读成"支持蓝牙"。
//!
//! ## 两条安全决定
//!
//! 1. **默认只读**（[`MOUNT_OPTS`]）：U 盘是不受信任的介质。只读 + `nodev` + `nosuid`
//!    + `noexec` 合起来的意思是"能看里面的文件，但它带进来的东西在这个系统里
//!    拿不到任何立足点"；
//! 2. **只允许挂载扫描到的设备**：设备名会被拼进路径，所以除了语法校验，
//!    服务端还要求它**出现在本次扫描结果里**（见 `is_scanned`）。

use std::path::{Path, PathBuf};

/// 挂载选项。**默认只读** —— 理由见模块说明。
///
/// 为什么不给 `rw` 开关：那需要一个"我知道我在做什么"的确认流程，
/// 而本批只做最小切片 —— **不提供比"只读"更强的能力，比提供一个半吊子的写开关诚实**。
pub const MOUNT_OPTS: &str = "ro,nodev,nosuid,noexec";

/// 挂载点根目录。
pub const MEDIA_ROOT: &str = "/media/aether";

/// 一个可移动设备（或它的分区）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Removable {
    /// 设备节点，例如 `/dev/sdb1`。
    pub dev: String,
    /// 属于哪块盘（同一块盘的分区排在一起），例如 `/dev/sdb`。
    pub disk: String,
    /// 大小（字节）。
    pub bytes: u64,
    /// 已经挂在哪里（没挂载则为 `None`）。
    pub mounted_at: Option<String>,
}

/// 设备名是否**语法上**像块设备（`/dev/` + 受限字符集）。
///
/// 这只是第一道；真正决定"能不能挂"的是"它出现在扫描结果里"（见 [`is_scanned`]）。
pub fn valid_dev(dev: &str) -> bool {
    let Some(rest) = dev.strip_prefix("/dev/") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 32
        && rest.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// 设备名 → 挂载点目录名（去掉 `/dev/`，其余字符已在 [`valid_dev`] 里限死）。
pub fn mount_name(dev: &str) -> String {
    dev.trim_start_matches("/dev/").to_string()
}

/// 扫描可移动块设备（`sys_block` 是 `/sys/block`，测试可注入临时目录）。
///
/// 规则：
/// * 盘本身必须有 `removable` = 1；
/// * 有分区就**列分区**（挂整盘通常是错的）；
/// * 没有分区（U 盘直接写文件系统，很常见）就把盘本身列为候选；
/// * `size` 为 0 的一律跳过（没插介质的光驱/读卡器就是这种）。
pub fn scan(sys_block: &Path) -> Vec<Removable> {
    let Ok(rd) = std::fs::read_dir(sys_block) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut disks: Vec<String> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // loop 设备不是可移动介质；它只是回环挂载
        if name.starts_with("loop") {
            continue;
        }
        let dir = e.path();
        if read_trim(&dir.join("removable")).as_deref() != Some("1") {
            continue;
        }
        disks.push(name);
    }
    disks.sort();
    for disk in &disks {
        let disk_dir = sys_block.join(disk);
        // 分区：与盘同名前缀 + 数字（sdb1、nvme0n1p1 都符合）
        let mut parts: Vec<(String, u64)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&disk_dir) {
            for e in rd.flatten() {
                let pname = e.file_name().to_string_lossy().to_string();
                if !pname.starts_with(disk.as_str()) {
                    continue;
                }
                let tail = &pname[disk.len()..];
                if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let bytes = sectors_to_bytes(&read_trim(&e.path().join("size")));
                if bytes > 0 {
                    parts.push((pname, bytes));
                }
            }
        }
        parts.sort();
        if parts.is_empty() {
            let bytes = sectors_to_bytes(&read_trim(&disk_dir.join("size")));
            if bytes > 0 {
                out.push(Removable {
                    dev: format!("/dev/{disk}"),
                    disk: format!("/dev/{disk}"),
                    bytes,
                    mounted_at: None,
                });
            }
        } else {
            for (pname, bytes) in parts {
                out.push(Removable {
                    dev: format!("/dev/{pname}"),
                    disk: format!("/dev/{disk}"),
                    bytes,
                    mounted_at: None,
                });
            }
        }
    }
    out
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// sysfs 的 `size` 是 **512 字节扇区数**。
pub fn sectors_to_bytes(s: &Option<String>) -> u64 {
    s.as_deref()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|n| n.saturating_mul(512))
        .unwrap_or(0)
}

/// 某个设备是不是**本次扫描到的**（这决定了"能不能挂"，比语法校验更强）。
pub fn is_scanned(dev: &str, scanned: &[Removable]) -> bool {
    valid_dev(dev) && scanned.iter().any(|r| r.dev == dev)
}

/// 该设备的挂载点。
pub fn mount_point(dev: &str) -> PathBuf {
    PathBuf::from(MEDIA_ROOT).join(mount_name(dev))
}

/// 挂载命令的参数（不含程序名）。抽出来是为了**能测**：
/// 挂载参数写错是"能把系统搞坏"的那类错误。
pub fn mount_args(dev: &str, mountpoint: &Path) -> Vec<String> {
    vec![
        "-o".to_string(),
        MOUNT_OPTS.to_string(),
        dev.to_string(),
        mountpoint.to_string_lossy().to_string(),
    ]
}

/// 从 `/proc/mounts` 的内容里找出某个设备的挂载点（**纯函数**，可测）。
///
/// `/proc/mounts` 的格式：`设备 挂载点 类型 选项 0 0`。
/// 空格与制表符都可能作分隔；挂载点里的空格被转义成 `\040`（内核行为）——
/// 本系统的挂载点是我们自己生成的（`/media/aether/<dev>`），不含空格，
/// 所以这里**不做反转义**，但要如实说明这一点。
pub fn mountpoint_of(dev: &str, mounts: &str) -> Option<String> {
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(d), Some(mp)) = (f.next(), f.next()) else {
            continue;
        };
        if d == dev {
            return Some(mp.to_string());
        }
    }
    None
}

/// 把"已挂载在哪"填进扫描结果（`/proc/mounts` 读不到时保持 `None`）。
pub fn with_mounts(mut list: Vec<Removable>, mounts: &str) -> Vec<Removable> {
    for r in &mut list {
        r.mounted_at = mountpoint_of(&r.dev, mounts);
    }
    list
}

/// 挂载一个**已扫描到**的设备（只读）。返回挂载点。
pub fn mount(dev: &str, scanned: &[Removable]) -> Result<String, String> {
    use std::process::Command;
    // **核心安全规则**：只有本次扫描到的设备能被挂载
    if !is_scanned(dev, scanned) {
        return Err(format!(
            "拒绝挂载 {dev}：它不在当前扫描到的可移动设备里（语法合法不代表它真是可移动介质）"
        ));
    }
    if let Some(r) = scanned.iter().find(|r| r.dev == dev) {
        if let Some(mp) = &r.mounted_at {
            return Err(format!("{dev} 已经挂在 {mp} 了"));
        }
    }
    let mp = mount_point(dev);
    std::fs::create_dir_all(&mp).map_err(|e| format!("建挂载点失败 {}：{e}", mp.display()))?;
    let args = mount_args(dev, &mp);
    let out = Command::new("/bin/mount")
        .args(&args)
        .output()
        .map_err(|e| format!("跑 mount 失败：{e}"))?;
    if !out.status.success() {
        // 失败时把挂载点也清掉，免得留下一堆空目录让人以为"挂上了"
        let _ = std::fs::remove_dir(&mp);
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("挂载 {dev} 失败：{why}"));
    }
    Ok(mp.to_string_lossy().to_string())
}

/// 卸载一个**由我们挂载的**设备。参数是设备名（不是任意路径）。
pub fn umount(dev: &str, scanned: &[Removable]) -> Result<String, String> {
    use std::process::Command;
    if !valid_dev(dev) {
        return Err(format!("设备名不合法：{dev:?}"));
    }
    // 卸载点由设备名重算 —— **不接受界面给的任意路径**
    let mp = mount_point(dev);
    let Some(r) = scanned.iter().find(|r| r.dev == dev) else {
        return Err(format!("{dev} 不在当前扫描到的可移动设备里"));
    };
    let Some(mounted) = &r.mounted_at else {
        return Err(format!("{dev} 没有挂载"));
    };
    // 只卸我们自己挂的位置：别人挂的（例如系统启动时挂的）不碰
    if Path::new(mounted) != mp {
        return Err(format!(
            "{dev} 挂在 {mounted}，不是本系统挂的（{}）—— 拒绝卸载",
            mp.display()
        ));
    }
    let out = Command::new("/bin/umount")
        .arg(&mp)
        .output()
        .map_err(|e| format!("跑 umount 失败：{e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("卸载 {dev} 失败：{why}"));
    }
    let _ = std::fs::remove_dir(&mp);
    Ok(format!("已卸载 {dev}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-removable-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 建一块盘（在 `/sys/block/<name>/` 下）。
    fn dev(sys: &Path, name: &str, removable: &str, size: &str) {
        let d = sys.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("removable"), removable).unwrap();
        std::fs::write(d.join("size"), size).unwrap();
    }

    /// 建一个分区 —— **必须嵌在磁盘目录内**（真实 sysfs 是 `/sys/block/sdb/sdb1/`，
    /// 不是和盘平级）。第一版夹具把它们建成同级，于是分区一个都没被扫到：
    /// 代码是对的，夹具错了（测试的价值正在这里）。
    fn part(sys: &Path, disk: &str, part: &str, size: &str) {
        let d = sys.join(disk).join(part);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("size"), size).unwrap();
    }

    /// 扫描：只收可移动、有介质、有分区的盘；固定盘与空读卡器不出现。
    #[test]
    fn scan_finds_removable_media_and_ignores_everything_else() {
        let root = tmp("scan");
        let sys = root.join("block");
        // 一块 U 盘：两个分区（分区在盘目录**内**）
        dev(&sys, "sdb", "1", "30253056");
        part(&sys, "sdb", "sdb1", "1024000");
        part(&sys, "sdb", "sdb2", "2024000");
        // 固定盘（不该出现）
        dev(&sys, "sda", "0", "500000000");
        part(&sys, "sda", "sda1", "499000000");
        // 空读卡器：removable=1 但 size=0（没插卡）
        dev(&sys, "mmcblk0", "1", "0");
        // 回环设备（不是可移动介质）
        dev(&sys, "loop0", "1", "1000");
        let out = scan(&sys);
        let devs: Vec<&str> = out.iter().map(|r| r.dev.as_str()).collect();
        assert_eq!(devs, vec!["/dev/sdb1", "/dev/sdb2"], "实得 {devs:?}");
        assert!(out.iter().all(|r| r.disk == "/dev/sdb"));
        // size 是 512 字节扇区
        assert_eq!(out[0].bytes, 1024000 * 512);
        assert!(out.iter().all(|r| r.mounted_at.is_none()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 整盘直接写文件系统（没有分区表）也要能列出来 —— 这种 U 盘很常见。
    #[test]
    fn a_raw_disk_without_partitions_is_listed_too() {
        let root = tmp("raw");
        let sys = root.join("block");
        dev(&sys, "sdc", "1", "2048000");
        let out = scan(&sys);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].dev, "/dev/sdc");
        assert_eq!(out[0].bytes, 2048000 * 512);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scanning_a_missing_directory_is_empty_not_an_error() {
        assert!(scan(Path::new("/sys/block-does-not-exist")).is_empty());
    }

    /// 语法校验：挡住路径拼接与奇怪字符。
    #[test]
    fn device_names_are_validated() {
        for ok in ["/dev/sdb1", "/dev/sda", "/dev/nvme0n1p1", "/dev/vdb2"] {
            assert!(valid_dev(ok), "{ok} 应当合法");
        }
        for bad in [
            "",
            "sdb1",
            "/dev/",
            "/dev/../etc/shadow",
            "/dev/sdb1/../../x",
            "/dev/SDB1",   // 大写不出现于内核命名
            "/dev/sd b1",  // 空格
            "/dev/sdb1\n",
            "//dev/sdb1",
            "/etc/passwd",
        ] {
            assert!(!valid_dev(bad), "{bad:?} 不该合法");
        }
    }

    /// **核心安全规则**：只有本次扫描到的设备能被挂载 —— 语法合法但不存在/不是可移动的，一律拒绝。
    #[test]
    fn only_scanned_devices_may_be_mounted() {
        let root = tmp("scanned");
        let sys = root.join("block");
        dev(&sys, "sdb", "1", "1024000");
        part(&sys, "sdb", "sdb1", "1023000");
        let scanned = scan(&sys);
        assert!(is_scanned("/dev/sdb1", &scanned));
        // 语法合法但不在扫描结果里（例如固定盘、或攻击者编的名字）
        assert!(!is_scanned("/dev/sda1", &scanned));
        assert!(!is_scanned("/dev/sdz9", &scanned));
        // 语法不合法
        assert!(!is_scanned("/dev/../etc/shadow", &scanned));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 挂载参数必须带齐四条约束 —— 少一条就是"U 盘能在系统里获得立足点"。
    #[test]
    fn mount_options_are_read_only_and_hardened() {
        for opt in ["ro", "nodev", "nosuid", "noexec"] {
            assert!(
                MOUNT_OPTS.split(',').any(|o| o == opt),
                "挂载选项缺少 {opt}：{MOUNT_OPTS}"
            );
        }
        assert!(!MOUNT_OPTS.contains("rw"), "默认不许可写：{MOUNT_OPTS}");
        // 参数拼法
        let mp = mount_point("/dev/sdb1");
        let args = mount_args("/dev/sdb1", &mp);
        assert_eq!(args[0], "-o");
        assert_eq!(args[1], MOUNT_OPTS);
        assert_eq!(args[2], "/dev/sdb1");
        assert_eq!(args[3], mp.to_string_lossy());
    }

    #[test]
    fn mount_point_is_derived_from_the_device_name() {
        // 平台无关的比较：Windows 上 `Path::join` 用 `\`，直接比字符串会假红
        assert_eq!(mount_point("/dev/sdb1"), PathBuf::from(MEDIA_ROOT).join("sdb1"));
        assert_eq!(mount_point("/dev/nvme0n1p1"), PathBuf::from(MEDIA_ROOT).join("nvme0n1p1"));
        assert_eq!(mount_name("/dev/sdb1"), "sdb1");
    }
    /// `/proc/mounts` 解析：找得到、找不到、格式不对的行都覆盖。
    #[test]
    fn mountpoint_lookup_parses_proc_mounts() {
        let mounts = "\n".to_string()
            + "/dev/sdb1 /media/aether/sdb1 ext4 ro,nodev,nosuid,noexec 0 0\n"
            + "/dev/sda1 / ext4 rw,relatime 0 0\n"
            + "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"
            + "坏行\n";
        assert_eq!(
            mountpoint_of("/dev/sdb1", &mounts).as_deref(),
            Some("/media/aether/sdb1")
        );
        assert_eq!(mountpoint_of("/dev/sda1", &mounts).as_deref(), Some("/"));
        assert_eq!(mountpoint_of("/dev/sdz9", &mounts), None);
        // 全部填进去
        let list = vec![
            Removable {
                dev: "/dev/sdb1".into(),
                disk: "/dev/sdb".into(),
                bytes: 1,
                mounted_at: None,
            },
            Removable {
                dev: "/dev/sdb2".into(),
                disk: "/dev/sdb".into(),
                bytes: 1,
                mounted_at: None,
            },
        ];
        let filled = with_mounts(list, &mounts);
        assert_eq!(filled[0].mounted_at.as_deref(), Some("/media/aether/sdb1"));
        assert_eq!(filled[1].mounted_at, None, "没挂的分区应当是 None");
    }

    /// 挂载/卸载只认"扫描到的"设备 —— 这是本模块的核心安全规则。
    #[test]
    fn mount_and_umount_refuse_devices_that_were_not_scanned() {
        let scanned: Vec<Removable> = Vec::new();
        assert!(mount("/dev/sdb1", &scanned).is_err());
        assert!(mount("/dev/../etc/shadow", &scanned).is_err());
        assert!(umount("/dev/sdb1", &scanned).is_err());
        assert!(umount("/dev/../etc/shadow", &scanned).is_err());
        // 已挂载的再挂一次要报"已经挂了"（而不是重复挂）
        let mounted = vec![Removable {
            dev: "/dev/sdb1".into(),
            disk: "/dev/sdb".into(),
            bytes: 1,
            mounted_at: Some("/media/aether/sdb1".into()),
        }];
        let err = mount("/dev/sdb1", &mounted).unwrap_err();
        assert!(err.contains("已经挂"), "{err}");
        // 别人挂的位置不卸（只卸我们自己挂的）
        let elsewhere = vec![Removable {
            dev: "/dev/sdb1".into(),
            disk: "/dev/sdb".into(),
            bytes: 1,
            mounted_at: Some("/mnt/other".into()),
        }];
        let err2 = umount("/dev/sdb1", &elsewhere).unwrap_err();
        assert!(err2.contains("不是本系统挂的"), "{err2}");
    }

}
