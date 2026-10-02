//! aether-install — AetherOS 磁盘安装器（M6）。
//!
//! 原理：ISO 是 isohybrid 镜像（重建流程末尾经 host isohybrid 处理，
//! 带 MBR 签名与隐藏 ISO 分区表），直接 dd 到块设备即可 BIOS 引导，
//! 引导链与光盘完全一致，零额外引导器依赖。
//!
//! 流程：
//!   1) 防呆校验（块设备 / 容量 / 显式 --yes）
//!   2) `dd if=/dev/sr0 of=<disk> bs=4M`（命令字面量，目标值仅作参数，无 shell 拼接）
//!   3) 【持久化】在磁盘空闲区追加第 2 分区（MBR 第 2 项，ext4）→ 重读分区表
//!      → mkfs.ext4 → 挂载建骨架 → 写安装标识 + 引导记录
//!   4) sync → 提示重启
//!
//! 第 3 步失败不影响引导（会明确告警）：系统仍能装，只是运行在内存里（无持久化）。
//! 启动时由 aether-init 把该分区挂到 /var（见其 mount_persist）。
//!
//! 用法（guest 内 root）：aether-install --disk /dev/vda --yes [--no-persist]

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// 安装源：光驱设备（内核 CONFIG_BLK_DEV_SR=y，devtmpfs 自动出节点）。
pub const SOURCE: &str = "/dev/sr0";
/// dd 的块大小。
pub const BS: &str = "4M";
/// 目标盘最小体积（MB）：镜像 32MB，留一倍余量。
pub const MIN_DISK_MB: u64 = 64;
/// 持久化分区起始扇区（32MB，2048 对齐，位于 ISO 区之后）。
pub const PERSIST_LBA_START: u32 = 65536;
/// 持久化分区最小扇区数（约 32MB，够放日志/状态）。
pub const PERSIST_MIN_SECTORS: u32 = 65536;
/// 持久化分区类型（Linux native）。
pub const PART_TYPE_LINUX: u8 = 0x83;
/// 持久化分区卷标（aether-init 据此识别）。
///
/// **必须与 `aether-init::persist::PERSIST_LABEL` 保持一致**：两边各有一份常量
/// （aether-init 不该依赖这个可执行 crate；这个 crate 连 aether-ipc 都不依赖，
/// 只为常量把 serde 拉进来不划算）。因此**两侧各钉一次字面量** —— 本文件有
/// `persist_label_matches_init_convention`，aether-init 有同名契约测试。
/// 只改一侧，那一侧就会红。
pub const PERSIST_LABEL: &str = "AETHER";
/// 持久化分区的挂载点（安装期）。
pub const PERSIST_MNT: &str = "/mnt/aether-persist";
/// 重读分区表的 ioctl（仅 Linux）。
#[cfg(target_os = "linux")]
const BLKRRPART: libc::c_ulong = 0x125f;

/// 解析命令行参数：(disk, yes, persist)。
pub fn parse_args(args: &[String]) -> Result<(String, bool, bool)> {
    let mut disk = None;
    let mut yes = false;
    let mut persist = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--disk" => {
                i += 1;
                disk = Some(
                    args.get(i)
                        .map(|s| s.to_string())
                        .ok_or_else(|| anyhow::anyhow!("--disk 缺少参数"))?,
                );
            }
            "--yes" => yes = true,
            "--no-persist" => persist = false,
            other => bail!("未知参数「{other}」（用法: aether-install --disk <块设备> --yes [--no-persist]）"),
        }
        i += 1;
    }
    let disk = disk.ok_or_else(|| anyhow::anyhow!("必须用 --disk 指定目标块设备（如 /dev/vda）"))?;
    Ok((disk, yes, persist))
}

/// 目标盘防呆校验：存在、是块设备（Unix）、不小于 MIN_DISK_MB。
pub fn validate_disk(disk: &str) -> Result<u64> {
    let path = Path::new(disk);
    let meta = std::fs::metadata(path).with_context(|| format!("目标 {disk} 不存在"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_block_device() {
            bail!("{disk} 不是块设备（拒绝写入，防止误写普通文件）");
        }
    }
    #[cfg(not(unix))]
    let _ = &meta;
    let mb = disk_size_mb(disk)?;
    if mb < MIN_DISK_MB {
        bail!("{disk} 只有 {mb}MB，小于最小要求 {MIN_DISK_MB}MB");
    }
    Ok(mb)
}

/// 块设备容量（MB）。块设备 metadata().len() 恒为 0，必须读 sysfs 扇区数。
pub fn disk_size_mb(disk: &str) -> Result<u64> {
    Ok(disk_size_sectors(disk)? * 512 / 1024 / 1024)
}

/// 块设备容量（512B 扇区数）。
pub fn disk_size_sectors(disk: &str) -> Result<u64> {
    let name = disk.trim_start_matches("/dev/");
    let raw = std::fs::read_to_string(format!("/sys/block/{name}/size"))
        .with_context(|| format!("无法读取 /sys/block/{name}/size（{disk} 不是块设备？）"))?;
    raw.trim()
        .parse::<u64>()
        .with_context(|| format!("解析 {disk} 扇区数失败: {raw:?}"))
}

/// 分区设备节点名：/dev/vda -> /dev/vda2，/dev/sda -> /dev/sda2。
/// （nvme 的 pN 命名不在 v0.2 范围）
pub fn partition_node(disk: &str, index: u32) -> String {
    format!("{disk}{index}")
}

/// MBR 分区项（16 字节，little-endian）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbrEntry {
    pub bootable: bool,
    pub part_type: u8,
    pub lba_start: u32,
    pub sectors: u32,
}

impl MbrEntry {
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = if self.bootable { 0x80 } else { 0x00 };
        // CHS 起始/结束留给 LBA 解释：填经典哨兵值（0xFE 0xFF 0xFF）
        b[1] = 0xFE;
        b[2] = 0xFF;
        b[3] = 0xFF;
        b[4] = self.part_type;
        b[5] = 0xFE;
        b[6] = 0xFF;
        b[7] = 0xFF;
        b[8..12].copy_from_slice(&self.lba_start.to_le_bytes());
        b[12..16].copy_from_slice(&self.sectors.to_le_bytes());
        b
    }

    pub fn from_bytes(b: &[u8]) -> Self {
        Self {
            bootable: b[0] == 0x80,
            part_type: b[4],
            lba_start: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            sectors: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.part_type == 0 && self.lba_start == 0 && self.sectors == 0
    }
}

/// 读取 MBR 里全部 4 个分区项（每项 16B，从偏移 446 起）。
pub fn read_entries(mbr: &[u8; 512]) -> [MbrEntry; 4] {
    std::array::from_fn(|i| MbrEntry::from_bytes(&mbr[446 + i * 16..446 + i * 16 + 16]))
}

/// 把第 index（1-based）个分区项写入 MBR。
pub fn set_partition_entry(
    mbr: &mut [u8; 512],
    index: usize,
    entry: &MbrEntry,
) -> Result<()> {
    if !(1..=4).contains(&index) {
        bail!("分区号 {index} 越界（1..=4）");
    }
    if entry.sectors == 0 {
        bail!("分区扇区数不能为 0");
    }
    if entry.lba_start == 0 {
        bail!("分区起始 LBA 不能为 0（0 保留给引导区）");
    }
    let off = 446 + (index - 1) * 16;
    mbr[off..off + 16].copy_from_slice(&entry.to_bytes());
    Ok(())
}

/// 在 MBR 空闲项里追加一个 Linux 数据分区；返回 (分区号, 起始 LBA, 扇区数)。
/// 已有空闲项不足时返回 None（不覆盖既有分区）。
pub fn plan_persist_partition(mbr: &[u8; 512], total_sectors: u64) -> Option<(usize, u32, u32)> {
    // MBR 分区项只有 32 位扇区数：>2TB 的盘无法在 MBR 里如实表达，
    // 截断写入会产生错误的分区大小——宁可放弃持久化分区也不能写错表。
    if total_sectors > u32::MAX as u64 {
        return None;
    }
    let start = PERSIST_LBA_START as u64;
    if total_sectors < start + PERSIST_MIN_SECTORS as u64 {
        return None; // 盘太小，放不下持久化分区
    }
    let entries = read_entries(mbr);
    // 找第一个空闲项
    let idx = entries.iter().position(|e| e.is_empty())? + 1;
    // 避开已有分区：起点不得落后于任何已用区域的末尾
    let lowest_free = entries
        .iter()
        .filter(|e| !e.is_empty())
        .map(|e| e.lba_start as u64 + e.sectors as u64)
        .max()
        .unwrap_or(0);
    let start = start.max(lowest_free) as u32;
    if (start as u64) + PERSIST_MIN_SECTORS as u64 > total_sectors {
        return None;
    }
    let sectors = (total_sectors - start as u64) as u32;
    Some((idx, start, sectors))
}

/// 重读分区表，等待分区节点出现。
#[cfg(target_os = "linux")]
fn reread_and_wait(disk: &str, index: u32) -> Result<PathBuf> {
    use std::os::unix::io::AsRawFd;
    let node = PathBuf::from(partition_node(disk, index));

    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk)
        .with_context(|| format!("打开 {disk} 失败"))?;
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), BLKRRPART as _) };
    if rc != 0 {
        eprintln!("aether-install: 警告 BLKRRPART ioctl 失败（内核可能已自动重扫）");
    }
    for _ in 0..20 {
        if node.exists() {
            return Ok(node);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("分区节点 {} 未出现", node.display())
}

/// 搭建持久化分区：追加分区 → 格式化 → 建骨架 → 写标识。
/// 返回分区节点路径。
#[cfg(target_os = "linux")]
fn setup_persist(disk: &str) -> Result<String> {
    let total = disk_size_sectors(disk)?;

    // 1. 读回磁盘头部（含 dd 写入的 isohybrid MBR）
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk)
        .with_context(|| format!("打开 {disk} 失败"))?;
    let mut mbr = [0u8; 512];
    f.read_exact(&mut mbr).context("读取 MBR 失败")?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        bail!("MBR 签名缺失（ISO 是否经过 isohybrid？）");
    }

    // 2. 规划并写入分区项
    let (index, start, sectors) = plan_persist_partition(&mbr, total).ok_or_else(|| {
        anyhow::anyhow!(
            "磁盘放不下持久化分区（剩余空间不足，或 >2TB 超出 MBR 可表达范围）；系统仍可引导但运行在内存里"
        )
    })?;
    let entry = MbrEntry {
        bootable: false,
        part_type: PART_TYPE_LINUX,
        lba_start: start,
        sectors,
    };
    set_partition_entry(&mut mbr, index, &entry)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&mbr)?;
    f.sync_all()?;
    drop(f);
    println!(
        "aether-install: 持久化分区 {index} 起始扇区 {start} 大小 {}MB",
        sectors as u64 * 512 / 1024 / 1024
    );

    // 3. 重读分区表并拿到节点
    let node = reread_and_wait(disk, index as u32)?;
    let node_s = node.to_string_lossy().to_string();

    // 4. 格式化
    let st = Command::new("/sbin/mkfs.ext4")
        .arg("-F")
        .arg("-q")
        .arg("-L")
        .arg(PERSIST_LABEL)
        .arg(&node_s)
        .status()
        .context("执行 mkfs.ext4 失败")?;
    if !st.success() {
        bail!("mkfs.ext4 {node_s} 失败: {st}");
    }

    // 5. 挂载 + 骨架
    std::fs::create_dir_all(PERSIST_MNT).ok();
    let st = Command::new("/bin/mount")
        .arg(&node_s)
        .arg(PERSIST_MNT)
        .status()
        .context("挂载持久化分区失败")?;
    if !st.success() {
        bail!("挂载 {node_s} 到 {PERSIST_MNT} 失败");
    }
    for d in ["log", "diag", "lib", "tmp", "spool", "home"] {
        std::fs::create_dir_all(format!("{PERSIST_MNT}/{d}")).ok();
    }
    // /var/run 在本系统里指向 /run（tmpfs）：持久分区上保持同样的软链，
    // 这样挂到 /var 后原有工具有正常的 pid 目录。
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let _ = symlink("/run", format!("{PERSIST_MNT}/run"));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::write(format!("{PERSIST_MNT}/log/install-id"), format!("{ts}\n")).ok();
    std::fs::write(
        format!("{PERSIST_MNT}/boot.log"),
        format!("{ts} 安装完成（分区 {node_s}）\n"),
    )
    .ok();

    // 6. 卸载（交给下次启动的 aether-init 挂到 /var）
    let _ = Command::new("/bin/umount").arg(PERSIST_MNT).status();
    println!("aether-install: 持久化已就绪（{node_s} → 启动时挂载到 /var）");
    Ok(node_s)
}

/// 安装源镜像是否具备可引导的 MBR 签名（isohybrid 的产物）。
///
/// **为什么必须在 dd 之前判**（2026-10-02 审计 H-6）：`setup_persist` 里也读 MBR，
/// 但那是在**写完目标盘之后** —— 那时目标盘首 32MB 已被覆盖、分区表已毁，
/// 再报错只是"事后告知"，数据不可恢复，而且程序仍会打印"安装完成"。
/// 实测：工作区 `aetheros-0.1-amd64.iso` 前 512 字节全 0、无 0x55AA、分区项全空，
/// 即纯 ISO9660 + El Torito，**不是** isohybrid。
///
/// 纯读、不写盘：这个函数返回 false 时调用方必须**中止且未动目标盘**。
fn source_has_mbr() -> Result<bool> {
    has_mbr(SOURCE)
}

/// 读 `path` 的前 512 字节并判断 MBR 签名（纯函数式，便于跨平台单测）。
fn has_mbr(path: &str) -> Result<bool> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("打不开安装源 {path}"))?;
    let mut mbr = [0u8; 512];
    f.read_exact(&mut mbr).with_context(|| format!("读取 {path} 头部失败"))?;
    Ok(mbr[510] == 0x55 && mbr[511] == 0xAA)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (disk, yes, persist) = parse_args(&args)?;

    let mb = validate_disk(&disk)?;
    // 目标盘不得是安装源自己：--disk /dev/sr0 会变成 dd if=/dev/sr0 of=/dev/sr0 自读自写
    if disk == SOURCE {
        bail!("目标盘不能是安装源（{SOURCE}）：拒绝自读自写");
    }
    println!("aether-install: 目标 {disk}（{mb}MB），源 {SOURCE}");
    if !yes {
        bail!("将整盘覆盖写入 {disk} —— 确认无误后追加 --yes 执行");
    }
    // 引导前提必须在**动目标盘之前**验证：不满足就中止，且**不做任何写入**。
    // 否则用户会得到一块被毁掉、又起不来的盘，而程序还报"安装完成"。
    if !source_has_mbr()? {
        bail!(
            "安装源 {SOURCE} 不是 isohybrid 镜像（首 512 字节无 MBR 签名 0x55AA）——\n\
             它只能作为光盘/ISO 引导，不能 dd 到磁盘引导。\n\
             构建侧需在产出 ISO 后执行 isohybrid（见 platform/build-iso.sh 与\n\
             scripts/rebuild-m4.sh）。\n\
             已中止：**未对 {disk} 做任何写入**"
        );
    }

    println!("aether-install: 写入中（约 32MB，几秒钟）…");
    let status = Command::new("/bin/dd")
        .arg(format!("if={SOURCE}"))
        .arg(format!("of={disk}"))
        .arg(format!("bs={BS}"))
        .status()
        .context("执行 /bin/dd 失败")?;
    if !status.success() {
        bail!("dd 写入失败: {status}");
    }

    if persist {
        #[cfg(target_os = "linux")]
        match setup_persist(&disk) {
            Ok(node) => println!("aether-install: 持久化分区 {node}"),
            Err(e) => {
                // 引导已就绪：持久化失败只告警，不判安装失败
                println!("aether-install: 警告 —— 持久化设置失败（{e}）；系统可引导但不保留状态");
            }
        }
        #[cfg(not(target_os = "linux"))]
        println!("aether-install: 非 Linux 环境，跳过持久化分区建立");
    } else {
        println!("aether-install: 已跳过持久化（--no-persist）");
    }

    let sync = Command::new("/bin/sync").status().context("执行 /bin/sync 失败")?;
    if !sync.success() {
        bail!("sync 失败");
    }
    println!("aether-install: 安装完成。请重启并移除引导介质，系统将从 {disk} 引导。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// 卷标必须与 `aether-init::persist::PERSIST_LABEL` 一致。
    ///
    /// 这里钉的是**字面量**：aether-init 那边有同名契约测试。两侧各钉一次，
    /// 于是"只改一侧"必然让那一侧变红（代码审查指出：只在一侧断言钉不住跨 crate 约定）。
    /// 真正的共享常量需要新 crate，收益不抵成本，见审计报告附录 C 的残余项。
    #[test]
    fn persist_label_matches_init_convention() {
        assert_eq!(PERSIST_LABEL, "AETHER");
    }

    /// 造一个与真实 isohybrid 镜像一致的 MBR：第 1 项类型 0x17、0..59392 扇区。
    fn isohybrid_mbr() -> [u8; 512] {
        let mut mbr = [0u8; 512];
        let e = MbrEntry {
            bootable: true,
            part_type: 0x17,
            lba_start: 0,
            sectors: 59392,
        };
        mbr[446..462].copy_from_slice(&e.to_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        mbr
    }

    #[test]
    fn parse_ok() {
        let (disk, yes, persist) = parse_args(&args(&["--disk", "/dev/vda", "--yes"])).unwrap();
        assert_eq!(disk, "/dev/vda");
        assert!(yes);
        assert!(persist);
        let (_, yes, persist) =
            parse_args(&args(&["--yes", "--disk", "/dev/sda", "--no-persist"])).unwrap();
        assert!(yes);
        assert!(!persist);
    }

    #[test]
    fn parse_requires_disk_and_rejects_unknown() {
        assert!(parse_args(&args(&["--yes"])).is_err());
        assert!(parse_args(&args(&["--disk"])).is_err());
        assert!(parse_args(&args(&["--disk", "/dev/vda", "--evil"])).is_err());
    }

    #[test]
    fn entry_roundtrip_and_checks() {
        let e = MbrEntry {
            bootable: false,
            part_type: PART_TYPE_LINUX,
            lba_start: 65536,
            sectors: 983040,
        };
        assert_eq!(MbrEntry::from_bytes(&e.to_bytes()), e);
        let mut mbr = isohybrid_mbr();
        set_partition_entry(&mut mbr, 2, &e).unwrap();
        let entries = read_entries(&mbr);
        assert_eq!(entries[1], e);
        assert!(!entries[0].is_empty(), "第 1 项（ISO）必须保持不动");
        assert_eq!(entries[0].part_type, 0x17);
        assert!(mbr[510] == 0x55 && mbr[511] == 0xAA, "签名不得被破坏");
        // 越界与非法值
        assert!(set_partition_entry(&mut mbr, 0, &e).is_err());
        assert!(set_partition_entry(&mut mbr, 5, &e).is_err());
        assert!(set_partition_entry(&mut mbr, 2, &MbrEntry { sectors: 0, ..e }).is_err());
        assert!(set_partition_entry(&mut mbr, 2, &MbrEntry { lba_start: 0, ..e }).is_err());
    }

    #[test]
    fn plan_persist_on_512mb_disk() {
        let mbr = isohybrid_mbr();
        let total = 1048576; // 512MB
        let (idx, start, sectors) = plan_persist_partition(&mbr, total).unwrap();
        assert_eq!(idx, 2, "应追加到第 2 项");
        assert_eq!(start, PERSIST_LBA_START);
        assert_eq!(start as u64 + sectors as u64, total, "应覆盖到盘尾");
        assert_eq!(sectors, total as u32 - PERSIST_LBA_START);
    }

    #[test]
    fn plan_persist_respects_existing_partitions() {
        // 假设第 1 项已经占到 100000 扇区 —— 新分区必须从其后开始
        let mut mbr = isohybrid_mbr();
        let big = MbrEntry {
            bootable: true,
            part_type: 0x17,
            lba_start: 0,
            sectors: 100000,
        };
        mbr[446..462].copy_from_slice(&big.to_bytes());
        let total = 1048576u64;
        let (idx, start, sectors) = plan_persist_partition(&mbr, total).unwrap();
        assert_eq!(idx, 2);
        assert_eq!(start, 100000);
        assert_eq!(start as u64 + sectors as u64, total);
    }

    #[test]
    fn plan_persist_skips_when_disk_too_small() {
        let mbr = isohybrid_mbr();
        assert!(plan_persist_partition(&mbr, 40960).is_none()); // 20MB
        assert!(plan_persist_partition(&mbr, 131071).is_none()); // 略小于起点+最小
    }

    #[test]
    fn plan_persist_skips_beyond_mbr_capacity() {
        // >2TB（u32::MAX 扇区）超出 MBR 可表达范围：宁可放弃持久化也不截断扇区数
        let mbr = isohybrid_mbr();
        let huge = u32::MAX as u64 + 1;
        assert!(plan_persist_partition(&mbr, huge).is_none());
    }

    #[test]
    fn plan_persist_none_when_table_full() {
        let mut mbr = isohybrid_mbr();
        for i in 1usize..4 {
            let e = MbrEntry {
                bootable: false,
                part_type: 0x83,
                lba_start: 70000 + i as u32 * 1000,
                sectors: 1000,
            };
            mbr[446 + i * 16..446 + i * 16 + 16].copy_from_slice(&e.to_bytes());
        }
        assert!(plan_persist_partition(&mbr, 1048576).is_none(), "4 项占满则放弃");
    }

    #[test]
    fn partition_node_naming() {
        assert_eq!(partition_node("/dev/vda", 2), "/dev/vda2");
        assert_eq!(partition_node("/dev/sda", 1), "/dev/sda1");
    }

    /// H-6 回归：安装前必须能识别"源镜像不是 isohybrid"。
    ///
    /// 这条判定的价值在于**时机**：它必须在 dd 之前跑。此前同样的检查写在
    /// `setup_persist` 里（dd 之后），于是目标盘已被覆盖、分区表已毁才发现起不来。
    #[test]
    fn has_mbr_detects_isohybrid_and_rejects_plain_iso() {
        let dir = std::env::temp_dir().join("aether_install_mbr_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");

        // 1) 纯 ISO9660（前 512 字节全 0）：不是 isohybrid
        let plain = dir.join("plain.iso");
        std::fs::write(&plain, vec![0u8; 512]).unwrap();
        assert!(!has_mbr(&plain.to_string_lossy()).unwrap(), "全 0 头部应判为非 isohybrid");

        // 2) 带 MBR 签名：是 isohybrid
        let hybrid = dir.join("hybrid.iso");
        let mut buf = vec![0u8; 512];
        buf[510] = 0x55;
        buf[511] = 0xAA;
        std::fs::write(&hybrid, &buf).unwrap();
        assert!(has_mbr(&hybrid.to_string_lossy()).unwrap(), "0x55AA 应判为 isohybrid");

        // 3) 只有半个签名也不行
        let half = dir.join("half.iso");
        let mut buf = vec![0u8; 512];
        buf[510] = 0x55;
        std::fs::write(&half, &buf).unwrap();
        assert!(!has_mbr(&half.to_string_lossy()).unwrap());

        // 4) 文件不足 512 字节：报错而不是当成"没有签名"（避免把读取失败误判为判定结果）
        let tiny = dir.join("tiny.bin");
        std::fs::write(&tiny, vec![0u8; 100]).unwrap();
        assert!(has_mbr(&tiny.to_string_lossy()).is_err(), "不足 512 字节应报错");

        // 5) 文件不存在：报错
        assert!(has_mbr(&dir.join("nope.iso").to_string_lossy()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
