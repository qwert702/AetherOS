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

/// 安装标记文件（相对持久分区根）。
///
/// **必须与 `aether-init::persist::INSTALL_MARKER` 保持一致** —— 写入侧（这里）与
/// 识别侧（aether-init 只读探测）各有一份常量，改一边不改另一边就会让
/// "装好了但重启后挂不上"。两处各有断言钉住字面量（`install_marker_matches_*`）。
pub const INSTALL_MARKER: &str = "log/install-id";
/// 重读分区表的 ioctl（仅 Linux）。
#[cfg(target_os = "linux")]
const BLKRRPART: libc::c_ulong = 0x125f;

/// 解析命令行参数：(disk, yes, persist)。
/// 命令行选项。
pub struct Options {
    pub disk: String,
    pub yes: bool,
    pub persist: bool,
    /// `--wipe`：先整盘清零再写入（审计 M-8）。默认**不做** ——
    /// 整盘清零在几百 GB 的盘上要几分钟到几十分钟，而"旧数据仍留在未使用扇区"
    /// 对演示系统不是威胁模型的一部分。需要移交/回收机器时显式打开。
    pub wipe: bool,
    /// `--keep-var`：**保留现有 /var**（只换系统，不重建持久分区）。默认**关** ——
    /// 它是"就地换系统"：误用会留下一个与旧系统混在一起的 /var；要换系统时显式打开。
    ///
    /// 为什么必须在 **dd 之前**记住分区项：dd 会把 ISO 的 MBR（只含 ISO 那一项）
    /// 写进目标盘首扇区，旧的分区表与**磁盘签名一起被覆盖** ——
    /// 这正是"整机更新会丢 /var"的机制（见 `capture_persist`）。
    pub keep_var: bool,
}

pub fn parse_args(args: &[String]) -> Result<Options> {
    let mut disk = None;
    let mut yes = false;
    let mut persist = true;
    let mut wipe = false;
    let mut keep_var = false;
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
            "--wipe" => wipe = true,
            "--keep-var" => keep_var = true,
            other => bail!(
                "未知参数「{other}」（用法: aether-install --disk <块设备> --yes \
                 [--no-persist] [--wipe] [--keep-var]）"
            ),
        }
        i += 1;
    }
    let disk = disk.ok_or_else(|| anyhow::anyhow!("必须用 --disk 指定目标块设备（如 /dev/vda）"))?;
    Ok(Options { disk, yes, persist, wipe, keep_var })
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

/// 把目标盘路径规范化成 `/dev/<name>`（2026-10-02 审计 M-7）。
///
/// 为什么不能只做字符串比较：`metadata()` 接受 `/dev//vda`、`/dev/./vda`、
/// `/dev/disk/by-id/...` 这些写法，而"目标盘不得是安装源"原先用的是
/// `disk == SOURCE` 纯字符串比较 —— 多一个斜杠就能绕过，
/// 结果是 `dd if=/dev/sr0 of=/dev//sr0`：**读安装介质又写回安装介质**。
/// 这里先 `canonicalize`（解析符号链接与 `.`/`..`），再要求落在 `/dev` 下。
pub fn canonicalize_disk(disk: &str) -> Result<String> {
    let p = std::fs::canonicalize(disk).with_context(|| format!("目标 {disk} 不存在"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let md = std::fs::metadata(&p)?;
        if !md.file_type().is_block_device() {
            bail!("{} 不是块设备（拒绝写入，防止误写普通文件）", p.display());
        }
    }
    let s = p.to_string_lossy().to_string();
    let name = s.strip_prefix("/dev/").unwrap_or("");
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        bail!("目标 {s} 不是 /dev 下的裸块设备名（拒绝写入）");
    }
    Ok(format!("/dev/{name}"))
}

/// 两个路径是否指向同一个设备（用于"拒绝自读自写"）。
///
/// 用 **`st_rdev` 设备号**比较，而不是路径字符串（对抗审查发现）：
/// 路径比较可被设备别名绕过 —— `mount --bind /dev/sr0 /dev/h/sr0` 或
/// `ln /dev/sr0 /dev/h`（hardlink，`canonicalize` 不解析）都会让两条路径不同名
/// 却指向同一个设备。设备号是内核给的唯一标识，绕不过去。
pub fn same_device(a: &str, b: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            // rdev 为 0 表示不是设备节点（普通文件）；那种情况仍按路径比较
            if ma.rdev() != 0 || mb.rdev() != 0 {
                return ma.rdev() == mb.rdev();
            }
        }
    }
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| std::path::PathBuf::from(a));
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| std::path::PathBuf::from(b));
    ca == cb
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
/// MBR 里的磁盘签名字段偏移（标准位置：0x1B8 = 440，小端 4 字节）。
pub const MBR_DISK_SIG_OFFSET: usize = 440;

/// 读 MBR 的磁盘签名。**纯函数**（跨平台可测）。
///
/// 为什么用它做身份绑定（安全审计 #13 / H-3 残余）：
/// 卷标与安装标记都**住在持久分区自己身上** —— 拷一个分区过来就都跟着来了。
/// 磁盘签名住在**盘**的 MBR 里：分区被拷到别的盘（U 盘/另一台机器）时，
/// 那个盘的 MBR 不会带着同一个签名，于是 aether-init 拒绝挂载。
/// **诚实边界**：能整盘伪造（含 MBR）的人仍可绕过 —— 那要靠安全启动/TPM。
pub fn mbr_disk_signature(mbr: &[u8; 512]) -> u32 {
    u32::from_le_bytes([
        mbr[MBR_DISK_SIG_OFFSET],
        mbr[MBR_DISK_SIG_OFFSET + 1],
        mbr[MBR_DISK_SIG_OFFSET + 2],
        mbr[MBR_DISK_SIG_OFFSET + 3],
    ])
}

/// 写入磁盘签名（纯函数）。
pub fn set_disk_signature(mbr: &mut [u8; 512], sig: u32) {
    mbr[MBR_DISK_SIG_OFFSET..MBR_DISK_SIG_OFFSET + 4].copy_from_slice(&sig.to_le_bytes());
}

/// 生成一个**非零**随机磁盘签名。
///
/// 只在 Linux 与测试下编译：唯一的生产调用点在 `setup_persist`（`cfg(target_os = "linux")`）里，
/// 而测试要在**所有**平台都能跑（否则这段逻辑在开发机上永远没被执行过）。
/// 不加这个 cfg 的话，Windows 构建会报"函数未使用"警告。
#[cfg(any(target_os = "linux", test))]
///
/// 非零是硬要求：0 是"未设置"的常见初值，用它会让"没绑定"与"绑定到 0"混淆 ——
/// 而 aether-init 侧的判据是 fail-closed，混淆的代价是分区挂不上。
/// 随机源取 `/dev/urandom`；拿不到就退回时间戳混合（可用性优先，安全性由"非零 + 随机"保证）。
fn random_disk_signature() -> u32 {
    use std::io::Read;
    let mut b = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut b).is_ok() {
            let v = u32::from_le_bytes(b);
            if v != 0 {
                return v;
            }
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let pid = std::process::id() as u64;
    let mixed = (ts ^ (pid << 32) ^ (ts >> 17)) as u32;
    if mixed == 0 {
        1
    } else {
        mixed
    }
}

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
fn setup_persist(disk: &str, kept: Option<(usize, MbrEntry, u32)>) -> Result<String> {
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

    // 2. 写入分区项。`--keep-var` 时**沿用记录下来的那一项**，磁盘签名也一并还原 ——
    // 安装标记绑定的就是那个签名，变一下启动侧的身份校验就失败（/var 挂不上）。
    let (index, entry, disk_sig) = match &kept {
        Some((idx, e, sig)) => {
            println!("aether-install: --keep-var：沿用原分区项（不重新规划），/var 原地保留");
            (*idx, e.clone(), *sig)
        }
        None => {
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
            // 身份绑定（审计 #13）：给这块盘一个随机签名，并把它写进分区里的安装标记。
            // 启动时 aether-init 要求"分区所在盘的 MBR 签名 == 标记里的 id" ——
            // 拷到别的盘的分区因此挂不上（见 `mbr_disk_signature` 的说明与诚实边界）。
            (index, entry, random_disk_signature())
        }
    };
    set_partition_entry(&mut mbr, index, &entry)?;
    set_disk_signature(&mut mbr, disk_sig);
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&mbr)?;
    f.sync_all()?;
    drop(f);
    let (start, sectors) = (entry.lba_start, entry.sectors);
    println!(
        "aether-install: 持久化分区 {index} 起始扇区 {start} 大小 {}MB",
        sectors as u64 * 512 / 1024 / 1024
    );

    // 3. 重读分区表并拿到节点
    let node = reread_and_wait(disk, index as u32)?;
    let node_s = node.to_string_lossy().to_string();

    // 4. `--keep-var` 时**整段跳过**：既不清签名也不 mkfs —— 这就是"保留 /var"的全部含义。
    if kept.is_none() {
    // 清掉这块分区上遗留的文件系统签名，再格式化（审计 M-8）。
    //
    // 为什么必须清：装机目标盘上常常有**上一个系统**的分区痕迹。mkfs.ext4 -F 会
    // 覆盖本分区的超级块，但残留的其它签名（如旧的 vfat/LVM/RAID 元数据）会让
    // 内核的自动探测与 `blkid` 报出乱七八糟的结果。wipefs 只动签名、不擦数据，
    // 秒级完成；工具缺失时只提示（不让装机因此失败）。
    if std::path::Path::new("/sbin/wipefs").exists() {
        let st = Command::new("/sbin/wipefs").arg("-a").arg(&node_s).status();
        match st {
            Ok(s) if s.success() => {}
            Ok(s) => println!("aether-install: 提示 —— wipefs 返回 {s}（继续格式化）"),
            Err(e) => println!("aether-install: 提示 —— wipefs 未能执行（{e}）"),
        }
    } else {
        println!("aether-install: 提示 —— 镜像内没有 wipefs，跳过旧签名清理");
    }

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
    } else {
        println!("aether-install: --keep-var：跳过 wipefs/mkfs —— 持久分区原样保留");
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
    // `apps` 必须在骨架里（审计 M-8）：`aetherd::apps::DEFAULT_DIR` 是 `/var/apps`，
    // 持久分区挂到 /var 之后这个目录就是"已装应用"的家。安装时建好它，
    // 用户装机后第一件事（装应用）才不会碰到"目录不存在"。
    // 骨架目录必须建出来：写不进目录 = 后面的标记文件也写不了，
    // 而"装机成功但重启后挂不上持久分区"是最难查的一类故障（代码审查指出）。
    for d in ["log", "diag", "lib", "tmp", "spool", "home", "apps"] {
        std::fs::create_dir_all(format!("{PERSIST_MNT}/{d}"))
            .map_err(|e| anyhow::anyhow!("创建 {PERSIST_MNT}/{d} 失败: {e}"))?;
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
    // 这个标记文件是**启动侧识别"这是我们的持久分区"的依据**（审计 H-3 残余）：
    // aether-init 会先把候选分区只读挂起，确认 `log/install-id` 存在才改挂 rw。
    // 诚实边界：能写盘的人可以伪造它；它挡的是"误挂别人的分区"（双系统机器上
    // 把宿主机的根分区当成 /var），不是有物理访问权的攻击者 —— 后者要靠安全启动/TPM。
    //
    // 写失败**必须报错**（代码审查指出）：这个标记现在是启动侧识别分区的**唯一判据**，
    // 写不进去却报"装机成功"，用户重启后会看到持久分区永远挂不上、而且毫无线索。
    // 常量与 aether-init 侧一致（那边的 `INSTALL_MARKER` 有断言钉住字面量）。
    // 第二行是身份绑定 id（与目标盘 MBR 的磁盘签名一致）—— 格式必须与
    // aether-init 的 `marker_install_id`（`id=<8 位十六进制>`）一致。
    std::fs::write(
        format!("{PERSIST_MNT}/{INSTALL_MARKER}"),
        format!("{ts}\nid={disk_sig:08x}\n"),
    )
        .map_err(|e| anyhow::anyhow!("写入 {INSTALL_MARKER} 失败: {e} —— 持久分区将无法被识别"))?;
    // `--keep-var` 时**追加**而不是覆盖：既然承诺"保留 /var"，就不该把它的启动历史清掉。
    let boot_line = format!("{ts} 安装完成（分区 {node_s}）\n");
    let boot_log = format!("{PERSIST_MNT}/boot.log");
    let boot_res = if kept.is_some() {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&boot_log)
            .and_then(|mut f| std::io::Write::write_all(&mut f, boot_line.as_bytes()))
    } else {
        std::fs::write(&boot_log, boot_line)
    };
    boot_res.map_err(|e| anyhow::anyhow!("写入 boot.log 失败: {e}"))?;

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

/// `--keep-var` 用：在 **dd 之前**读出现有的持久分区项与磁盘签名。
///
/// 认法：MBR 分区表里**类型为 `PART_TYPE_LINUX`（0x83）**的那一项就是我们的持久分区
/// （ISO 那一项的类型是 0x17／隐藏 ISO，本项目布局下两者不会混）。
/// 找不到就**报错中止**，绝不"悄悄建一块新的 /var" —— 那会让用户以为数据保住了。
///
/// 为什么连磁盘签名一起记：安装标记 `log/install-id` 里绑定了 `id=<签名>`，
/// 启动侧 `aether-init` 要求"分区所在盘的 MBR 签名 == 标记里的 id"。
/// 签名变了 ⇒ `/var` 挂不上 ⇒ 用户看到"装完系统，数据全没了"。
#[cfg(target_os = "linux")]
fn capture_persist(disk: &str) -> Result<(usize, MbrEntry, u32)> {
    use std::io::Read;
    let mut mbr = [0u8; 512];
    std::fs::File::open(disk)
        .with_context(|| format!("打开 {disk} 失败"))?
        .read_exact(&mut mbr)
        .context("读取 MBR 失败")?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        bail!("目标盘 {disk} 上没有 MBR 签名 —— 它看起来不是 AetherOS 装过的盘（--keep-var 需要一块装过的盘）");
    }
    for i in 0..4 {
        let base = 446 + i * 16;
        if mbr[base + 4] != PART_TYPE_LINUX {
            continue;
        }
        let lba_start = u32::from_le_bytes([mbr[base + 8], mbr[base + 9], mbr[base + 10], mbr[base + 11]]);
        let sectors = u32::from_le_bytes([mbr[base + 12], mbr[base + 13], mbr[base + 14], mbr[base + 15]]);
        if sectors == 0 {
            continue;
        }
        let entry = MbrEntry { bootable: false, part_type: PART_TYPE_LINUX, lba_start, sectors };
        let sig = u32::from_le_bytes([mbr[440], mbr[441], mbr[442], mbr[443]]);
        return Ok((i, entry, sig));
    }
    bail!("--keep-var 找不到现有持久分区（分区表里没有 0x83 项）—— 已中止，未对 {disk} 做任何写入")
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = parse_args(&args)?;
    let (yes, persist, wipe, keep_var) = (opt.yes, opt.persist, opt.wipe, opt.keep_var);

    // 先规范化（审计 M-7）：后面所有比较与写入都用规范化后的路径
    let disk = canonicalize_disk(&opt.disk)?;
    let mb = validate_disk(&disk)?;
    // 目标盘不得是安装源自己：--disk /dev/sr0 会变成 dd if=/dev/sr0 of=/dev/sr0 自读自写。
    // 用规范化后的路径比较（`same_device`），别再退回字符串比较。
    if same_device(&disk, SOURCE) {
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

    // `--keep-var`：**必须在 dd 之前**把现有持久分区项与磁盘签名记下来。
    // dd 会把 ISO 的 MBR 写进首扇区，分区表与签名**一起被覆盖** ——
    // 不先记住，就会得到"新系统 + 一块谁也不认领的旧 /var"。
    // 找不到现有持久分区就**中止**：绝不"悄悄建一块新的 /var"（那等于骗用户说数据保住了）。
    #[cfg(target_os = "linux")]
    let kept = if keep_var {
        let k = capture_persist(&disk)?;
        println!(
            "aether-install: --keep-var：记住现有持久分区（项 {} 起始扇区 {} 大小 {}MB，磁盘签名 {:08x}）",
            k.0,
            k.1.lba_start,
            k.1.sectors as u64 * 512 / 1024 / 1024,
            k.2
        );
        Some(k)
    } else {
        None
    };
    // 非 Linux（开发机）上 `capture_persist` 与 `setup_persist` 都不参与编译 ⇒
    // 显式吃掉这个变量（真正会装机的只有 Linux），否则本机 0 告警标准会被打破。
    #[cfg(not(target_os = "linux"))]
    let _ = keep_var; // 非 Linux：capture_persist/setup_persist 不参与编译 —— 显式吃掉，避免告警

    // 可选的整盘清零（审计 M-8）：默认不做，理由是时间成本 vs 威胁模型；
    // 需要把机器移交出去时用 --wipe 把旧数据真正抹掉。
    if wipe {
        println!("aether-install: --wipe：先整盘清零（{mb}MB，可能要几分钟）…");
        let st = Command::new("/bin/dd")
            .arg("if=/dev/zero")
            .arg(format!("of={disk}"))
            .arg("bs=4M")
            .arg("conv=fsync")
            .status()
            .context("执行整盘清零失败")?;
        if !st.success() {
            bail!("整盘清零失败: {st}");
        }
    }

    println!("aether-install: 写入中（约 32MB，几秒钟）…");
    let status = Command::new("/bin/dd")
        .arg(format!("if={SOURCE}"))
        .arg(format!("of={disk}"))
        .arg(format!("bs={BS}"))
        // conv=fsync：dd 返回前把数据落盘（审计 L-12）。不加的话"dd 成功"只代表
        // 数据进了页缓存，紧接着的分区重读/mkfs 可能与写盘竞争。
        .arg("conv=fsync")
        .status()
        .context("执行 /bin/dd 失败")?;
    if !status.success() {
        bail!("dd 写入失败: {status}");
    }

    if persist {
        #[cfg(target_os = "linux")]
        match setup_persist(&disk, kept) {
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

    /// 安装标记路径必须与 `aether-init::persist::INSTALL_MARKER` 一致。
    ///
    /// 为什么值得一条断言（代码审查发现）：这个路径此前在写入侧是**裸字面量**，
    /// 而它现在决定"重启后能不能认出持久分区"。两侧各钉一次字面量，
    /// 于是"只改一侧"必然让那一侧变红。
    #[test]
    fn install_marker_matches_init_convention() {
        assert_eq!(INSTALL_MARKER, "log/install-id");
        // 必须是相对路径且落在 log/ 下（识别侧按分区根拼接）
        assert!(!INSTALL_MARKER.starts_with('/'), "标记应是相对路径");
        assert!(INSTALL_MARKER.starts_with("log/"), "标记应在 log/ 下");
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
        let o = parse_args(&args(&["--disk", "/dev/vda", "--yes"])).unwrap();
        assert_eq!(o.disk, "/dev/vda");
        assert!(o.yes);
        assert!(o.persist);
        assert!(!o.wipe, "--wipe 默认必须是关的（整盘清零很慢）");
        let o2 = parse_args(&args(&["--yes", "--disk", "/dev/sda", "--no-persist"])).unwrap();
        assert!(o2.yes);
        assert!(!o2.persist);
        let o3 = parse_args(&args(&["--disk", "/dev/vdb", "--yes", "--wipe"])).unwrap();
        assert!(o3.wipe);
    }

    /// M-7 回归：目标路径必须被规范化，否则"拒绝自读自写"能被多余的斜杠绕过。
    ///
    /// 在开发机上用临时**普通文件**验证"规范化 + 拒非块设备"这条路径：
    /// 块设备分支要 Linux 才走得到，这里至少钉住"路径收敛"与"非块设备必须拒"。
    #[test]
    fn canonicalize_rejects_non_block_device_and_normalizes() {
        let dir = std::env::temp_dir().join("aether-install-canon");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("disk.img");
        std::fs::write(&f, b"x").unwrap();

        #[cfg(unix)]
        {
            // 普通文件不是块设备 → 必须拒
            assert!(canonicalize_disk(f.to_str().unwrap()).is_err());
        }

        // 带 `./` 与重复分隔符的写法必须收敛到同一路径（这正是绕过字符串比较的手法）
        let messy = format!("{}{}disk.img", dir.display(), std::path::MAIN_SEPARATOR);
        let messy2 = format!("{}{}.{}disk.img", dir.display(),
                             std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
        if std::fs::canonicalize(&messy).is_ok() && std::fs::canonicalize(&messy2).is_ok() {
            assert_eq!(std::fs::canonicalize(&messy).unwrap(),
                       std::fs::canonicalize(&messy2).unwrap(),
                       "不同写法必须收敛到同一路径");
        }

        // same_device：同一文件的不同写法必须判为同一设备
        assert!(same_device(
            f.to_str().unwrap(),
            std::fs::canonicalize(&f).unwrap().to_str().unwrap()
        ));
        // 不存在的路径退回字符串比较，不应误判为相同
        assert!(!same_device("/dev/definitely-not-here-a", "/dev/definitely-not-here-b"));
        let _ = std::fs::remove_dir_all(&dir);
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
    /// H-6 的**时机**回归（审计 D.2 第 17 条：此前只能靠人工审查）。
    ///
    /// 断言"三道门在源码里出现在 dd 之前"。为什么需要它：
    /// 这条修复的**价值全在顺序上** —— 同样的检查放在 dd 之后，目标盘已被覆盖、
    /// 分区表已毁才发现起不来，用户拿到的是一块毁掉又起不来的盘，
    /// 而程序还会报"安装完成"。后果太贵，不能只靠人记得。
    ///
    /// 用 `include_str!` 编译期嵌入源码扫描（不做运行期文件 IO）。
    /// 回滚成"先 dd 再校验"会让这条立刻变红 —— 这正是它存在的意义。
    #[test]
    fn destructive_write_happens_after_all_gates() {
        let src = include_str!("main.rs");
        // 安装流程在 `fn main()` 里（本 crate 没有 `fn install`）；
        // 作用域到测试模块之前为止 —— 这样扫描范围恰好是"会动目标盘的那段代码"。
        let start = src.find("fn main() -> Result<()> {").expect("找不到 main");
        let end = src[start..]
            .find("#[cfg(test)]")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];

        let dd = body
            .find("Command::new(\"/bin/dd\")")
            .expect("install 里应有 dd 调用（否则这条测试失去意义）");

        for (marker, what) in [
            ("if !yes", "确认门（--yes）"),
            ("source_has_mbr()", "isohybrid 引导前提校验"),
            ("same_device(", "目标盘不得是安装源"),
        ] {
            let at = body
                .find(marker)
                .unwrap_or_else(|| panic!("install 里找不到「{what}」（标记 {marker}）"));
            assert!(
                at < dd,
                "「{what}」必须出现在 dd **之前** —— 现在在其后：目标盘会被毁掉才发现问题"
            );
        }
    }

    /// 审计 #13：磁盘签名的读写与**非零**要求。
    #[test]
    fn disk_signature_is_written_at_the_standard_offset() {
        let mut mbr = isohybrid_mbr();
        // 偏移必须是 MBR 的标准磁盘签名字段 0x1B8
        assert_eq!(MBR_DISK_SIG_OFFSET, 440);
        set_disk_signature(&mut mbr, 0xDEAD_BEEF);
        assert_eq!(mbr[440], 0xEF, "小端：低字节在前");
        assert_eq!(mbr[441], 0xBE);
        assert_eq!(mbr[442], 0xAD);
        assert_eq!(mbr[443], 0xDE);
        assert_eq!(mbr_disk_signature(&mbr), 0xDEAD_BEEF);
        // 写签名不能破坏 MBR 引导签名
        assert_eq!(&mbr[510..512], &[0x55, 0xAA]);
    }

    /// 审计 #13：随机签名必须**非零**（0 是"未设置"的常见初值，混淆会让分区挂不上）。
    #[test]
    fn random_disk_signature_is_nonzero_and_varies() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..16 {
            let s = random_disk_signature();
            assert_ne!(s, 0, "磁盘签名不得为 0");
            seen.insert(s);
        }
        assert!(seen.len() > 1, "多次生成应至少出现不同值（否则不是随机）");
    }

    /// 审计 #13：标记文件的格式必须与 aether-init 的解析约定一致。
    ///
    /// 两侧各写一份格式是这类改动的典型故障点（一边改成 `ID=` 另一边还在找 `id=`）。
    /// 这里断言格式串本身；init 侧有对应的解析测试。
    #[test]
    fn install_marker_carries_the_identity_id() {
        let sig: u32 = 0x0A0B_0C0D;
        let content = format!("1700000000\nid={sig:08x}\n");
        assert!(content.contains("id=0a0b0c0d"), "必须是 8 位小写十六进制：{content}");
        assert!(content.starts_with("1700000000"), "第一行仍是时间戳（兼容旧格式）");
        // 解析回来必须与写入的一致（复刻 init 的解析规则，避免只测一半）
        let parsed = content
            .lines()
            .find_map(|l| l.trim().strip_prefix("id="))
            .and_then(|h| u32::from_str_radix(h, 16).ok());
        assert_eq!(parsed, Some(sig));
    }

}
