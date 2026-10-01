//! 网络状态（P4.2 设置中心「网络」页）。
//!
//! ## 为什么不走 IPC
//!
//! 合成器已经是 root，并且**本来就直接读系统文件**：`/var/apps`（已装应用）、
//! `/etc/localtime` 或 `TZ`（时区，见 `text.rs`）。网络状态同样有**内核提供的真值**：
//! `/sys/class/net/*/{address,operstate}`、`/proc/net/route`、`/proc/net/fib_trie`、
//! `/etc/resolv.conf`。
//!
//! 所以这一页**读真值而不是造数据**，也不需要新增 IPC 变体
//! （那要同步 `aetherd/src/server.rs` 的鉴权分支与 `request_variants_are_gated` 清单）。
//!
//! ## 解析是纯函数
//!
//! 所有解析都接收**文本**而不是直接读文件（`parse_route`、`parse_fib_trie`、
//! `parse_resolv`），这样能在开发机上用样例内容单测 —— 不必真的联网或跑在 Linux 上。

// 解析函数只在 Linux 的 `read_linux()` 里被调用（非 Linux 目标 `read()` 直接返回空值），
// 但**单测在所有平台都会跑它们**（样例文本 → 期望值），所以按目标收敛而不是删掉。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// 一个网络接口的状态。
#[derive(Clone, PartialEq, Debug)]
pub struct Iface {
    /// 接口名（`eth0` / `enp0s3` …）
    pub name: String,
    /// MAC 地址（原样，大写规范化）
    pub mac: String,
    /// 链路是否 up（`/sys/class/net/*/operstate` == `up`）
    pub up: bool,
    /// IPv4 地址（无则空串）
    pub ipv4: String,
    /// 默认网关（无则空串）
    pub gateway: String,
}

/// 整机网络概况（设置页要显示的东西）。
#[derive(Clone, PartialEq, Debug, Default)]
pub struct NetInfo {
    /// 主接口（取第一个非 `lo` 且有 MAC 的）
    pub iface: Option<Iface>,
    /// DNS 服务器（`/etc/resolv.conf` 的 `nameserver` 行）
    pub dns: Vec<String>,
    /// 读不到任何东西时的说明（给界面显示，而不是假装"已连接"）
    pub note: String,
}

impl NetInfo {
    /// 一句话状态：**只说真值**（没有接口就说没有，不写"已连接"）。
    pub fn summary(&self) -> String {
        match &self.iface {
            None => "未发现网络接口".to_string(),
            Some(i) => {
                if i.ipv4.is_empty() {
                    format!("{} · {}", i.name, if i.up { "链路已连接" } else { "链路断开" })
                } else {
                    format!("{} · {}", i.name, i.ipv4)
                }
            }
        }
    }
}

/// 解析 `/proc/net/route`，取默认网关（Destination == 00000000 的那一行）。
///
/// 格式：`Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT`
/// 网关是**小端十六进制**，例如 `0102A8C0` → `192.168.2.1`。
pub fn parse_route(text: &str) -> Option<String> {
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        // 默认路由：Destination 全 0
        if f[1] != "00000000" {
            continue;
        }
        let hex = f[2];
        if hex.len() != 8 {
            continue;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        // 内核给的是网络字节序的小端表示：按字节反着取
        let b = v.to_le_bytes();
        return Some(format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]));
    }
    None
}

/// 解析 `/proc/net/fib_trie`，取本机 IPv4（LOCAL 段里的第一个可用地址）。
///
/// 只认 `|-- x.x.x.x` 且**排除**回环与 0.0.0.0；这个文件是内核的内部转储，
/// 所以宁可"取不到就返回 None"，也不猜。
pub fn parse_fib_trie(text: &str) -> Option<String> {
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim();
        // LOCAL 段（本机地址）之后才看
        if !t.starts_with("|-- ") {
            continue;
        }
        let addr = t.trim_start_matches("|-- ").trim();
        if !is_ipv4(addr) || addr.starts_with("127.") || addr == "0.0.0.0" {
            continue;
        }
        // 紧跟的 `/32 host LOCAL` 才算本机地址（其它是路由表项）
        if let Some(next) = lines.peek() {
            if next.contains("host LOCAL") {
                return Some(addr.to_string());
            }
        }
    }
    None
}

/// 解析 `/etc/resolv.conf` 的 `nameserver` 行（去重、保序）。
pub fn parse_resolv(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let mut f = t.split_whitespace();
        if f.next() == Some("nameserver") {
            if let Some(a) = f.next() {
                if is_ipv4(a) && !out.iter().any(|x| x == a) {
                    out.push(a.to_string());
                }
            }
        }
    }
    out
}

/// 极简 IPv4 形状校验（四段 0-255）。
pub fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()) && p.parse::<u8>().is_ok())
}

/// 从各接口目录名里挑主接口：跳过 `lo`，取第一个。
pub fn pick_iface(names: &[String]) -> Option<String> {
    names.iter().find(|n| *n != "lo").cloned()
}

/// 读真实系统状态（Linux）。非 Linux 目标返回带说明的空值。
pub fn read() -> NetInfo {
    #[cfg(target_os = "linux")]
    {
        read_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        NetInfo { iface: None, dns: Vec::new(), note: "非 Linux 目标：无网络接口信息".to_string() }
    }
}

#[cfg(target_os = "linux")]
fn read_linux() -> NetInfo {
    use std::fs;
    let mut names: Vec<String> = fs::read_dir("/sys/class/net")
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let dns = fs::read_to_string("/etc/resolv.conf").map(|t| parse_resolv(&t)).unwrap_or_default();
    let gateway = fs::read_to_string("/proc/net/route")
        .ok()
        .and_then(|t| parse_route(&t))
        .unwrap_or_default();
    let ipv4 = fs::read_to_string("/proc/net/fib_trie")
        .ok()
        .and_then(|t| parse_fib_trie(&t))
        .unwrap_or_default();
    let iface = pick_iface(&names).map(|name| {
        let mac = fs::read_to_string(format!("/sys/class/net/{name}/address"))
            .map(|s| s.trim().to_ascii_uppercase())
            .unwrap_or_default();
        let up = fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
            .map(|s| s.trim() == "up")
            .unwrap_or(false);
        Iface { name, mac, up, ipv4, gateway }
    });
    let note = if iface.is_none() {
        "未发现网络接口（/sys/class/net 为空）".to_string()
    } else {
        String::new()
    };
    NetInfo { iface, dns, note }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网关：内核给的是小端十六进制，必须按字节反着读（写错了就是"看起来像个 IP"的错值）。
    #[test]
    fn route_gateway_is_little_endian() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                 eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_route(t).as_deref(), Some("192.168.2.1"));
    }

    #[test]
    fn route_without_default_is_none() {
        let t = "Iface\tDestination\tGateway\neth0\t0002A8C0\t00000000\n";
        assert_eq!(parse_route(t), None);
    }

    /// 本机 IP：只认 `host LOCAL` 那一段，且排除回环。
    #[test]
    fn fib_trie_takes_local_non_loopback() {
        let t = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n\
                 Local:\n  +-- 127.0.0.0/8 2 0 2\n     |-- 127.0.0.1\n        /32 host LOCAL\n\
                 \x20 +-- 192.168.2.10/32 2 0 2\n     |-- 192.168.2.10\n        /32 host LOCAL\n";
        assert_eq!(parse_fib_trie(t).as_deref(), Some("192.168.2.10"));
    }

    #[test]
    fn resolv_parses_and_dedups() {
        let t = "# comment\nnameserver 10.0.2.3\nnameserver 10.0.2.3\nsearch lan\nnameserver 不是IP\nnameserver 8.8.8.8\n";
        assert_eq!(parse_resolv(t), vec!["10.0.2.3".to_string(), "8.8.8.8".to_string()]);
    }

    #[test]
    fn iface_pick_skips_loopback() {
        let names = vec!["lo".to_string(), "eth0".to_string(), "wlan0".to_string()];
        assert_eq!(pick_iface(&names).as_deref(), Some("eth0"));
        assert_eq!(pick_iface(&["lo".to_string()]), None);
    }

    #[test]
    fn summary_never_claims_connected_when_no_iface() {
        let n = NetInfo::default();
        assert_eq!(n.summary(), "未发现网络接口");
        // 有接口但没拿到 IP：不许写"已连接"
        let n = NetInfo {
            iface: Some(Iface {
                name: "eth0".into(),
                mac: "52:54:00:12:34:56".into(),
                up: false,
                ipv4: String::new(),
                gateway: String::new(),
            }),
            dns: Vec::new(),
            note: String::new(),
        };
        assert_eq!(n.summary(), "eth0 · 链路断开");
        assert!(!n.summary().contains("已连接"));
    }
}
