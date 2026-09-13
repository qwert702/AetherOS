//! aether-install — AetherOS 磁盘安装器（M6 v0.1）。
//!
//! 原理：ISO 是 isohybrid 镜像（rebuild 流程末尾经 host isohybrid 处理，
//! 带 MBR 签名与隐藏 ISO 分区表），直接 dd 到块设备即可 BIOS 引导，
//! 引导链与光盘完全一致，零额外引导器依赖。
//!
//! 流程：--disk 指定目标块设备 → 防呆校验（块设备/大小/显式 --yes）
//! → `dd if=/dev/sr0 of=<disk> bs=4M`（命令字面量，目标值仅作参数，
//!   无 shell 拼接）→ sync → 提示重启。
//!
//! 用法（在 guest 的 getty shell 里，root）：
//!   aether-install --disk /dev/vda --yes
//!
//! 限制（v0.1）：整盘覆盖写；安装后与光盘同体（只读 rootfs，无持久化），
//! 持久化分区与 rootfs 扩容在 v0.2。

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// 安装源：光驱设备（内核 CONFIG_BLK_DEV_SR=y，devtmpfs 自动出节点）。
pub const SOURCE: &str = "/dev/sr0";
/// dd 的块大小。
pub const BS: &str = "4M";
/// 目标盘最小体积（MB）：镜像 30MB，留一倍余量。
pub const MIN_DISK_MB: u64 = 64;

/// 解析命令行参数：(--disk, --yes)。未知参数报错。
pub fn parse_args(args: &[String]) -> Result<(String, bool)> {
    let mut disk = None;
    let mut yes = false;
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
            other => bail!("未知参数「{other}」（用法: aether-install --disk <块设备> --yes）"),
        }
        i += 1;
    }
    let disk = disk.ok_or_else(|| anyhow::anyhow!("必须用 --disk 指定目标块设备（如 /dev/vda）"))?;
    Ok((disk, yes))
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
    let mb = match std::fs::metadata(path) {
        Ok(m) if m.len() > 0 => m.len() / 1024 / 1024,
        _ => {
            // 块设备的 metadata().len() 恒为 0：真实大小从 sysfs 读（扇区数 ×512B）
            let name = disk.trim_start_matches("/dev/");
            let sectors = std::fs::read_to_string(format!("/sys/block/{name}/size"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .ok_or_else(|| anyhow::anyhow!("无法确定 {disk} 的大小（sysfs 无记录）"))?;
            sectors * 512 / 1024 / 1024
        }
    };
    if mb < MIN_DISK_MB {
        bail!("{disk} 只有 {mb}MB，小于最小要求 {MIN_DISK_MB}MB");
    }
    Ok(mb)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (disk, yes) = parse_args(&args)?;

    let mb = validate_disk(&disk)?;
    println!("aether-install: 目标 {disk}（{mb}MB），源 {SOURCE}");
    if !yes {
        bail!("将整盘覆盖写入 {disk} —— 确认无误后追加 --yes 执行");
    }

    println!("aether-install: 写入中（约 30MB，几秒钟）…");
    let status = Command::new("/bin/dd")
        .arg(format!("if={SOURCE}"))
        .arg(format!("of={disk}"))
        .arg(format!("bs={BS}"))
        .status()
        .context("执行 /bin/dd 失败")?;
    if !status.success() {
        bail!("dd 写入失败: {status}");
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

    #[test]
    fn parse_ok() {
        let (disk, yes) = parse_args(&args(&["--disk", "/dev/vda", "--yes"])).unwrap();
        assert_eq!(disk, "/dev/vda");
        assert!(yes);
        let (disk, yes) = parse_args(&args(&["--yes", "--disk", "/dev/sda"])).unwrap();
        assert_eq!(disk, "/dev/sda");
        assert!(yes);
    }

    #[test]
    fn parse_requires_disk() {
        assert!(parse_args(&args(&["--yes"])).is_err());
        assert!(parse_args(&args(&[])).is_err());
        // --disk 缺参数
        assert!(parse_args(&args(&["--disk"])).is_err());
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(parse_args(&args(&["--disk", "/dev/vda", "--evil"])).is_err());
    }
}
