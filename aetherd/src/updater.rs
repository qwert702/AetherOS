//! **自动更新：定时从更新源拉包并暂存**（P1 guest 侧，2026-10-16）。
//!
//! ## 它做什么 / 不做什么
//!
//! * 做：定时 `GET <url>/update.json` → 比版本 → 下载 → **逐文件校验** → 投进
//!   `incoming/<version>/` → 交给既有的 [`crate::update::stage`] 暂存；
//! * 不做：**不直接改系统文件**。真正生效仍由 PID 1 在下次启动时按 `apply.list` 应用
//!   —— 这条链是既有机制，本模块只是把"包"送进 `incoming/`，与管理员手工投放等价。
//!
//! ## 四条硬约束（都来自既有设计，别在这里绕开）
//!
//! 1. **先验签**：`update.json` 必须有 `update.json.sig`（Ed25519），验签失败整包拒绝；
//! 2. **拒降级**：`stage()` 自己会挡（`version_is_newer`），这里也先挡一次，省一次下载；
//! 3. **整包拒绝**：任一文件哈希/大小不符就不投递（`verify_bundle` 的语义）；
//! 4. **只写 incoming/**：目标路径的白名单与路径形态由既有的 `parse_manifest` 校验
//!   （绝对、无 `..`、无控制字符、命中 `/var/apps` `/etc/aether` `/var/lib/aether`）。
//!
//! ## 安全边界（诚实写在这里）
//!
//! * 更新源只接受 **https://** —— 明文 http 会被中间人换包（哈希清单与包同源，一起被换）。
//!   局域网明文源要显式改这一条，属于刻意的取舍，不是遗漏。
//! * HTTPS 只保护传输；**清单与包的真实性由 Ed25519 签名保证**。服务器被攻破时，
//!   没有私钥就签不出能被接受的清单。`AETHER_ALLOW_UNSIGNED_UPDATE=1` 只用于本地开发。
//! * 失败一律**只记日志、不重试轰炸**：下一轮（默认 6 小时）再看，避免把更新源打挂。
//! * 开关默认 **关**：配置不存在就不跑（`/etc/aether/updater.json` 由管理员/镜像提供）。

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

/// 配置文件（在 `TARGET_PREFIXES` 白名单内，可被更新包自己更新）。
#[cfg(target_os = "linux")]
pub const CONFIG_PATH: &str = "/etc/aether/updater.json";
/// 更新源上清单文件的固定名字（与 `crate::update::MANIFEST` 同名，指同一个东西）。
pub const MANIFEST_NAME: &str = "update.json";

/// 检查间隔的下限：太短会把更新源打挂（默认 6 小时）。
pub const MIN_CHECK_MINUTES: u64 = 5;
/// 检查间隔的上限：一周。再长就等于"没开"。
pub const MAX_CHECK_MINUTES: u64 = 7 * 24 * 60;
/// 单文件大小上限（64 MB）：防"清单说 1 字节、实际给几个 GB"把持久分区写满。
#[cfg(target_os = "linux")]
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// 清单大小上限（1 MB）：清单只该是路径/哈希列表，不应该有别的载荷。
#[cfg(target_os = "linux")]
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
/// 签名大小上限（128 字节）：Ed25519 原始签名固定 64 字节，留一倍余量。
#[cfg(target_os = "linux")]
pub const MAX_SIGNATURE_BYTES: usize = 128;
/// 单次 HTTP 超时：清单与文件都用它。
#[cfg(target_os = "linux")]
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 更新配置。字段名即是 JSON 键名（`serde` 默认 rename 规则）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// 更新源基址，**必须 https**（末尾斜杠可有可无，见 [`manifest_url`]）。
    pub url: String,
    /// 检查间隔（分钟）。
    pub check_minutes: u64,
}

/// 解析并校验配置。**任何一条不合规都返回 Err**（不静默降级为默认值 ——
/// 那样会让"配置写错了"表现成"自动更新莫名不发"，是最难查的一类故障）。
pub fn parse_config(text: &str) -> Result<Config, String> {
    let cfg: Config =
        serde_json::from_str(text).map_err(|e| format!("配置不是合法 JSON：{e}"))?;
    if !cfg.url.starts_with("https://") {
        return Err(format!(
            "更新源必须是 https://（拒绝明文 http：哈希清单与包同源，明文可被一起替换）：{:?}",
            cfg.url
        ));
    }
    if cfg.url.contains(char::is_whitespace) {
        return Err(format!("更新源里有空白字符：{:?}", cfg.url));
    }
    if !(MIN_CHECK_MINUTES..=MAX_CHECK_MINUTES).contains(&cfg.check_minutes) {
        return Err(format!(
            "check_minutes 必须在 {MIN_CHECK_MINUTES}‥{MAX_CHECK_MINUTES} 之间（给了 {}）",
            cfg.check_minutes
        ));
    }
    Ok(cfg)
}

/// 清单的完整 URL（容忍基址末尾带不带斜杠 —— 少一个斜杠就 404 是最常见的部署坑）。
pub fn manifest_url(base: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), MANIFEST_NAME)
}

/// 清单签名的完整 URL（与清单同目录的 `update.json.sig`）。
pub fn signature_url(base: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        crate::update::MANIFEST_SIG
    )
}

/// 是否允许"无签名更新"——**只用于本地开发**，生产必须保持关闭。
#[cfg(target_os = "linux")]
fn unsigned_update_allowed() -> bool {
    std::env::var("AETHER_ALLOW_UNSIGNED_UPDATE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// 远端这个版本是否**已经暂存过**（等着下次启动生效）。
///
/// 为什么需要它：`stage()` 成功之后 `incoming/<version>/` 按设计会留着，
/// 而 `current_version()` 读的是 `/etc/aether/version`（**还是旧版**）——
/// 于是不加判断的话，下一轮会再次认为"有新版"，撞上残留目录后**永久卡死**。
pub fn already_staged(remote: &str, staged: Option<&str>) -> bool {
    staged == Some(remote)
}

/// 距离上次检查是否已经够了。**纯函数**：便于把"定时"这件事本身也测掉。
pub fn should_check(last_check_unix: u64, now_unix: u64, check_minutes: u64) -> bool {
    // 时钟回拨（对时/休眠恢复）时 `now < last` —— 这时**允许**检查一次，
    // 否则一次回拨就能让自动更新静默停摆到"追平"为止。
    if now_unix < last_check_unix {
        return true;
    }
    now_unix - last_check_unix >= check_minutes.saturating_mul(60)
}

/// 一轮检查的结果（既用于日志，也用于测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(target_os = "linux")]
pub enum Outcome {
    /// 远端不比当前新 —— 什么都不做。
    UpToDate { remote: String, current: String },
    /// 已下载、校验、投递并暂存（下次启动生效）。
    Staged { version: String, files: usize },
}

#[cfg(target_os = "linux")]
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build()
}

/// 取一段有限大小的字节（清单、签名共用）。
#[cfg(target_os = "linux")]
fn fetch_bytes(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("拉取失败（{url}）：{e}"))?;
    let mut reader = resp.into_reader();
    let mut body = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("读正文失败（{url}）：{e}"))?;
    if body.len() > limit {
        return Err(format!("{url} 超过大小上限 {limit} 字节"));
    }
    Ok(body)
}

/// 取清单原始字节（**验签必须基于原始字节**，不能先转字符串再拼回去）。
#[cfg(target_os = "linux")]
fn fetch_manifest(url: &str) -> Result<Vec<u8>, String> {
    fetch_bytes(url, MAX_MANIFEST_BYTES)
}

/// 取清单签名（原始 64 字节）。
#[cfg(target_os = "linux")]
fn fetch_signature(url: &str) -> Result<Vec<u8>, String> {
    fetch_bytes(url, MAX_SIGNATURE_BYTES)
}

/// 下载单个文件到 `dest`，带大小上限（边写边数，超限即中止并返回 Err）。
#[cfg(target_os = "linux")]
fn download_to(url: &str, dest: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("下载失败（{url}）：{e}"))?;
    let mut reader = resp.into_reader();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录 {parent:?} 失败：{e}"))?;
    }
    let mut file = std::fs::File::create(dest).map_err(|e| format!("建文件 {dest:?} 失败：{e}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("读 {url} 失败：{e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_FILE_BYTES {
            // 先把半截文件删掉再报错，别让它留在 incoming/ 里冒充一个包
            let _ = std::fs::remove_file(dest);
            return Err(format!(
                "{url} 超过单文件上限 {MAX_FILE_BYTES} 字节 —— 已中止并删除半截文件"
            ));
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写 {dest:?} 失败：{e}"))?;
    }
    file.sync_all().map_err(|e| format!("sync {dest:?} 失败：{e}"))
}

/// 跑一轮：拉清单 → 比版本 → 下载 → 校验 → 投递 → 暂存。
///
/// `root` 是更新根目录（生产上是 [`crate::update::UPDATE_ROOT`]；测试里传临时目录）。
#[cfg(target_os = "linux")]
pub fn run_once(root: &Path, cfg: &Config) -> Result<Outcome, String> {
    let url = manifest_url(&cfg.url);
    let manifest_bytes = fetch_manifest(&url)?;
    let sig_url = signature_url(&cfg.url);
    let signature = match fetch_signature(&sig_url) {
        Ok(s) => s,
        Err(e) if unsigned_update_allowed() => {
            eprintln!(
                "[aetherd] ⚠ 自动更新开发模式：允许无签名清单（AETHER_ALLOW_UNSIGNED_UPDATE=1；{e}）"
            );
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    // **先验签、后解析**：攻击者不能拿任意 JSON 喂给解析器。
    if !(signature.is_empty() && unsigned_update_allowed()) {
        crate::update::verify_manifest_signature(&manifest_bytes, &signature)?;
    }
    let text = String::from_utf8(manifest_bytes)
        .map_err(|e| format!("清单不是合法 UTF-8：{e}"))?;
    let manifest = crate::update::parse_manifest(&text)?;
    let current = crate::update::current_version();
    if !crate::update::version_is_newer(&manifest.version, &current) {
        return Ok(Outcome::UpToDate { remote: manifest.version, current });
    }

    // 投递目录：先写临时名，全部校验通过后再改名 —— 不然"下到一半"会被当成一个包。
    let incoming = root.join(crate::update::INCOMING);
    let tmp = incoming.join(format!(".{}.partial", manifest.version));
    let final_dir = incoming.join(&manifest.version);
    // 已经暂存过这个版本 ⇒ 不是错：它正等着 PID 1 下次启动应用。
    // （不加这一条的话，第一次成功 stage 之后每轮都会走到下面的"残留目录"分支，
    //  updater 从此永久卡死 —— 2026-10-17 复查发现。）
    let staged_now = crate::update::staged(root).map(|s| s.version);
    if already_staged(&manifest.version, staged_now.as_deref()) {
        return Ok(Outcome::UpToDate { remote: manifest.version, current });
    }
    // 残留同名目录（例如上一轮 stage 失败留下的）⇒ 清掉重下，别把 updater 卡在这里。
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)
            .map_err(|e| format!("清理残留目录 {} 失败：{e}", final_dir.display()))?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    for f in &manifest.files {
        let rel = f.path.trim_start_matches('/');
        let dst = tmp.join(rel);
        let file_url = format!("{}/{}", cfg.url.trim_end_matches('/'), rel);
        download_to(&file_url, &dst)?;
    }
    // 逐文件核对哈希与大小（与管理员手工投放走的是同一套判据）
    let n = crate::update::verify_bundle(&tmp, &manifest)?;
    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建 {parent:?} 失败：{e}"))?;
    }
    std::fs::rename(&tmp, &final_dir)
        .map_err(|e| format!("投递到 {} 失败：{e}", final_dir.display()))?;
    let version = manifest.version.clone();
    // 签名是 (bundle, root, name, current_version)：bundle 是刚投递好的包目录
    // stage(bundle, root, current_version, now_unix)：第 4 个是时间戳（用于暂存记录）
    crate::update::stage(&final_dir, root, &current, crate::timesync::now_unix().0)?;
    Ok(Outcome::Staged { version, files: n })
}

/// 读配置；文件不存在或读不出内容都视为"未开启"（返回 `None` 并打一行日志）。
#[cfg(target_os = "linux")]
pub fn load_config(path: &Path) -> Option<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[aetherd] 自动更新未开启（读不到 {}：{e}）", path.display());
            return None;
        }
    };
    match parse_config(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            // 配置写错要**吵**：静默失败会让"更新不发"变成谜案
            eprintln!("[aetherd] 自动更新配置有误（{}）：{e}", path.display());
            None
        }
    }
}

/// 起一个后台线程做定时更新。**失败不影响服务启动**（与 `apps::link_all` 同一条原则：
/// AI 中枢才是本体，更新是附加能力）。
#[cfg(target_os = "linux")]
pub fn spawn(root: PathBuf, config_path: PathBuf) -> Option<std::thread::JoinHandle<()>> {
    let cfg = load_config(&config_path)?;
    eprintln!(
        "[aetherd] 自动更新已开启：每 {} 分钟检查一次 {}（源 {}）",
        cfg.check_minutes,
        manifest_url(&cfg.url),
        cfg.url
    );
    Some(std::thread::spawn(move || {
        let mut last: u64 = 0;
        loop {
            // .0 是 i64（时钟未同步时可能为负）⇒ 夹到非负，避免无符号回绕
            let now = crate::timesync::now_unix().0.max(0) as u64;
            if should_check(last, now, cfg.check_minutes) {
                last = now;
                match run_once(&root, &cfg) {
                    Ok(Outcome::UpToDate { remote, current }) => eprintln!(
                        "[aetherd] 自动更新：已是最新（远端 {remote} / 本机 {current}）"
                    ),
                    Ok(Outcome::Staged { version, files }) => eprintln!(
                        "[aetherd] 自动更新：{version} 已暂存（{files} 个文件），**下次启动生效**"
                    ),
                    // 只记日志、不重试轰炸：下一轮再看（避免把更新源打挂）
                    Err(e) => eprintln!("[aetherd] 自动更新失败：{e}"),
                }
            }
            // 每秒醒一次只为对齐"该不该查"的纯判据；负载可忽略（一次比较）。
            std::thread::sleep(Duration::from_secs(60));
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_https_and_sane_interval() {
        let ok = r#"{"url":"https://aether.cbnac.com/updates","check_minutes":360}"#;
        assert_eq!(
            parse_config(ok).unwrap(),
            Config { url: "https://aether.cbnac.com/updates".into(), check_minutes: 360 }
        );
        // 明文 http 必须拒绝（哈希清单与包同源，明文可被一起替换）
        assert!(parse_config(r#"{"url":"http://aether.cbnac.com/updates","check_minutes":360}"#).is_err());
        // 间隔越界必须拒绝（不是静默钳到边界 —— 那会让配置错误表现成"莫名不发"）
        assert!(parse_config(r#"{"url":"https://x/y","check_minutes":1}"#).is_err());
        assert!(parse_config(r#"{"url":"https://x/y","check_minutes":99999}"#).is_err());
        // 空白字符（含换行注入）必须拒绝
        assert!(parse_config("{\"url\":\"https://x/a b\",\"check_minutes\":360}").is_err());
        // 不是 JSON
        assert!(parse_config("{}").is_err());
    }

    #[test]
    fn manifest_url_tolerates_trailing_slash() {
        assert_eq!(manifest_url("https://h/updates"), "https://h/updates/update.json");
        assert_eq!(manifest_url("https://h/updates/"), "https://h/updates/update.json");
        // 少一个斜杠就 404 是最常见的部署坑，两种写法必须等价
        assert_eq!(manifest_url("https://h"), "https://h/update.json");
    }

    #[test]
    fn signature_url_matches_manifest_layout() {
        assert_eq!(
            signature_url("https://h/updates"),
            "https://h/updates/update.json.sig"
        );
        assert_eq!(
            signature_url("https://h/updates/"),
            "https://h/updates/update.json.sig"
        );
        assert_eq!(signature_url("https://h"), "https://h/update.json.sig");
    }

    #[test]
    fn check_interval_boundaries() {
        let now = 1_000_000u64;
        assert!(should_check(0, now, 5), "从没查过 ⇒ 立刻查");
        assert!(should_check(now - 300, now, 5), "正好到点 ⇒ 查");
        assert!(!should_check(now - 299, now, 5), "差一秒 ⇒ 不查");
        // 时钟回拨（休眠恢复/对时）：必须允许查，否则一次回拨就静默停摆
        assert!(should_check(now + 10_000, now, 360), "时钟回拨 ⇒ 允许查一次");
    }

    #[test]
    fn staged_version_is_not_an_error_and_never_sticks_the_updater() {
        // 已暂存同一版本 ⇒ 视为"已就绪"，不再重复下载（否则会卡死）
        assert!(already_staged("0.1.2", Some("0.1.2")));
        // 暂存的是别的版本 / 没暂存 ⇒ 该继续处理
        assert!(!already_staged("0.1.3", Some("0.1.2")));
        assert!(!already_staged("0.1.2", None));
    }

    #[test]
    fn version_decision_uses_the_existing_comparator() {
        // 不在这里重造版本比较：直接复用 update.rs 的判据（单一来源）
        assert!(crate::update::version_is_newer("0.1.2", "0.1.1"));
        assert!(!crate::update::version_is_newer("0.1.1", "0.1.1"));
        assert!(!crate::update::version_is_newer("0.1.0", "0.1.1"));
    }
}
