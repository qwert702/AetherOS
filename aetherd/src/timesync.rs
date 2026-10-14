//! 时间同步与时区写入（FEATURE-GAPS §3.3）。
//!
//! 当前时钟只是"本地偏移"（`settings.rs::tz_offset_min`），两件事都不成立：
//! **时间会漂**（没有对时），**时区改了系统时间没变**（只影响合成器显示）。
//! 本模块负责服务端那半：SNTP 对时 + 把时区写进系统。
//!
//! ## SNTP 报文：几处必须当错误的地方
//!
//! * **stratum 0 = kiss-o'-death**：服务器在说"别问了"。它的时间戳是无意义的，
//!   若照单收下，会把 1900 年（NTP 纪元起点）当成"现在"写进系统时钟 ——
//!   那是比不同步严重得多的故障。
//! * **LI = 3（alarm condition）**：服务器自己都没同步，它的时间不可信。
//! * **模式不是 4/5**：对方回了请求包或别的东西，说明这个响应不该被采信。
//! * 长度不足 48 字节：直接拒绝，绝不去读越界的字节。
//!
//! ## 时区名：**它是路径的一部分**
//!
//! `name` 会被拼成 `/usr/share/zoneinfo/<name>`，所以必须校验：
//! 拒绝 `..`、绝对路径、反斜杠、空串，并且写之前用 `canonicalize` 再确认
//! 解析出来的真实路径**仍在 zoneinfo 目录内**（防符号链接逃逸）。
//! 这不是洁癖 —— 一个能被写成任意路径的功能，等于给了一个"任意文件写"。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 系统时区文件（glibc 读它）。
pub const LOCALTIME: &str = "/etc/localtime";
/// 发行版约定的时区名文件。
pub const TIMEZONE_FILE: &str = "/etc/timezone";
/// 时区数据库目录。
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// NTP 纪元（1900-01-01）与 Unix 纪元（1970-01-01）之间的秒数。
pub const NTP_UNIX_EPOCH_DELTA: i64 = 2_208_988_800;

/// SNTP 报文长度。
pub const PACKET_LEN: usize = 48;

/// 构造 SNTP 请求（LI=0, VN=4, Mode=3 = client）。
pub fn build_request() -> [u8; PACKET_LEN] {
    let mut b = [0u8; PACKET_LEN];
    b[0] = 0b00_100_011; // LI=0, VN=4, Mode=3
    b
}

/// 从响应里解出的时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtpTime {
    /// Unix 秒（UTC）。
    pub unix_secs: i64,
    /// 小数部分的纳秒（0..1_000_000_000）。
    pub nanos: u32,
}

/// 解析 SNTP 响应。**任何可疑之处都返回 Err**（宁可不同步，也不写错时间）。
pub fn parse_response(buf: &[u8]) -> Result<NtpTime, String> {
    if buf.len() < PACKET_LEN {
        return Err(format!("响应太短（{} 字节，至少 {}）", buf.len(), PACKET_LEN));
    }
    let li = buf[0] >> 6;
    let vn = (buf[0] >> 3) & 0b111;
    let mode = buf[0] & 0b111;
    let stratum = buf[1];
    if li == 3 {
        return Err("服务器报告未同步（LI=3，alarm condition）—— 它的时间不可信".into());
    }
    if mode != 4 && mode != 5 {
        return Err(format!("模式 {mode} 不是服务器响应（应为 4 或 5）"));
    }
    if vn < 3 || vn > 4 {
        return Err(format!("版本 {vn} 不认识"));
    }
    // **kiss-o'-death**：stratum 0 的时间戳毫无意义，必须当错误
    if stratum == 0 {
        return Err("服务器要求停止请求（stratum 0 / kiss-o'-death）".into());
    }
    if stratum > 15 {
        return Err(format!("stratum {stratum} 越界（1..=15）"));
    }
    // 发送时间戳在偏移 40..48
    let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as i64;
    let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]);
    // 全零时间戳 = 服务器没填（同样不可采信）
    if secs == 0 && frac == 0 {
        return Err("发送时间戳为空".into());
    }
    let unix_secs = secs - NTP_UNIX_EPOCH_DELTA;
    // 分数转纳秒：frac / 2^32 秒
    let nanos = ((frac as u64 * 1_000_000_000) >> 32) as u32;
    Ok(NtpTime { unix_secs, nanos })
}

/// 时间偏移的处置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustment {
    /// 已经准了，不用动（阈值内）
    None,
    /// 小幅偏移：**平滑**调整（`adjtime`），不跳变
    Slew,
    /// 大幅偏移：直接设置（`clock_settime`）
    Step,
    /// **拒绝**：偏移超出合理性上限，**没有动时钟**。
    ///
    /// 与"调整失败"是两件事：这是"我不接受这个偏移"。
    /// 独立成一个结论，是为了让界面能说清"是拒绝，不是出错"。
    Refused,
}

/// 偏移超过这个值就"跳"而不是"平滑"。
///
/// 1 秒是常见选择：小于它时跳变会让 `sleep`/超时算错、日志时间戳倒退，
/// 而平滑调整能在几分钟内悄悄校正；大于它时慢慢来没意义（用户已经看得见钟不对了）。
pub const STEP_THRESHOLD_MS: i64 = 1000;

/// 偏移量的**合理性上限**（毫秒）：24 小时。
///
/// 为什么必须有这条：`aetherd` 以 root 运行，而 SNTP 走 UDP（可伪造）。
/// 没有上限时，一个伪造回复就能让系统时钟跳到任意年代 ——
/// 打断 TLS 证书有效期、日志顺序、会话超时、审计时间线。
/// 真正差出一天以上的情况（新装机、主板电池没电）应当由**用户显式**处理，
/// 不该由一次网络回复替用户决定。
pub const MAX_OFFSET_MS: i64 = 24 * 60 * 60 * 1000;

/// 偏移量是否在合理范围内（**纯函数**，任何平台都能测）。
///
/// 用 `unsigned_abs()` 而不是 `abs()`：`i64::MIN.abs()` 在 debug 下会 panic，
/// release 下回绕成负数 —— 一个来自网络的值不该有这种"看构建类型"的行为。
pub fn offset_is_sane(offset_ms: i64) -> bool {
    offset_ms.unsigned_abs() <= MAX_OFFSET_MS as u64
}

/// 根据偏移（毫秒）决定怎么调。
///
/// **只看"怎么调"，不管"该不该调"** —— 合理性上限在 [`apply_offset`] 里判，
/// 因为那是"要不要动系统时钟"的决定，而不是分类问题。
pub fn adjustment_for(offset_ms: i64) -> Adjustment {
    let mag = offset_ms.unsigned_abs();
    // 阈值内不动：反复微调只会让时钟一直在动
    if mag < 100 {
        Adjustment::None
    } else if mag < STEP_THRESHOLD_MS as u64 {
        Adjustment::Slew
    } else {
        Adjustment::Step
    }
}

/// 时区名是否合法（**它会被拼进路径**）。
///
/// 规则：非空、长度有上限、不以 `/` 开头或结尾、不含 `..`、不含反斜杠、
/// 字符集限于 `[A-Za-z0-9_+/-]`。
pub fn valid_tz_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("..") {
        return false;
    }
    if name.contains('\\') || name.contains('\0') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-' || c == '/')
}

/// 把时区写进系统（真实路径）。
pub fn set_timezone(name: &str) -> std::io::Result<PathBuf> {
    set_timezone_with(Path::new(ZONEINFO), Path::new(LOCALTIME), Path::new(TIMEZONE_FILE), name)
}

/// 可测版本：三个路径都可传（测试用临时目录）。
///
/// 写入顺序刻意如此：**先写 `/etc/localtime`（真正生效的那个），再写名字文件**。
/// 反过来的话，中途失败会留下"名字说东京、实际还是 UTC"的不一致状态。
pub fn set_timezone_with(
    zoneinfo: &Path,
    localtime: &Path,
    timezone_file: &Path,
    name: &str,
) -> std::io::Result<PathBuf> {
    if !valid_tz_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("时区名不合法：{name:?}"),
        ));
    }
    let candidate = zoneinfo.join(name);
    // **符号链接逃逸守卫**：解析真实路径后必须仍在 zoneinfo 之内
    let real = candidate.canonicalize()?;
    let root = zoneinfo
        .canonicalize()
        .unwrap_or_else(|_| zoneinfo.to_path_buf());
    if !real.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("时区文件逃出了 {root:?}（可疑链接）"),
        ));
    }
    if !real.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("没有这个时区：{name}"),
        ));
    }
    let bytes = std::fs::read(&real)?;
    // 原子替换：临时文件 → rename（半截的 localtime 会让所有程序拿到坏时区）
    if let Some(parent) = localtime.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = localtime.with_extension("aether-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, localtime)?;
    // 再写"名字"文件（0600 不需要：它不是秘密；0644 是这类文件的惯例）
    if let Some(parent) = timezone_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(timezone_file, format!("{name}\n"))?;
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个"看起来正常"的 SNTP 响应，发送时间戳取给定 Unix 秒。
    fn response(unix_secs: i64, frac: u32) -> Vec<u8> {
        let mut b = vec![0u8; PACKET_LEN];
        b[0] = 0b00_100_100; // LI=0, VN=4, Mode=4（服务器）
        b[1] = 2; // stratum 2
        let ntp = (unix_secs + NTP_UNIX_EPOCH_DELTA) as u32;
        b[40..44].copy_from_slice(&ntp.to_be_bytes());
        b[44..48].copy_from_slice(&frac.to_be_bytes());
        b
    }

    #[test]
    fn request_is_a_minimal_client_packet() {
        let r = build_request();
        assert_eq!(r.len(), PACKET_LEN);
        assert_eq!(r[0], 0b00_100_011, "LI=0 VN=4 Mode=3");
        assert!(r[1..].iter().all(|b| *b == 0), "其余字段留空由服务器填");
    }

    #[test]
    fn parses_a_normal_response_across_the_epoch_boundary() {
        // 2026-10-03T00:00:00Z
        let t = parse_response(&response(1_791_619_200, 0)).expect("应当解析成功");
        assert_eq!(t.unix_secs, 1_791_619_200);
        assert_eq!(t.nanos, 0);
        // 小数部分：0x80000000 = 半个 NTP 秒 = 500ms
        let half = parse_response(&response(1_791_619_200, 0x8000_0000)).unwrap();
        assert_eq!(half.unix_secs, 1_791_619_200);
        assert!((half.nanos as i64 - 500_000_000).abs() < 1000, "应为 500ms，实得 {}", half.nanos);
        // 纪元换算：NTP 纪元比 Unix 纪元早 2208988800 秒，**helper 已经做过换算**，
        // 所以这里验"往返一致"；1970 年前的时刻（负 Unix 秒）也要能算对。
        let before_epoch = -NTP_UNIX_EPOCH_DELTA + 1;
        let old = parse_response(&response(before_epoch, 0)).unwrap();
        assert_eq!(old.unix_secs, before_epoch, "纪元换算要能往返");
        // NTP 时间戳恰为 1（1900-01-01 00:00:01）→ Unix 秒 = 1 - 2208988800
        let mut raw = vec![0u8; PACKET_LEN];
        raw[0] = 0b00_100_100;
        raw[1] = 2;
        raw[40..44].copy_from_slice(&1u32.to_be_bytes());
        assert_eq!(parse_response(&raw).unwrap().unix_secs, 1 - NTP_UNIX_EPOCH_DELTA);
    }

    /// 这几类响应**必须拒绝**，否则会把 1900 年写进系统时钟。
    #[test]
    fn rejects_the_responses_that_must_never_be_trusted() {
        // 太短
        assert!(parse_response(&[0u8; 20]).is_err());
        assert!(parse_response(&[]).is_err());
        // kiss-o'-death（stratum 0）：时间戳无意义
        let mut kiss = response(1_791_619_200, 0);
        kiss[1] = 0;
        let e = parse_response(&kiss).unwrap_err();
        assert!(e.contains("kiss") || e.contains("stratum 0"), "理由要说清楚：{e}");
        // LI=3：服务器自己没同步
        let mut unsync = response(1_791_619_200, 0);
        unsync[0] = 0b11_100_100;
        assert!(parse_response(&unsync).is_err());
        // 模式不对（回的是客户端请求）
        let mut wrong_mode = response(1_791_619_200, 0);
        wrong_mode[0] = 0b00_100_011;
        assert!(parse_response(&wrong_mode).is_err());
        // stratum 越界
        let mut bad_stratum = response(1_791_619_200, 0);
        bad_stratum[1] = 16;
        assert!(parse_response(&bad_stratum).is_err());
        // 空时间戳（NTP 秒与分数都是 0）—— 必须手工构造：helper 会加纪元偏移，
        // 用 `response(0, 0)` 构造出来的是"1900 年"而不是"空"
        let mut empty = vec![0u8; PACKET_LEN];
        empty[0] = 0b00_100_100;
        empty[1] = 2;
        let e2 = parse_response(&empty).unwrap_err();
        assert!(e2.contains("空"), "理由要说清楚：{e2}");
        // 版本不认识
        let mut bad_vn = response(1_791_619_200, 0);
        bad_vn[0] = 0b00_010_100;
        assert!(parse_response(&bad_vn).is_err());
    }

    #[test]
    fn adjustment_policy_avoids_useless_jitter_and_slow_catchup() {
        // 阈值内：不动（反复微调会让时钟一直在动）
        assert_eq!(adjustment_for(0), Adjustment::None);
        assert_eq!(adjustment_for(99), Adjustment::None);
        assert_eq!(adjustment_for(-99), Adjustment::None);
        // 小幅：平滑（不跳变，sleep/超时不会算错）
        assert_eq!(adjustment_for(100), Adjustment::Slew);
        assert_eq!(adjustment_for(-999), Adjustment::Slew);
        // 大幅：直接设置（慢慢来没意义，用户已经看得见钟不对）
        assert_eq!(adjustment_for(1000), Adjustment::Step);
        assert_eq!(adjustment_for(-60_000), Adjustment::Step);
    }

    /// **路径穿越守卫**：时区名是路径的一部分。
    #[test]
    fn timezone_names_that_escape_are_rejected() {
        for bad in [
            "",
            "/etc/passwd",
            "../../etc/shadow",
            "Asia/../../etc/passwd",
            "Asia/Shanghai/",
            "Asia\\Shanghai",
            "Asia/Shang hai",
            "Asia/Shanghai\0x",
            &"a".repeat(65),
        ] {
            assert!(!valid_tz_name(bad), "不该接受 {bad:?}");
            // 并且真的写不进去（不是"只靠校验函数"）
            let dir = std::env::temp_dir();
            assert!(
                set_timezone_with(&dir, &dir.join("lt-guard"), &dir.join("tz-guard"), bad).is_err(),
                "非法名字必须被拒绝：{bad:?}"
            );
        }
        for ok in ["UTC", "Asia/Shanghai", "America/New_York", "Etc/GMT+8", "Europe/Paris"] {
            assert!(valid_tz_name(ok), "{ok} 应当合法");
        }
    }

    #[test]
    fn writes_localtime_and_the_name_file() {
        let root = std::env::temp_dir().join(format!(
            "aether-tz-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let zoneinfo = root.join("zoneinfo");
        std::fs::create_dir_all(zoneinfo.join("Asia")).unwrap();
        // 造一个"时区文件"（内容随便，验的是搬运与顺序）
        std::fs::write(zoneinfo.join("Asia/Shanghai"), b"TZFILE-DUMMY").unwrap();
        let localtime = root.join("etc/localtime");
        let tzfile = root.join("etc/timezone");

        let real = set_timezone_with(&zoneinfo, &localtime, &tzfile, "Asia/Shanghai").unwrap();
        assert!(real.ends_with("Shanghai"));
        assert_eq!(std::fs::read(&localtime).unwrap(), b"TZFILE-DUMMY", "localtime 要写成时区内容");
        let name = std::fs::read_to_string(&tzfile).unwrap();
        assert_eq!(name.trim(), "Asia/Shanghai");
        assert!(name.ends_with('\n'), "名字文件要有换行（很多工具按行读）");
        // 临时文件不该留下
        assert!(!localtime.with_extension("aether-tmp").exists(), "临时文件必须被 rename 掉");
        // 不存在的时区
        assert!(set_timezone_with(&zoneinfo, &localtime, &tzfile, "Mars/Olympus").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **符号链接逃逸**：zoneinfo 里一个指向外部的链接必须被拒绝。
    #[test]
    #[cfg(unix)]
    fn a_symlink_pointing_outside_zoneinfo_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "aether-tzlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let zoneinfo = root.join("zoneinfo");
        std::fs::create_dir_all(&zoneinfo).unwrap();
        let secret = root.join("secret");
        std::fs::write(&secret, b"SECRET").unwrap();
        std::os::unix::fs::symlink(&secret, zoneinfo.join("Evil")).unwrap();

        let r = set_timezone_with(
            &zoneinfo,
            &root.join("etc/localtime"),
            &root.join("etc/timezone"),
            "Evil",
        );
        assert!(r.is_err(), "指向 zoneinfo 之外的链接必须拒绝");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// 默认 NTP 服务器。
///
/// 用池化域名而不是某个具体地址：单台服务器挂了不该让对时功能整体失效。
/// 可用环境变量 `AETHER_NTP_SERVER` 覆盖（离线机房/测试要指向自己的服务器）。
pub const DEFAULT_SERVER: &str = "pool.ntp.org:123";

/// 查询一次时间（真实网络）。
pub fn query() -> Result<NtpTime, String> {
    let server = std::env::var("AETHER_NTP_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());
    query_with(&server, std::time::Duration::from_secs(3))
}

/// 可测版本：服务器地址与超时都可传（测试用本地回环，不碰外网）。
pub fn query_with(server: &str, timeout: std::time::Duration) -> Result<NtpTime, String> {
    use std::net::UdpSocket;
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("绑定本地端口失败：{e}"))?;
    sock.set_read_timeout(Some(timeout)).ok();
    let req = build_request();
    // connect + send：这样 send 会做路由检查，也能挡住"忘了 connect 就 read"的错
    sock.connect(server).map_err(|e| format!("连接 {server} 失败：{e}"))?;
    sock.send(&req).map_err(|e| format!("发送请求失败：{e}"))?;
    let mut buf = [0u8; 256];
    let n = sock.recv(&mut buf).map_err(|e| format!("等待响应失败（{server}）：{e}"))?;
    parse_response(&buf[..n])
}

/// 当前系统时间（Unix 秒 + 纳秒），用于算偏移。
pub fn now_unix() -> (i64, u32) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        // 系统时间早于 1970（时钟被设坏了）：如实返回负值，不要假装成 0
        Err(e) => {
            let d = e.duration();
            (-(d.as_secs() as i64), d.subsec_nanos())
        }
    }
}

/// 把系统时钟调整 `offset_ms` 毫秒。
///
/// 小幅用**平滑**（`adjtime`），大幅才**跳变**（`clock_settime`）——
/// 跳变会让 `sleep`/超时算错、日志时间戳倒退，能避免就避免。
pub fn apply_offset(offset_ms: i64) -> Result<Adjustment, String> {
    // **先判合理性，再决定动不动时钟**：不合理的偏移直接拒绝，一个字节都不碰。
    if !offset_is_sane(offset_ms) {
        return Ok(Adjustment::Refused);
    }
    let how = adjustment_for(offset_ms);
    if how == Adjustment::None {
        return Ok(how);
    }
    apply_offset_platform(offset_ms, how)?;
    Ok(how)
}

#[cfg(unix)]
fn apply_offset_platform(offset_ms: i64, how: Adjustment) -> Result<(), String> {
    // 安全：这些调用只碰时间，不做指针算术之外的任何解析
    unsafe {
        match how {
            Adjustment::Step => {
                let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                // 先读当前时间，再加上偏移，然后设置
                if libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) != 0 {
                    return Err(format!("读取系统时钟失败：{}", std::io::Error::last_os_error()));
                }
                let total_ns = ts.tv_sec as i128 * 1_000_000_000 + ts.tv_nsec as i128
                    + offset_ms as i128 * 1_000_000;
                let mut next = libc::timespec {
                    // 用 `as _` 让字段自身的类型说话：写 `libc::time_t` 会触发
                    // musl 目标下的 deprecated 警告（libc 该别名在 musl 1.2 起改 64 位，
                    // 上游要求别再直接用这个名字），而写死 `i64` 又会把 32 位平台写死。
                    tv_sec: (total_ns.div_euclid(1_000_000_000)) as _,
                    tv_nsec: (total_ns.rem_euclid(1_000_000_000)) as i64,
                };
                if libc::clock_settime(libc::CLOCK_REALTIME, &next) != 0 {
                    return Err(format!(
                        "设置系统时钟失败（需要 root/CAP_SYS_TIME）：{}",
                        std::io::Error::last_os_error()
                    ));
                }
                // `next` 只用于调用，读一次避免 unused_mut 误报
                let _ = &mut next;
                Ok(())
            }
            Adjustment::Slew => {
                let delta = libc::timeval {
                    tv_sec: offset_ms.div_euclid(1000),
                    tv_usec: (offset_ms.rem_euclid(1000)) * 1000,
                };
                if libc::adjtime(&delta, std::ptr::null_mut()) != 0 {
                    return Err(format!(
                        "平滑调整失败（需要 root/CAP_SYS_TIME）：{}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(())
            }
            Adjustment::None => Ok(()),
            // `None`（已在阈值内）与 `Refused`（偏移不合理、被拒绝）都有一个共同语义：
            // **一个字节都不碰时钟**。调用方 `apply_offset` 已经在前面把两者拦掉了，
            // 正常路径下走不到这里；但穷尽匹配必须写出来 —— 而且语义上它就该是"什么都不做"，
            // 不是伪造成"调整成功"，也不是伪装成"出错"（`Refused` 的文档特意强调：
            // 那是"我不接受这个偏移"，不是"调整失败"）。
            Adjustment::Refused => Ok(()),
        }
    }
}

#[cfg(not(unix))]
fn apply_offset_platform(_offset_ms: i64, _how: Adjustment) -> Result<(), String> {
    // 非 Unix 平台没有这套接口。**如实说不支持**，而不是假装调过了 ——
    // 假装成功会让"时间没对上"变成一个查不出来的谜。
    Err("本平台不支持调整系统时钟".into())
}

#[cfg(test)]
mod query_tests {
    use super::*;

    /// **回环集成测试**：真的发一个包、真的收一个包（不碰外网）。
    ///
    /// 这条覆盖的是 `query_with` 的网络路径 —— 报文解析另有单测，
    /// 但"发出去的请求长什么样、收到的响应有没有被正确切到 48 字节"只有真的跑一遍才知道。
    #[test]
    fn query_round_trips_against_a_local_server() {
        use std::net::UdpSocket;
        let server = UdpSocket::bind("127.0.0.1:0").expect("绑定回环端口");
        let addr = server.local_addr().unwrap().to_string();
        let expected = 1_791_619_200i64;
        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let (n, peer) = server.recv_from(&mut buf).expect("应收到请求");
            // 请求的形状：48 字节、模式 3（客户端）
            assert_eq!(n, PACKET_LEN);
            assert_eq!(buf[0] & 0b111, 3, "必须是客户端请求");
            // 回一个正常响应
            let mut r = vec![0u8; PACKET_LEN];
            r[0] = 0b00_100_100;
            r[1] = 2;
            let ntp = (expected + NTP_UNIX_EPOCH_DELTA) as u32;
            r[40..44].copy_from_slice(&ntp.to_be_bytes());
            server.send_to(&r, peer).expect("应能回包");
        });
        let got = query_with(&addr, std::time::Duration::from_secs(3)).expect("应当拿到时间");
        assert_eq!(got.unix_secs, expected);
        handle.join().unwrap();
    }

    /// 服务器不回 → **超时并报错**（不能无限等，也不能返回假时间）。
    #[test]
    fn query_times_out_instead_of_hanging() {
        use std::net::UdpSocket;
        let dead = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = dead.local_addr().unwrap().to_string();
        let t0 = std::time::Instant::now();
        // 用短超时，避免测试拖时间
        let r = query_with(&addr, std::time::Duration::from_millis(200));
        assert!(r.is_err(), "没人回就该报错");
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "不该等太久");
    }

    /// 服务器回了**垃圾** → 报错（而不是把垃圾当时间）。
    #[test]
    fn query_rejects_a_garbage_response() {
        use std::net::UdpSocket;
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let (_, peer) = server.recv_from(&mut buf).unwrap();
            server.send_to(b"not an ntp packet", peer).unwrap();
        });
        assert!(query_with(&addr, std::time::Duration::from_secs(3)).is_err());
        h.join().unwrap();
    }

    #[test]
    fn now_unix_is_sane_and_offset_policy_is_reachable() {
        let (secs, nanos) = now_unix();
        assert!(secs > 1_600_000_000, "系统时间应当不小于 2020 年：{secs}");
        assert!(nanos < 1_000_000_000);
        // 「在阈值内不动」这条**不碰系统时钟**，所以能安全断言
        assert_eq!(apply_offset(0).unwrap(), Adjustment::None);
        assert_eq!(apply_offset(50).unwrap(), Adjustment::None);
    }
    /// **审计发现（附录 M）**：偏移量的合理性上限 —— 纯函数，边界逐个验。
    #[test]
    fn offset_sanity_boundary() {
        assert!(offset_is_sane(0));
        assert!(offset_is_sane(500));
        assert!(offset_is_sane(MAX_OFFSET_MS), "正好 24 小时应当允许");
        assert!(offset_is_sane(-MAX_OFFSET_MS));
        assert!(!offset_is_sane(MAX_OFFSET_MS + 1), "超过 24 小时必须拒绝");
        assert!(!offset_is_sane(-(MAX_OFFSET_MS + 1)));
        // 极端值：**不许 panic**（`abs()` 在 i64::MIN 上会 panic —— 这正是改 unsigned_abs 的原因）
        assert!(!offset_is_sane(i64::MIN));
        assert!(!offset_is_sane(i64::MAX));
        assert_eq!(adjustment_for(i64::MIN), Adjustment::Step, "分类本身不该 panic");
    }

    /// 正常量级仍然按原规则分类（别把上限加进来时改坏了平滑/跳变的判断）。
    #[test]
    fn normal_offsets_still_classify_as_before() {
        assert_eq!(adjustment_for(0), Adjustment::None);
        assert_eq!(adjustment_for(99), Adjustment::None);
        assert_eq!(adjustment_for(-99), Adjustment::None);
        assert_eq!(adjustment_for(100), Adjustment::Slew);
        assert_eq!(adjustment_for(-999), Adjustment::Slew);
        assert_eq!(adjustment_for(1000), Adjustment::Step);
        assert_eq!(adjustment_for(3_600_000), Adjustment::Step);
    }

    /// 超大偏移 → **`Refused`，而且一个字节都不碰时钟**。
    ///
    /// 这条在**任何平台**都能跑：拒绝发生在调用平台接口**之前**，
    /// 所以 Windows 上也能验证"拒绝"这条语义（真去调 `clock_settime` 的路径
    /// 只能在 Linux 上验，见附录 B）。
    #[test]
    fn absurd_offsets_are_refused_before_any_clock_change() {
        // 48 小时：明显不合理
        assert_eq!(apply_offset(48 * 3_600_000).unwrap(), Adjustment::Refused);
        // 50 年：一个伪造的 UDP 回复能做到的量级
        assert_eq!(
            apply_offset(50 * 365 * 24 * 3_600_000).unwrap(),
            Adjustment::Refused
        );
        // 负方向同样拒绝
        assert_eq!(apply_offset(-MAX_OFFSET_MS - 1).unwrap(), Adjustment::Refused);
        // 上限之内、且在"不动"阈值内 → None（不会真去调时钟，所以任何平台都能测）
        assert_eq!(apply_offset(0).unwrap(), Adjustment::None);
    }

}
