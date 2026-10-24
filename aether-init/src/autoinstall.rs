//! 引导期**无人安装**（live 介质专用）—— 让"挂上 ISO 一开机就装完"成立。
//!
//! ## 两种模式，判据互补（都必须"开关 + 盘的现状"同时成立）
//!
//! | 模式 | 引导开关 | 还要求 | 效果 |
//! |---|---|---|---|
//! | 首次装机 | `aether.autoinstall=1` | 盘上**还没有** AetherOS | 整盘装（含重建 /var） |
//! | 换系统   | `aether.keepvar=1`     | 盘上**已经有** AetherOS | 只换系统，**/var 原地保留** |
//!
//! 只看开关会出**毁盘级**事故：系统分区是 ISO 的拷贝，连引导配置一起被装进磁盘，
//! 于是**从磁盘引导时命令行照样带那个开关** —— 首次装机模式会把刚装好的系统**再装一遍**。
//!
//! ## 第 2 条判据踩过的坑（2026-10-15 实测）
//!
//! 第一版写的是"根是**只读** + iso9660/squashfs 之类的 live 类型"。实机一跑就**永远不成立**：
//! AetherOS 是 initramfs 引导，根报 `rootfs`（rw），既不是 ISO 类型也不是只读 ——
//! 串口里看得清清楚楚：命令行有 `aether.autoinstall=1`，却没有一行我的输出（判据静默不成立）。
//!
//! 幸好当时的降级路径是"什么都不做、按普通 live 启动"，所以**没有乱写磁盘**。
//! 换成本项目自己的**安装标记**（写持久分区上、绑定磁盘 MBR 签名）之后一次通过。
//! **判据要挑"这件事本身的证据"，而不是它的间接迹象。**

#[cfg(target_os = "linux")]
use std::path::Path;

/// 首次装机开关。**逐 token 精确匹配** —— `contains` 会让 `aether.autoinstall=10` 假阳。
pub const FLAG: &str = "aether.autoinstall=1";
/// 换系统（保留 /var）开关。
pub const KEEP_VAR_FLAG: &str = "aether.keepvar=1";

/// 结果标记：宿主脚本与串口判据按这几行取证据（不要改字面量，证据文件里引用了它们）。
///
/// 只被 `run()`（Linux 专属）使用 ⇒ 按平台加门。否则 `--all-targets` 构建里，
/// 测试目标下的 Windows 版本会因为"没人用"报 dead_code 告警（项目标准是 0 告警）。
#[cfg(target_os = "linux")]
pub const RESULT_PASS: &str = "AETHER-AUTOINSTALL: PASS";
#[cfg(target_os = "linux")]
pub const RESULT_FAIL: &str = "AETHER-AUTOINSTALL: FAIL";
#[cfg(target_os = "linux")]
pub const RESULT_PASS_KEEP: &str = "AETHER-KEEPVAR: PASS";

/// 安装器候选路径。镜像里装在 `/usr/bin`；留候选是为了不让"路径变了"变成静默失败。
#[cfg(target_os = "linux")]
pub const INSTALLER_CANDIDATES: [&str; 3] = [
    "/usr/bin/aether-install",
    "/sbin/aether-install",
    "/bin/aether-install",
];

/// 这次启动该干什么。三态而不是 bool：**"什么都不做"是一等公民** ——
/// 判据不成立时必须什么都不做，而不是猜一个动作去做。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 不装（按普通 live 启动，或按已装系统启动）。
    None,
    /// 首次装机：整盘写系统 + 重建 /var。
    Install,
    /// 换系统：只写系统分区，**保留 /var**（安装器 `--keep-var`）。
    InstallKeepVar,
}

/// 命令行里是否有某个开关（逐 token 比较）。
pub fn flag_present(cmdline: &str, flag: &str) -> bool {
    cmdline.split_whitespace().any(|t| t == flag)
}

/// 光驱"有没有盘"的权威判据：`CDROM_DRIVE_STATUS` 的返回值（见 linux/cdrom.h）。
///
/// 为什么不再用 `/sys/block/sr0/size`：设备存在但**没放盘**时，该值仍可能报告
/// 上一次介质的容量（VMware 实测，2026-10-22）—— 于是一次从盘引导会被误判成
/// "在 live 介质上"，把 `aether.keepvar=1` 当成"请再装一遍"，安装器找不到安装源后关机。
pub const CDS_NO_INFO: i32 = 0;
/// 光驱里没有盘。
pub const CDS_NO_DISC: i32 = 1;
/// 托盘打开。
pub const CDS_TRAY_OPEN: i32 = 2;
/// 还没就绪（虚拟光驱刚上电常见，值得重试）。
pub const CDS_DRIVE_NOT_READY: i32 = 3;
/// 有盘且可读。
pub const CDS_DISC_OK: i32 = 4;

/// `CDROM_DRIVE_STATUS` 的 ioctl 请求号（linux/cdrom.h）。
#[cfg(target_os = "linux")]
const CDROM_DRIVE_STATUS: libc::c_int = 0x5326;

/// 只有"有盘且可读"才算 live（**纯函数**，便于测试；不猜）。
pub fn disc_ready(status: i32) -> bool {
    status == CDS_DISC_OK
}

/// 打开 `/dev/sr0` 问一次"有没有盘"。
///
/// * 打不开设备（没有光驱，或光驱已被摘掉）⇒ 不是 live（从盘引导的常态）；
/// * `CDS_DISC_OK` ⇒ live；`CDS_NO_DISC` ⇒ 立刻结论"没有盘"；
/// * 其它（`CDS_DRIVE_NOT_READY` / `CDS_TRAY_OPEN` / 失败）⇒ **有界重试** 20×100ms 后判否，
///   绝不无限等待。
#[cfg(target_os = "linux")]
fn cdrom_has_disc() -> bool {
    use std::os::unix::io::AsRawFd;
    for _ in 0..20 {
        let Ok(f) = std::fs::File::open("/dev/sr0") else {
            return false;
        };
        // SAFETY: 用刚打开的有效 fd；CDROM_DRIVE_STATUS 只返回整数状态，不写用户内存。
        let rc = unsafe { libc::ioctl(f.as_raw_fd(), CDROM_DRIVE_STATUS, 0) };
        if disc_ready(rc) {
            return true;
        }
        if rc == CDS_NO_DISC {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// 决定这次启动干什么。**纯函数**（判据一旦写错就是"重装整盘"或"误删 /var"）。
///
/// 两条判据互补：
/// * 首次装机要求"还没装过" —— 否则从盘引导会把刚装好的系统再装一遍（毁盘级）；
/// * 换系统要求"已经装过" —— 没有旧系统时"保留 /var"没有对象，安装器也会中止。
pub fn plan(
    first_install_flag: bool,
    keep_var_flag: bool,
    already_installed: bool,
    live: bool,
) -> Action {
    // 两种自动动作都要求"在 live 介质上" —— 否则从磁盘引导时，引导配置里残留的
    // aether.keepvar=1 / aether.autoinstall=1 会让系统**反复重装**（2026-10-17 实测）。
    if !live {
        return Action::None;
    }
    if keep_var_flag && already_installed {
        return Action::InstallKeepVar;
    }
    if first_install_flag && !already_installed {
        return Action::Install;
    }
    Action::None
}

/// 从 `/proc/partitions` 里挑目标盘：跳过可移动/虚拟设备，取最大的一块**整盘**。
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

/// 读 `/proc/cmdline`，并在有开关时问一次"本机装过没有"，最后交给纯函数判定。
#[cfg(target_os = "linux")]
pub fn requested() -> Action {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let first = flag_present(&cmdline, FLAG);
    let keep = flag_present(&cmdline, KEEP_VAR_FLAG);
    if !first && !keep {
        return Action::None;
    }
    let already = crate::persist::any_existing_install();
    // 光驱里有盘 ⇒ 大概率是插着 ISO 引导；从盘引导时按手册光驱是断开的（见 cdrom_has_disc）
    let live = cdrom_has_disc();
    let action = plan(first, keep, already, live);
    if !live && (first || keep) {
        eprintln!(
            "[aether-init] 自动动作跳过：光驱里没有盘（说明不是 live 引导，而是从磁盘引导）\
             —— 引导配置里残留的 aether.autoinstall/keepvar 参数不会触发重装"
        );
    }
    match action {
        Action::None if keep => eprintln!(
            "[aether-init] 换系统跳过：盘上没有 AetherOS 安装（没有可保留的 /var）—— 按普通 live 启动"
        ),
        Action::None if first => eprintln!(
            "[aether-init] 首次装机跳过：本机已有 AetherOS 安装（安装标记在）—— 绝不重装"
        ),
        _ => {}
    }
    action
}

// 注意：`requested()` 只在 Linux 上存在 —— 唯一调用点是 main.rs 的引导期钩子（同为 Linux 专属）。

/// 找一个真实存在的安装器（按候选顺序）。找不到返回 `None` —— 调用方**降级为普通 live 启动**。
#[cfg(target_os = "linux")]
pub fn installer_path() -> Option<&'static str> {
    INSTALLER_CANDIDATES
        .iter()
        .find(|p| Path::new(p).is_file())
        .copied()
}

/// 装机并**关机**（不返回）。`keep_var` 决定是否保留 /var。
#[cfg(target_os = "linux")]
pub fn run(installer: &str, disk: &str, keep_var: bool) -> ! {
    use crate::power::sync_and_reboot;
    // PowerAction 定义在 aether_ipc 里（不是 power）—— 从 power 导会报 E0603 私有访问
    use aether_ipc::PowerAction;

    let mut args = vec!["--disk", disk, "--yes"];
    if keep_var {
        args.push("--keep-var");
    }
    eprintln!(
        "[aether-init] 自动安装开始（keep_var={keep_var}）：{installer} {}",
        args.join(" ")
    );
    let status = std::process::Command::new(installer).args(&args).status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("[aether-init] 自动安装完成（{disk}，keep_var={keep_var}）");
            eprintln!("{}", if keep_var { RESULT_PASS_KEEP } else { RESULT_PASS });
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
        assert!(flag_present("console=ttyS0 aether.autoinstall=1 quiet", FLAG));
        assert!(flag_present("aether.keepvar=1", KEEP_VAR_FLAG));
        // 前缀像但值不同：**不能**假阳（否则"手动装到一半"会被误判）
        assert!(!flag_present("aether.autoinstall=10", FLAG));
        assert!(!flag_present("aether.autoinstall=1x", FLAG));
        assert!(!flag_present("xaether.autoinstall=1", FLAG));
        assert!(!flag_present("", FLAG));
    }

    #[test]
    fn first_install_only_when_disk_is_empty() {
        // 开关在 + 盘是空的 ⇒ 装
        assert_eq!(plan(true, false, false, true), Action::Install);
        // 开关在但**已经装过** ⇒ 绝不装（毁盘级：从盘引导时命令行同样带这个开关）
        assert_eq!(plan(true, false, true, true), Action::None);
        // 没有开关：无论如何都不动
        assert_eq!(plan(false, false, false, true), Action::None);
        assert_eq!(plan(false, false, true, true), Action::None);
    }

    #[test]
    fn keep_var_only_when_disk_already_installed() {
        // 换系统 + 已有安装 ⇒ 保留 /var 地装
        assert_eq!(plan(false, true, true, true), Action::InstallKeepVar);
        // 换系统但没有旧系统 ⇒ 什么都不做（没有可保留的 /var，安装器也会中止）
        assert_eq!(plan(false, true, false, true), Action::None);
        // 两个开关同时给：以"换系统"为准（盘已装过时）
        assert_eq!(plan(true, true, true, true), Action::InstallKeepVar);
        // 两个开关同时给且盘是空的：按首次装机
        assert_eq!(plan(true, true, false, true), Action::Install);
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

    #[test]
    fn disc_ready_only_accepts_disc_ok() {
        // 正向：有盘可读才算 live
        assert!(disc_ready(CDS_DISC_OK), "CDS_DISC_OK ⇒ live");
        // 负向：没盘 / 托盘开 / 未就绪 / 未知 / ioctl 失败（负值）都不是 live
        assert!(!disc_ready(CDS_NO_DISC), "没盘 ⇒ 不是 live");
        assert!(!disc_ready(CDS_TRAY_OPEN), "托盘打开 ⇒ 不是 live");
        assert!(!disc_ready(CDS_DRIVE_NOT_READY), "未就绪 ⇒ 不是 live（不猜）");
        assert!(!disc_ready(CDS_NO_INFO), "未知 ⇒ 不是 live");
        assert!(!disc_ready(-1), "ioctl 失败 ⇒ 不是 live");
    }

    #[test]
    fn no_live_medium_never_installs_even_with_flags() {
        // 从磁盘引导时，引导配置里残留的开关**不能**触发任何自动动作（否则会反复重装）
        assert_eq!(plan(true, false, false, false), Action::None, "无盘 + autoinstall 开关 ⇒ 什么都不做");
        assert_eq!(plan(false, true, true, false), Action::None, "无盘 + keepvar 开关 ⇒ 什么都不做");
        assert_eq!(plan(true, true, true, false), Action::None);
        // 有盘时行为不变
        assert_eq!(plan(true, false, false, true), Action::Install);
        assert_eq!(plan(false, true, true, true), Action::InstallKeepVar);
    }
}
