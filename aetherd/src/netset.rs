//! 系统设置写入（2026-10-03，FEATURE-GAPS §2.4 网络写入）。
//!
//! ## 当前范围：只做 DNS
//!
//! `/etc/resolv.conf` 是**真实被消费**的路径：`aether-compositor/src/net.rs` 的网络页
//! 读它显示 DNS，libc 解析也读它 —— 写完立即生效。
//!
//! **静态 IP 不做**：仓库里网络地址由 busybox `udhcpc`（DHCP）拉起
//! （`aether-init/src/unit.rs`），没有 ifupdown。要让静态 IP 真正生效并持久，
//! 需要一个 aether-init 侧的开机消费者；只写一个**没人读**的配置文件，
//! 用户会以为"设置生效了"而实际没有 —— 那是假功能，比不写更坏。
//!
//! ## 这是"改系统配置"的接口，按最严处理
//!
//! * **纯函数校验**：只接受 1~3 个 IPv4 点分四段地址（含 IPv6 的明确拒绝）；
//! * **失败绝不碰文件**：校验不过就返回错误，不留半成品；
//! * **原子写入**：临时文件 → `fsync` → `rename`（同目录 rename 是原子的），
//!   避免解析器读到写了一半的文件；
//! * **拒绝覆盖符号链接**：`/etc/resolv.conf` 常被其它解析器做成符号链接，
//!   跟着写会改到我们没打算改的目标（与安全审计 H-4 同一条教训）；
//! * **备份只做一次**：保留**原系统文件**，而不是我们上一次写的内容。

use aether_ipc::{parse_dns_list, SettingKey};

/// DNS 配置的目标文件。
pub const RESOLV_CONF: &str = "/etc/resolv.conf";

/// 首次写入前的备份路径（保存**原系统文件**，之后不再覆盖它）。
pub const RESOLV_BACKUP: &str = "/etc/resolv.conf.aether-bak";

/// 写入文件的头部：**说清楚谁在管**，免得用户手改后被覆盖却不知原因。
const MANAGED_HEADER: &str =
    "# 本文件由 AetherOS 设置中心写入；首次写入前的原文件备份在 /etc/resolv.conf.aether-bak\n";

/// 渲染 `/etc/resolv.conf` 的内容（纯函数）。
pub fn render_resolv(servers: &[std::net::Ipv4Addr]) -> String {
    let mut s = String::from(MANAGED_HEADER);
    for ip in servers {
        s.push_str(&format!("nameserver {ip}\n"));
    }
    s
}

/// 原子写入：同目录临时文件 → `fsync` → `rename`。
///
/// 为什么不能直接 `fs::write`：那会先把文件截断，解析器在这中间读到的可能是**空文件**
/// 或残缺内容 —— 表现是"改完 DNS 后有一小段时间解析失败"，而且难以复现。
pub fn write_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = dir.join(format!(
        ".{}.aether-tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("resolv")
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // 目录项也要落盘：否则掉电后可能"文件内容在、名字不在"
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// 应用一项设置（**可注入路径**，便于在开发机上测；生产走 [`apply`]）。
///
/// 这是设置的**唯一写入口**：校验 → 拒绝符号链接 → 备份（仅首次）→ 原子写入。
/// 任一步失败都返回 `Err`，且**不会留下半成品**。
pub fn apply_to(
    key: SettingKey,
    value: &str,
    path: &std::path::Path,
    backup: &std::path::Path,
) -> Result<String, String> {
    match key {
        SettingKey::Dns => {
            // 1) 校验（纯函数，失败即返回，绝不碰文件）
            // `parse_dns_list` 现在返回 `DnsError`（协议层不决定文案）。
            // aetherd 拿不到界面语言，所以回一句**中文兜底**：正常路径下界面自己
            // 已经本地校验过（`settings.rs` 用同一份逻辑），这一句只在
            // "绕过界面直接发 IPC"时才会被看到。
            let servers = parse_dns_list(value)
                .map_err(|e| format!("DNS 设置被拒（{}）：{e}", e.as_str()))?;

            // 2) 拒绝覆盖符号链接：跟着写会改到别的目标
            if let Ok(md) = std::fs::symlink_metadata(path) {
                if md.file_type().is_symlink() {
                    return Err(format!(
                        "{} 是符号链接（可能由其它解析器管理），拒绝覆盖",
                        path.display()
                    ));
                }
                if !md.is_file() {
                    return Err(format!("{} 不是普通文件，拒绝覆盖", path.display()));
                }
                // 3) 备份**只做一次**：保留原系统文件，而不是我们上次写的内容
                if !backup.exists() {
                    std::fs::copy(path, backup)
                        .map_err(|e| format!("备份 {} 失败：{e}", path.display()))?;
                }
            }

            // 4) 原子写入
            let content = render_resolv(&servers);
            write_atomic(path, &content).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
            Ok(format!(
                "DNS 已设为 {}（写入 {}）",
                servers
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                path.display()
            ))
        }
    }
}

/// 生产入口：写真实的系统路径。
pub fn apply(key: SettingKey, value: &str) -> Result<String, String> {
    apply_to(
        key,
        value,
        std::path::Path::new(RESOLV_CONF),
        std::path::Path::new(RESOLV_BACKUP),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录（每个用例一个，避免相互干扰）。
    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-netset-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dns_list_accepts_only_well_formed_ipv4() {
        assert_eq!(parse_dns_list("1.1.1.1").unwrap().len(), 1);
        assert_eq!(parse_dns_list("1.1.1.1, 8.8.8.8").unwrap().len(), 2);
        // 空白分隔也要认（用户从别处粘贴常见）
        assert_eq!(parse_dns_list("1.1.1.1   8.8.8.8").unwrap().len(), 2);
        // 顺序必须保持（DNS 有优先级语义）
        let v = parse_dns_list("9.9.9.9, 1.1.1.1").unwrap();
        assert_eq!(v[0].to_string(), "9.9.9.9");
        assert_eq!(v[1].to_string(), "1.1.1.1");
    }

    /// 拒绝路径：每条都要能测到，且**理由是给人看的**（设置页原样显示）。
    #[test]
    fn dns_list_rejects_everything_else_with_a_reason() {
        let bad = [
            ("", "空"),
            ("   ", "空"),
            (",", "空"),
            ("1.1.1.1, 8.8.8.8, 9.9.9.9, 4.4.4.4", "超过 3 个"),
            ("::1", "IPv6"),
            ("2001:4860:4860::8888", "IPv6"),
            ("1.1.1", "段数不足"),
            ("1.1.1.1.1", "段数过多"),
            ("999.1.1.1", "越界"),
            ("01.2.3.4", "前导零（不同解析器解释不同，最危险的一类）"),
            ("1.1.1.1; reboot", "注入形状"),
            ("1.1.1.1\nnameserver 8.8.8.8", "换行注入"),
            ("127.0.0.1", "回环（上游 DNS 不接受回环）"),
            ("localhost", "域名不是地址"),
        ];
        for (input, why) in bad {
            let r = parse_dns_list(input);
            assert!(r.is_err(), "{input:?}（{why}）应被拒绝，却通过成 {r:?}");
            // parse_dns_list 现在返回 DnsError（协议层不决定文案）：
        // 这里用 Display 取中性描述，够这条测试校验理由不能是空话、不能含换行。
        let e = r.unwrap_err();
        assert!(!e.as_str().is_empty(), "机器名不能为空");
        let msg = e.to_string();
            assert!(!msg.is_empty(), "{input:?} 的拒绝理由不能是空话");
            // 拒绝理由里**不能回显原始输入**（它可能含控制字符/换行，直接进日志与界面）
            if input.contains('\n') {
                assert!(!msg.contains('\n'), "理由不得含换行：{msg:?}");
            }
        }
    }

    #[test]
    fn rendered_resolv_is_what_the_reader_expects() {
        let servers = parse_dns_list("1.1.1.1, 8.8.8.8").unwrap();
        let text = render_resolv(&servers);
        assert!(text.starts_with('#'), "应以注释头开头");
        assert!(text.contains("nameserver 1.1.1.1\n"));
        assert!(text.contains("nameserver 8.8.8.8\n"));
        // 顺序保持，且恰好两行 nameserver
        assert_eq!(text.matches("nameserver ").count(), 2);
        let first = text.find("nameserver 1.1.1.1").unwrap();
        let second = text.find("nameserver 8.8.8.8").unwrap();
        assert!(first < second, "顺序即优先级，不能被改动");
    }

    #[test]
    fn apply_writes_atomically_and_backs_up_the_original_only_once() {
        let d = tmpdir("apply");
        let target = d.join("resolv.conf");
        let backup = d.join("resolv.conf.aether-bak");
        std::fs::write(&target, "nameserver 10.0.2.3\n").unwrap();

        let msg = apply_to(SettingKey::Dns, "1.1.1.1", &target, &backup).unwrap();
        assert!(msg.contains("1.1.1.1"), "回执应说明设成了什么：{msg}");
        let now = std::fs::read_to_string(&target).unwrap();
        assert!(now.contains("nameserver 1.1.1.1"));
        // 备份保存的是**原系统文件**
        assert!(std::fs::read_to_string(&backup).unwrap().contains("10.0.2.3"));

        // 第二次写入：备份**不得**被我们上次写的内容覆盖
        apply_to(SettingKey::Dns, "8.8.8.8", &target, &backup).unwrap();
        let bk = std::fs::read_to_string(&backup).unwrap();
        assert!(bk.contains("10.0.2.3"), "备份被覆盖了：{bk}");
        assert!(!bk.contains("1.1.1.1"), "备份应是原文件，不是上次写入结果：{bk}");

        // 不留临时文件（原子写入的中间产物必须清理干净）
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("aether-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **失败不留半成品**：校验不过时，目标文件必须原封不动。
    #[test]
    fn invalid_value_leaves_the_file_untouched() {
        let d = tmpdir("keep");
        let target = d.join("resolv.conf");
        let backup = d.join("resolv.conf.aether-bak");
        let original = "nameserver 10.0.2.3\n";
        std::fs::write(&target, original).unwrap();

        for bad in ["", "999.1.1.1", "::1", "1.1.1.1; reboot"] {
            let r = apply_to(SettingKey::Dns, bad, &target, &backup);
            assert!(r.is_err(), "{bad:?} 应被拒绝");
        }
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            original,
            "校验失败时不得改动文件"
        );
        assert!(!backup.exists(), "校验失败时不该产生备份");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 拒绝覆盖符号链接（与安全审计 H-4 同一条教训：别跟着链接写）。
    #[cfg(unix)]
    #[test]
    fn refuses_to_overwrite_a_symlink() {
        let d = tmpdir("symlink");
        let real = d.join("real.conf");
        let link = d.join("resolv.conf");
        let backup = d.join("resolv.conf.aether-bak");
        std::fs::write(&real, "nameserver 10.0.2.3\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let r = apply_to(SettingKey::Dns, "1.1.1.1", &link, &backup);
        assert!(r.is_err(), "符号链接应被拒绝");
        assert!(r.unwrap_err().contains("符号链接"), "理由应说明是符号链接");
        // 链接指向的真实文件**不得**被改动
        assert!(std::fs::read_to_string(&real).unwrap().contains("10.0.2.3"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 目标是目录等非普通文件时也必须拒绝（否则 rename 会失败得莫名其妙）。
    #[test]
    fn refuses_when_target_is_not_a_regular_file() {
        let d = tmpdir("notfile");
        let target = d.join("resolv.conf");
        std::fs::create_dir_all(&target).unwrap();
        let backup = d.join("resolv.conf.aether-bak");
        let r = apply_to(SettingKey::Dns, "1.1.1.1", &target, &backup);
        assert!(r.is_err(), "目录应被拒绝");
        let _ = std::fs::remove_dir_all(&d);
    }
}
