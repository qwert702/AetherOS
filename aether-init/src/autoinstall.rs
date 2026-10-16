//! 引导期**无人安装**（live 介质专用）—— 让"挂上 ISO 一开机就装完"成立。
//!
//! ## 触发条件必须**同时**满足两条
//!
//! 1. 内核命令行里有 `aether.autoinstall=1`（只有引导菜单里那个"自动安装"项带它）；
//! 2. **本机还没有 AetherOS 的安装**（任一分区没有合法安装标记，见
//!    [`crate::persist::any_existing_install`]）。
//!
//! 只看第 1 条会出**毁盘级**事故：系统分区是 ISO 的拷贝，连引导配置一起被装进磁盘，
//! 于是**从磁盘引导时命令行照样带这个参数** —— 它会把刚装好的系统**再装一遍**。
//!
//! ## 第 2 条判据踩过的坑（2026-10-15 实测）
//!
//! 第一版写的是"根是**只读** + iso9660/squashfs 之类的 live 类型"。实机一跑就**永远不成立**：
//! AetherOS 是 initramfs 引导，根报 `rootfs`（rw），既不是 ISO 类型也不是只读 ——
//! 串口里看得清清楚楚：命令行有 `aether.autoinstall=1`，却没有一行我的输出（判据静默不成立）。
//!
//! 幸好当时的降级路径是"什么都不做、按普通 live 启动"，所以**没有乱写磁盘**。
//! 换成本项目自己的**安装标记**（写持久分区上、绑定磁盘 MBR 签名）之后：
//! 从已装盘引导时标记必然在 ⇒ 绝不重装。**判据要挑"这件事本身的证据"，而不是它的间接迹象。**
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

#[cfg(target_os = "linux")]
use std::path::Path;

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

/// 命令行里是否有开关（逐 token 比较）。
pub fn flag_present(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|t| t == FLAG)
}

/// 该不该装：**开关在** 且 **本机还没有安装**。两条缺一不可（第二条是毁盘级判据）。
///
/// 抽成纯函数是为了能在任意平台跑单元测试 —— 这条判据的分支一旦写错就是"重装整盘"。
pub fn should_install(flag: bool, already_installed: bool) -> bool {
    flag && !already_installed
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

/// 读 `/proc/cmdline`，并在开关存在时问一次"本机装过没有"。
///
/// 注意：**先看开关再探测** —— 开关不在时完全不去碰磁盘（不做任何多余动作）。
#[cfg(target_os = "linux")]
pub fn requested() -> bool {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let flag = flag_present(&cmdline);
    if !flag {
        return false;
    }
    let already = crate::persist::any_existing_install();
    if already {
        eprintln!("[aether-init] 自动安装跳过：本机已有 AetherOS 安装（安装标记在）—— 绝不重装");
    }
    should_install(flag, already)
}

#[cfg(not(target_os = "linux"))]
pub fn requested() -> bool {
    false
}

/// 找一个真实存在的安装器（按候选顺序）。找不到返回 `None` —— 调用方**降级为普通 live 启动**。
#[cfg(target_os = "linux")]
pub fn installer_path() -> Option<&'static str> {
    INSTALLER_CANDIDATES
        .iter()
        .find(|p| Path::new(p).is_file())
        .copied()
}

/// 装机并**关机**（不返回）。只在"该装 + 装得了"时调用。
///
/// 为什么关机而不是重启：让宿主有机会先改引导顺序（光驱 → 磁盘）再开机，
/// 否则会又从 ISO 引导一次（虽然安装标记挡住了重装，但那样就白装一遍）。
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
    fn installs_only_with_flag_and_no_existing_install() {
        // 该装：开关在 + 盘是空的
        assert!(should_install(true, false));
        // 开关在但**已经装过** ⇒ 绝不装（毁盘级：从盘引导时命令行同样带这个开关）
        assert!(!should_install(true, true));
        // 没有开关：无论盘什么状态都不动
        assert!(!should_install(false, false));
        assert!(!should_install(false, true));
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
