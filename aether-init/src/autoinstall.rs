//! 引导期**无人安装**（live 介质专用）—— 让"挂上 ISO 一开机就装完"成立。
//!
//! ## 触发条件必须**同时**满足两条
//!
//! 1. 内核命令行里有 `aether.autoinstall=1`（只有引导菜单里那个"自动安装"项带它）；
//! 2. **根是只读的 live 介质**（iso9660/squashfs/…）。
//!
//! 只看第 1 条会出**毁盘级**事故：系统分区是 ISO 的拷贝，连引导配置一起被装进磁盘，
//! 于是**从磁盘引导时命令行照样带这个参数** —— 它会把刚装好的系统**再装一遍**。
//! 加上第 2 条之后，从盘引导（ext4、rw）永远不会触发。这是本模块最要紧的一条判据。
//!
//! ## 目标盘怎么选
//!
//! 无人值守时没人给 `--disk`。规则：跳过光驱/软驱/回环/zram/device-mapper 后，
//! 取容量最大的那块整盘 —— VMware 与 VirtualBox 的装机盘就是唯一的固定盘。
//!
//! ## 退出方式
//!
//! 装完打一行结果标记（宿主脚本与串口判据都按它取证）→ `sync` → **关机**。
//! 关机而不是重启：让宿主有机会先改引导顺序（从光驱改磁盘），再开机引导新系统。

/// 触发开关。**逐 token 精确匹配** —— `contains` 会让 `aether.autoinstall=10` 假阳。
pub const FLAG: &str = "aether.autoinstall=1";

/// 结果标记：宿主脚本与串口判据按这两行取证据（不要改字面量，手册里引用了它们）。
pub const RESULT_PASS: &str = "AETHER-AUTOINSTALL: PASS";
pub const RESULT_FAIL: &str = "AETHER-AUTOINSTALL: FAIL";

/// 安装器候选路径。镜像里装在 `/usr/bin`；留候选是为了不让"路径变了"变成静默失败。
///
/// 只在 Linux 上用到（本模块的检测与执行都是 Linux 专属），所以这里也加门 ——
/// 否则 Windows 构建会因 dead_code 告警（项目标准是 0 告警）。
#[cfg(target_os = "linux")]
pub const INSTALLER_CANDIDATES: [&str; 3] = [
    "/usr/bin/aether-install",
    "/sbin/aether-install",
    "/bin/aether-install",
];

/// 根文件系统是这些类型 ⇒ 认为"当前跑在 live 介质上"。
pub const LIVE_FSTYPES: [&str; 6] = ["iso9660", "squashfs", "erofs", "udf", "cramfs", "romfs"];

/// 命令行里是否有开关（逐 token 比较）。
pub fn flag_present(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|t| t == FLAG)
}

/// `/proc/mounts` 的根那一行是否为"**只读** + live 文件系统"。
///
/// 非 Linux 上永远返回 false（本模块的核心路径也是 `cfg(target_os = "linux")`）。
pub fn root_is_readonly_live(mounts: &str) -> bool {
    mounts
        .lines()
        .find_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            // proc/mounts: device mountpoint fstype options …
            (f.len() >= 4 && f[1] == "/").then(|| (f[2], f[3]))
        })
        .is_some_and(|(fstype, opts)| {
            LIVE_FSTYPES.contains(&fstype) && opts.split(',').any(|o| o == "ro")
        })
}

/// 从 `/proc/partitions` 里挑目标盘：跳过可移动/虚拟设备，取最大的一块**整盘**。
///
/// `/proc/partitions` 每行形如 `   8        0   41943040 sda`（主设备号 次设备号 块数 名字）。
pub fn pick_target_disk(partitions: &str) -> Option<String> {
    /// 这些前缀不是装机目标：光驱、回环、内存盘、软驱、压缩内存盘、device-mapper。
    const SKIP: [&str; 6] = ["sr", "loop", "ram", "fd", "zram", "dm-"];
    partitions
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                return None;
            }
            let blocks: u64 = f[2].parse().ok()?;
            let name = f[3];
            // 只要整盘：分区名以数字结尾（sda1、nvme0n1p2…）的一律跳过
            if name.chars().next_back()?.is_ascii_digit() {
                return None;
            }
            if SKIP.iter().any(|p| name.starts_with(p)) {
                return None;
            }
            Some((blocks, name.to_string()))
        })
        .max_by_key(|(blocks, _)| *blocks)
        .map(|(_, name)| format!("/dev/{name}"))
}

/// 读 `/proc/cmdline` 与 `/proc/mounts`，判断这次启动是否该自动装。
#[cfg(target_os = "linux")]
pub fn requested() -> bool {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if !flag_present(&cmdline) {
        return false;
    }
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    root_is_readonly_live(&mounts)
}

#[cfg(not(target_os = "linux"))]
pub fn requested() -> bool {
    false
}

/// 找一个真实存在的安装器（按候选顺序）。找不到返回 `None` —— 调用方**降级为普通 live 启动**。
#[cfg(target_os = "linux")]
pub fn installer_path() -> Option<&'static str> {
    use std::path::Path;
    INSTALLER_CANDIDATES
        .iter()
        .find(|p| Path::new(p).is_file())
        .copied()
}

/// 装机并**关机**（不返回）。只在"该装 + 装得了"时调用。
///
/// 为什么关机而不是重启：让宿主有机会先改引导顺序（光驱 → 磁盘）再开机，
/// 否则会又从 ISO 引导一次（虽然第 2 条判据挡住了重装，但那样就白装一遍）。
#[cfg(target_os = "linux")]
pub fn run(installer: &str, disk: &str) -> ! {
    use crate::power::sync_and_reboot;
    // PowerAction 定义在 aether_ipc 里（不是 power）—— 从 power 导会报 E0603 私有访问
    use aether_ipc::PowerAction;

    eprintln!("[aether-init] 自动安装开始：{installer} --disk {disk} --yes");
    let status = std::process::Command::new(installer)
        .args(["--disk", disk, "--yes"])
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("[aether-init] 自动安装完成（{disk}）");
            eprintln!("{RESULT_PASS}");
            eprintln!("[aether-init] 关机 —— 等宿主把引导顺序改成磁盘后再开机");
        }
        other => {
            eprintln!("[aether-init] 自动安装失败：{other:?}");
            eprintln!("{RESULT_FAIL}");
        }
    }
    // 无论成功失败都 sync + 关机：留着 live 系统在那儿跑没有意义，而且失败时
    // 继续跑服务会让"装了一半的盘"被误挂（mount_persist 会去认那个半成品分区）。
    let _ = sync_and_reboot(PowerAction::PowerOff);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_matches_exact_token_only() {
        assert!(flag_present("console=ttyS0 aether.autoinstall=1 quiet"));
        assert!(flag_present("aether.autoinstall=1"));
        // 前缀像但值不同：**不能**假阳（否则"手动装到一半"会被误判）
        assert!(!flag_present("aether.autoinstall=10"));
        assert!(!flag_present("aether.autoinstall=1x"));
        assert!(!flag_present("xaether.autoinstall=1"));
        assert!(!flag_present(""));
    }

    #[test]
    fn live_root_detected_only_when_readonly_iso() {
        // 真·live：iso9660 且 ro
        assert!(root_is_readonly_live(
            "/dev/sr0 / iso9660 ro,relatime 0 0\n/dev/sda2 /var ext4 rw 0 0\n"
        ));
        // 装完从**磁盘**引导：ext4 rw ⇒ 绝不能再触发（这是毁盘级判据）
        assert!(!root_is_readonly_live("/dev/sda1 / ext4 rw,relatime 0 0\n"));
        // ISO 类型但被挂成 rw（异常场景）：也不触发 —— 只读才是 live 的判据
        assert!(!root_is_readonly_live("/dev/sr0 / iso9660 rw 0 0\n"));
        // squashfs ro 也算 live
        assert!(root_is_readonly_live("none / squashfs ro 0 0\n"));
        // 没有根那一行：不触发
        assert!(!root_is_readonly_live("/dev/sda2 /var ext4 rw 0 0\n"));
        assert!(!root_is_readonly_live(""));
    }

    #[test]
    fn picks_largest_fixed_disk_and_skips_removable() {
        let sample = "\
major minor  #blocks  name

   8        0   8388608 sda
   8        1      1024 sda1
   8        2   41939968 sda2
  11        0      41984 sr0
   7        0     102400 loop0
   1        0       4096 ram0
 253        0   10485760 dm-0
";
        // sda2 是分区（以数字结尾）→ 跳过；sr0/loop0/ram0/dm-0 → 跳过；剩下 sda 最大
        assert_eq!(pick_target_disk(sample), Some("/dev/sda".to_string()));
    }

    #[test]
    fn picks_nothing_when_only_removable_present() {
        let only_cd = "  11        0      41984 sr0\n   7        0     102400 loop0\n";
        assert_eq!(pick_target_disk(only_cd), None, "只有光驱时不该猜一块盘出来");
        assert_eq!(pick_target_disk(""), None);
    }

    #[test]
    fn picks_the_bigger_of_two_disks() {
        let two = "   8        0    1048576 sda\n   8       16   41943040 sdb\n";
        assert_eq!(pick_target_disk(two), Some("/dev/sdb".to_string()));
    }
}
