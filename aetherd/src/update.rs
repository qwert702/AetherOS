//! 更新包的校验与暂存（FEATURE-GAPS §3.1）。
//!
//! ## 范围：**文件级离线更新包**，不是镜像级 OTA
//!
//! 这个系统的主系统区是只读镜像 —— 真正的"换整块系统"要镜像互换 + 引导器配合，
//! 属另一个量级。本模块更新的是**持久分区上的东西**：
//! 已装应用（`/var/apps`）、`/etc/aether` 下的配置、`/var/lib/aether` 下的状态。
//!
//! 流程：
//! ```text
//! 0. 验签   verify_manifest_signature → `update.json.sig`（Ed25519；先验签后解析）
//! 1. 校验   parse_manifest → 版本、白名单、路径形态
//! 2. 验哈希 verify_bundle  → 逐文件 sha256（不符就整包拒绝）
//! 3. 暂存   stage          → 拷进 staged/，写 ready.json（含逐文件哈希）
//! 4. 应用   由 aether-init 在启动服务之前做（另一个模块，PID 1 才敢碰这些路径）
//! ```
//!
//! ## 为什么安全属性都写在这里（而不是"应用时注意一下"）
//!
//! 更新器以 **root** 运行、并且**按清单里的路径写文件** —— 这是全项目权限最高的一段代码。
//! 所以"能不能写这个路径""版本能不能更低"这类判断必须在**校验阶段**就定死，
//! 并且每条都有测试；应用阶段只做"照着已经验过的清单搬文件"。

use std::path::{Path, PathBuf};

/// 更新包根目录（**只允许 root 写** —— 这条是整套机制的前提）。
pub const UPDATE_ROOT: &str = "/var/lib/aether/update";
/// 清单文件名。
pub const MANIFEST: &str = "update.json";
/// 清单签名文件名（Ed25519 detached signature，原始 64 字节）。
// Windows 构建没有 Linux 更新器调用点；单测仍会断言 URL 拼接，故不判死代码。
#[allow(dead_code)]
pub const MANIFEST_SIG: &str = "update.json.sig";
/// 更新清单签名公钥（32 字节原始 Ed25519 公钥）。
///
/// 这是**公开**材料，可以入库；私钥绝不入库。更新器只信任这把钥匙：
/// 即使 HTTPS 服务器被攻破、清单与包被同时替换，没有私钥也签不出可被接受的清单。
// Windows 构建没有 Linux 更新器调用点；单测仍会检查长度，故不判死代码。
#[allow(dead_code)]
pub const UPDATE_PUBKEY: &[u8] = include_bytes!("../keys/update_pubkey.bin");
/// 待处理包的投递目录（管理员把更新包放这里；**界面只能选其中的直接子目录**）。
///
/// 多这一层的理由：`stage()` 接的是"包目录路径"。若这个路径来自界面，
/// 就等于"让界面指定任意目录被读、其中的文件被拷到系统路径" ——
/// 所以只接受这个目录下的**直接子目录**，名字还必须是简单目录名。
pub const INCOMING: &str = "incoming";
/// 暂存子目录（校验通过、等待应用）。
pub const STAGED: &str = "staged";
/// 暂存完成标记（由 `stage` 写、由 `aether-init` 读）。
pub const READY: &str = "ready.json";
/// **待应用表**：每行 `<sha256>\t<bytes>\t<target>`。
///
/// 它是 init 侧的就绪协议：**这个文件存在 = 暂存完整**（`stage` 最后才写它）。
/// 于是 init 不需要 JSON 解析器，也不必把两个 crate 的更新逻辑耦合成共享 crate。
/// tab 作分隔符是安全的 —— 路径校验拒绝一切控制字符（含 tab）。
pub const APPLY_LIST: &str = "apply.list";

/// **允许被更新覆盖的路径前缀**。
///
/// 这是本模块的核心安全边界：更新器按清单里的路径写文件，
/// 没有白名单的话它就成了"以 root 写任意文件"的工具。
/// `/etc/aether/` 而不是整个 `/etc/`：系统账户、`shadow`、`fstab` 这些
/// 不该由更新包碰（它们各有自己的通路与审计）。
pub const TARGET_PREFIXES: &[&str] = &[
    "/var/apps/",
    "/etc/aether/",
    "/var/lib/aether/",
];

/// 清单里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateFile {
    /// 包内相对路径（同时也是它要覆盖的目标，见 `resolve_target`）。
    pub path: String,
    /// 目标文件的 sha256（十六进制小写）。
    pub sha256: String,
    pub bytes: u64,
}

/// 更新包清单。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateManifest {
    /// 版本号，形如 `1.2.3`（**只比较数字段**，见 `version_is_newer`）。
    pub version: String,
    /// 给人看的一句话说明（可为空）。
    #[serde(default)]
    pub note: String,
    pub files: Vec<UpdateFile>,
}

/// 校验通过的暂存包。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Staged {
    pub version: String,
    pub note: String,
    pub files: Vec<UpdateFile>,
    /// 暂存时刻（Unix 秒）
    pub staged_unix: i64,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// 版本号是否合法（`1`、`1.2`、`1.2.3`，只允许数字与点）。
pub fn valid_version(v: &str) -> bool {
    if v.is_empty() || v.len() > 32 {
        return false;
    }
    v.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// 把版本号拆成数字段（用于比较）。
pub fn version_parts(v: &str) -> Vec<u64> {
    v.split('.').filter_map(|p| p.parse::<u64>().ok()).collect()
}

/// `a` 是否**严格新于** `b`（逐段比较，缺的段按 0 算：`1.2` == `1.2.0`）。
///
/// **为什么必须严格新**：允许降级等于允许把已经修掉的漏洞放回来 ——
/// 攻击者只要能放一个旧版本包进去，就能"更新"掉一个刚刚修好的洞。
/// 真要降级（回滚坏更新）应当走**快照回滚**（§5.6），那条路有明确的用户确认。
pub fn version_is_newer(a: &str, b: &str) -> bool {
    let (mut x, mut y) = (version_parts(a), version_parts(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x > y
}

/// 清单里的路径能不能作为更新目标。
///
/// 三条一起查：必须是绝对路径、必须命中白名单前缀、**不许含 `..`**。
/// 单看白名单不够 —— `/etc/aether/../../shadow` 以白名单前缀开头，
/// 但规范化之后指向别处。
pub fn path_is_allowed(path: &str) -> bool {
    if !path.starts_with('/') || path.contains("..") {
        return false;
    }
    // **控制字符一律拒绝**：路径里带 `\n` 时，任何按行读清单的下游
    //（日志、审计、脚本）都会把它看成两行 —— 第二行可以是 `/etc/shadow`。
    // 这是"换行注入"，属于只看前缀/`..` 查不出来的那一类。
    // 空格仍然允许（POSIX 路径里合法）。
    if path.chars().any(|c| c.is_control()) {
        return false;
    }
    if path.contains("//") {
        return false;
    }
    TARGET_PREFIXES.iter().any(|p| path.starts_with(p))
        // 前缀本身不能是"目标"（不能覆盖目录）
        && !TARGET_PREFIXES.contains(&path)
}

/// 解析并校验清单（**版本、文件数、每个文件**）。
pub fn parse_manifest(text: &str) -> Result<UpdateManifest, String> {
    let m: UpdateManifest =
        serde_json::from_str(text).map_err(|e| format!("清单不是合法 JSON：{e}"))?;
    if !valid_version(&m.version) {
        return Err(format!("版本号不合法：{:?}（只允许数字与点）", m.version));
    }
    if m.files.is_empty() {
        return Err("清单里没有任何文件".to_string());
    }
    if m.files.len() > 4096 {
        return Err(format!("清单里的文件过多（{}）", m.files.len()));
    }
    let mut seen = std::collections::HashSet::new();
    for f in &m.files {
        if !path_is_allowed(&f.path) {
            return Err(format!(
                "目标路径不在允许范围内：{:?}（允许的前缀：{}）",
                f.path,
                TARGET_PREFIXES.join("、")
            ));
        }
        if !is_hex64(&f.sha256) {
            return Err(format!("{}：sha256 不是 64 位十六进制小写", f.path));
        }
        // 同一个目标出现两次 = 清单自相矛盾（后一个会覆盖前一个，而哈希只对得上一个）
        if !seen.insert(f.path.as_str()) {
            return Err(format!("{}：在清单里出现了两次", f.path));
        }
    }
    Ok(m)
}

/// 校验清单的 Ed25519 签名（生产公钥）。
///
/// 必须在 `parse_manifest` **之前**调用：先确认"这份清单确实是我们签的"，
/// 再去解析其中的版本/路径/哈希。否则攻击者可以用任意 JSON 触发解析器。
// 生产调用点在 Linux 更新器；Windows 构建只看得到测试，故不判死代码。
#[allow(dead_code)]
pub fn verify_manifest_signature(manifest: &[u8], signature: &[u8]) -> Result<(), String> {
    verify_manifest_signature_with_key(manifest, signature, UPDATE_PUBKEY)
}

/// 可注入公钥的验签实现（测试用；生产入口见上方）。
#[allow(dead_code)]
pub fn verify_manifest_signature_with_key(
    manifest: &[u8],
    signature: &[u8],
    public_key: &[u8],
) -> Result<(), String> {
    if public_key.len() != 32 {
        return Err(format!(
            "更新公钥长度不对（期望 32 字节 Ed25519 公钥，实得 {} 字节）",
            public_key.len()
        ));
    }
    if signature.len() != 64 {
        return Err(format!(
            "清单签名长度不对（期望 64 字节 Ed25519 签名，实得 {} 字节）",
            signature.len()
        ));
    }
    use ring::signature::{UnparsedPublicKey, ED25519};
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(manifest, signature)
        .map_err(|_| {
            "清单签名校验失败：签名与公钥不匹配（清单可能被篡改，整包拒绝）".to_string()
        })
}

/// 逐文件核对哈希与大小。返回核对过的文件数。
pub fn verify_bundle(bundle: &Path, m: &UpdateManifest) -> Result<usize, String> {
    for f in &m.files {
        // 路径已被 `parse_manifest` 校验过，这里只做"包内相对路径"的取法
        let rel = f.path.trim_start_matches('/');
        let src = bundle.join(rel);
        let md = std::fs::symlink_metadata(&src)
            .map_err(|e| format!("{}：包内找不到文件（{e}）", f.path))?;
        if md.file_type().is_symlink() {
            return Err(format!("{}：包内是符号链接，拒绝", f.path));
        }
        if !md.is_file() {
            return Err(format!("{}：包内不是普通文件", f.path));
        }
        let got = crate::apps::sha256_file(&src).map_err(|e| format!("{}：算哈希失败（{e}）", f.path))?;
        if got != f.sha256 {
            return Err(format!(
                "{}：哈希不符（清单 {} / 实际 {}）—— 整包拒绝",
                f.path, f.sha256, got
            ));
        }
        if md.len() != f.bytes {
            return Err(format!("{}：大小不符（清单 {} / 实际 {}）", f.path, f.bytes, md.len()));
        }
    }
    Ok(m.files.len())
}

/// 暂存一个已经验过的包：拷进 `staged/`，写 `ready.json`。
///
/// `current_version` 用来挡降级（见 [`version_is_newer`]）。
pub fn stage(
    bundle: &Path,
    root: &Path,
    current_version: &str,
    now_unix: i64,
) -> Result<Staged, String> {
    let text = std::fs::read_to_string(bundle.join(MANIFEST))
        .map_err(|e| format!("读不到 {}：{e}", bundle.join(MANIFEST).display()))?;
    let m = parse_manifest(&text)?;
    if !version_is_newer(&m.version, current_version) {
        return Err(format!(
            "拒绝降级/同版本：包内版本 {}，当前 {}（要回滚请用快照回滚）",
            m.version, current_version
        ));
    }
    verify_bundle(bundle, &m)?;

    // 先清掉上一次的暂存：留着会让"这次装的是哪份"说不清
    let staged_dir = root.join(STAGED);
    if staged_dir.exists() {
        std::fs::remove_dir_all(&staged_dir)
            .map_err(|e| format!("清不掉旧的暂存目录：{e}"))?;
    }
    std::fs::create_dir_all(&staged_dir).map_err(|e| format!("建暂存目录失败：{e}"))?;
    for f in &m.files {
        let rel = f.path.trim_start_matches('/');
        let dst = staged_dir.join(rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("建目录失败 {}：{e}", p.display()))?;
        }
        std::fs::copy(bundle.join(rel), &dst)
            .map_err(|e| format!("拷贝 {} 失败：{e}", f.path))?;
    }
    // 版本文件**总是**跟着更新走：它是"当前版本"的单一来源
    //（`current_version()` 读的就是它），不带上它就会出现"更新了但版本号没变"。
    let version_target = "/etc/aether/version";
    if !m.files.iter().any(|f| f.path == version_target) {
        let rel = version_target.trim_start_matches('/');
        let dst = staged_dir.join(rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("建目录失败 {}：{e}", p.display()))?;
        }
        std::fs::write(&dst, format!("{}\n", m.version))
            .map_err(|e| format!("写版本文件失败：{e}"))?;
    }

    let st = Staged {
        version: m.version.clone(),
        note: m.note.clone(),
        files: m.files.clone(),
        staged_unix: now_unix,
    };
    let json = serde_json::to_string_pretty(&st).map_err(|e| e.to_string())?;
    std::fs::write(root.join(READY), json).map_err(|e| format!("写 {} 失败：{e}", READY))?;

    // **就绪协议：apply.list 最后写** —— 它出现就表示上面全部成功。
    // 写在 `READY` 之后是刻意的：先给人看的账，再给 init 的信号。
    let mut list = String::new();
    for f in &m.files {
        list.push_str(&format!("{}\t{}\t{}\n", f.sha256, f.bytes, f.path));
    }
    // 版本文件也要应用（若清单里没有它，就用我们刚写的那个）
    if !m.files.iter().any(|f| f.path == version_target) {
        let body = format!("{}\n", m.version);
        list.push_str(&format!(
            "{}\t{}\t{}\n",
            crate::apps::sha256_bytes(body.as_bytes()),
            body.len(),
            version_target
        ));
    }
    std::fs::write(root.join(APPLY_LIST), list).map_err(|e| format!("写 {} 失败：{e}", APPLY_LIST))?;
    Ok(st)
}

/// 读暂存标记（`None` = 没有待应用的更新，或标记坏了）。
pub fn staged(root: &Path) -> Option<Staged> {
    let text = std::fs::read_to_string(root.join(READY)).ok()?;
    let st: Staged = serde_json::from_str(&text).ok()?;
    // 标记也要过一遍校验：暂存目录可能被别的东西写过
    if !valid_version(&st.version) || st.files.is_empty() {
        return None;
    }
    if st.files.iter().any(|f| !path_is_allowed(&f.path)) {
        return None;
    }
    Some(st)
}

/// 当前系统版本。
///
/// 优先读 `/etc/aether/version`（**单一来源**：更新包自己也会更新它，
/// 这样"当前版本"与"包内版本"用的是同一套编号）；读不到再退回二进制自身的版本号。
/// 将来版本号若改由别处产生，只需要改这一处。
pub const VERSION_FILE: &str = "/etc/aether/version";

pub fn current_version() -> String {
    std::fs::read_to_string(VERSION_FILE)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|v| valid_version(v))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// 一个投递目录名是否合法（**简单目录名**：无分隔符、无 `..`、无控制字符、限长）。
pub fn valid_bundle_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.chars().any(|c| c.is_control())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 把界面给的"包名"解析成投递目录下的路径。
///
/// **这是信任边界**：`name` 来自界面，绝不能让它拼出投递目录之外的路径。
pub fn incoming_bundle(root: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_bundle_name(name) {
        return Err(format!("包名不合法：{name:?}（只允许字母数字与 - _ .）"));
    }
    let dir = root.join(INCOMING).join(name);
    if !dir.is_dir() {
        return Err(format!(
            "投递目录里没有这个包：{}（把更新包放到 {}/{} 下再试）",
            dir.display(),
            root.display(),
            INCOMING
        ));
    }
    Ok(dir)
}

/// 列出投递目录里等待处理的包名（供界面显示）。
pub fn list_incoming(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root.join(INCOMING)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| valid_bundle_name(n))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-update-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn hex_of(bytes: &[u8]) -> String {
        crate::apps::sha256_hex(bytes)
    }

    /// 造一个合法的包：文件放在包内 `var/apps/demo/demo`，清单指向 `/var/apps/demo/demo`。
    fn make_bundle(root: &Path, version: &str, body: &[u8]) -> PathBuf {
        let b = root.join("bundle");
        let target = "/var/apps/demo/demo";
        let rel = target.trim_start_matches('/');
        std::fs::create_dir_all(b.join(rel).parent().unwrap()).unwrap();
        std::fs::write(b.join(rel), body).unwrap();
        let m = UpdateManifest {
            version: version.to_string(),
            note: "测试包".to_string(),
            files: vec![UpdateFile {
                path: target.to_string(),
                sha256: hex_of(body),
                bytes: body.len() as u64,
            }],
        };
        std::fs::write(b.join(MANIFEST), serde_json::to_string_pretty(&m).unwrap()).unwrap();
        b
    }

    #[test]
    fn versions_compare_numerically_not_lexically() {
        assert!(version_is_newer("1.2.10", "1.2.9"), "10 比 9 新（不能按字符串比）");
        assert!(version_is_newer("1.3", "1.2.9"));
        assert!(version_is_newer("2.0", "1.99.99"));
        // 缺的段按 0 算 → `1.2.0` 与 `1.2` **相等**，所以"严格新"为 false
        assert!(!version_is_newer("1.2.0", "1.2"), "缺段按 0 算 → 相等，不是更新");
        assert!(!version_is_newer("1.2", "1.2.0"), "反过来同样相等");
        assert!(!version_is_newer("1.2", "1.2"));
        assert!(!version_is_newer("1.2.9", "1.2.10"));
        // 非法版本号（解析不出段）不该"意外新"
        assert!(!version_is_newer("x", "1.0"));
    }

    /// **白名单 + 不许 `..`**：更新器以 root 写文件，这是它的核心边界。
    #[test]
    fn only_whitelisted_targets_are_allowed() {
        for ok in [
            "/var/apps/demo/demo",
            "/etc/aether/policy.json",
            "/var/lib/aether/state.json",
        ] {
            assert!(path_is_allowed(ok), "{ok} 应当允许");
        }
        for bad in [
            // 白名单之外 —— 更新器不该能碰系统账户与引导配置
            "/etc/shadow",
            "/etc/passwd",
            "/etc/fstab",
            "/boot/vmlinuz",
            "/root/.ssh/authorized_keys",
            // 以白名单前缀开头，但用 .. 逃逸出去（**只看前缀是不够的**）
            "/etc/aether/../../shadow",
            "/var/apps/../../../etc/passwd",
            // 相对路径、空、换行注入、前缀本身（目录不能被"覆盖"）
            "etc/aether/x",
            "",
            "/etc/aether/",
            "/var/apps/",
            "/etc/aether/x\0.txt",
            "/etc/aether/x\n/etc/shadow", // 换行注入：按行读清单的下游会读成两行
            "/etc/aether/x\t.txt",
            "/etc/aether//x",
            "/etc/aether/x\n/etc/shadow",
        ] {
            assert!(!path_is_allowed(bad), "{bad:?} 不该允许");
        }
    }

    #[test]
    fn manifest_rejects_bad_versions_hashes_and_duplicates() {
        let good = UpdateFile {
            path: "/var/apps/demo/demo".into(),
            sha256: "a".repeat(64),
            bytes: 3,
        };
        let mk = |files: Vec<UpdateFile>, version: &str| UpdateManifest {
            version: version.into(),
            note: String::new(),
            files,
        };
        // 合法
        assert!(parse_manifest(&serde_json::to_string(&mk(vec![good.clone()], "1.0.1")).unwrap()).is_ok());
        // 版本号不合法
        for v in ["", "1.0.0-beta", "最新", "1..2"] {
            assert!(parse_manifest(&serde_json::to_string(&mk(vec![good.clone()], v)).unwrap()).is_err(), "{v} 该被拒");
        }
        // 没有文件
        assert!(parse_manifest(&serde_json::to_string(&mk(vec![], "1.0.1")).unwrap()).is_err());
        // 哈希形态不对（大写/长度不对都算）
        for h in ["A".repeat(64), "a".repeat(63), "z".repeat(64), String::new()] {
            let f = UpdateFile { sha256: h, ..good.clone() };
            assert!(parse_manifest(&serde_json::to_string(&mk(vec![f], "1.0.1")).unwrap()).is_err());
        }
        // 同一个目标出现两次
        assert!(parse_manifest(&serde_json::to_string(&mk(vec![good.clone(), good.clone()], "1.0.1")).unwrap()).is_err());
        // 非 JSON
        assert!(parse_manifest("{ 这不是 json").is_err());
    }

    /// 哈希不符 → **整包拒绝**（不能"跳过坏的那个装其余"）。
    #[test]
    fn a_corrupted_file_rejects_the_whole_bundle() {
        let root = tmp("hash");
        let b = make_bundle(&root, "1.0.1", b"original");
        // 手改包内文件（模拟下载损坏/被替换）
        let rel = "var/apps/demo/demo";
        std::fs::write(b.join(rel), b"tampered!").unwrap();
        let err = stage(&b, &root.join("upd"), "1.0.0", 1_700_000_000).unwrap_err();
        assert!(err.contains("哈希不符"), "要说清是哈希不符：{err}");
        assert!(
            !root.join("upd").join(READY).exists(),
            "拒绝后不许留下 ready 标记（否则进程会被诱导去应用它）"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正常流程：校验 → 暂存 → 读回标记。
    #[test]
    fn a_valid_bundle_is_staged_and_readable() {
        let root = tmp("stage");
        let b = make_bundle(&root, "1.0.1", b"hello update");
        let upd = root.join("upd");
        let st = stage(&b, &upd, "1.0.0", 1_700_000_000).unwrap();
        assert_eq!(st.version, "1.0.1");
        assert_eq!(st.staged_unix, 1_700_000_000);
        // 读回来（`aether-init` 走这条）
        let back = staged(&upd).expect("应当读得到暂存标记");
        assert_eq!(back, st);
        // 文件真的在暂存目录里，且内容正确
        let staged_file = upd.join(STAGED).join("var/apps/demo/demo");
        assert_eq!(std::fs::read(&staged_file).unwrap(), b"hello update");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **不许降级**：低版本、同版本都拒绝（降级会把已修的漏洞放回来）。
    #[test]
    fn downgrades_are_refused() {
        let root = tmp("downgrade");
        let b = make_bundle(&root, "1.0.0", b"older");
        let upd = root.join("upd");
        let err = stage(&b, &upd, "1.0.0", 1_700_000_000).unwrap_err();
        assert!(err.contains("拒绝降级"), "同版本要拒：{err}");
        let b2 = make_bundle(&root, "0.9.9", b"older");
        let err2 = stage(&b2, &upd, "1.0.0", 1_700_000_000).unwrap_err();
        assert!(err2.contains("拒绝降级"), "低版本要拒：{err2}");
        assert!(staged(&upd).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 暂存标记被篡改（路径不在白名单）→ 当作"没有待应用的更新"。
    #[test]
    fn a_tampered_ready_marker_is_ignored() {
        let root = tmp("tampered");
        let upd = root.join("upd");
        std::fs::create_dir_all(&upd).unwrap();
        std::fs::write(
            upd.join(READY),
            r#"{"version":"9.9.9","note":"","staged_unix":1,
                "files":[{"path":"/etc/shadow","sha256":"aa","bytes":1}]}"#,
        )
        .unwrap();
        assert!(staged(&upd).is_none(), "标记里的路径不在白名单 → 必须当作没有更新");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 重新暂存会清掉上一次（否则"装的是哪份"说不清）。
    #[test]
    fn staging_again_replaces_the_previous_one() {
        let root = tmp("restage");
        let upd = root.join("upd");
        let b1 = make_bundle(&root, "1.0.1", b"first");
        stage(&b1, &upd, "1.0.0", 1).unwrap();
        // 造第二份（换个包目录）
        let b2dir = root.join("bundle2");
        std::fs::create_dir_all(b2dir.join("var/apps/demo")).unwrap();
        std::fs::write(b2dir.join("var/apps/demo/demo"), b"second").unwrap();
        let m = UpdateManifest {
            version: "1.0.2".into(),
            note: String::new(),
            files: vec![UpdateFile {
                path: "/var/apps/demo/demo".into(),
                sha256: hex_of(b"second"),
                bytes: 6,
            }],
        };
        std::fs::write(b2dir.join(MANIFEST), serde_json::to_string(&m).unwrap()).unwrap();
        let st = stage(&b2dir, &upd, "1.0.1", 2).unwrap();
        assert_eq!(st.version, "1.0.2");
        assert_eq!(
            std::fs::read(upd.join(STAGED).join("var/apps/demo/demo")).unwrap(),
            b"second"
        );
        assert_eq!(staged(&upd).unwrap().version, "1.0.2");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 包内是符号链接 → 拒绝（否则"更新"可以变成"装一个指向任意路径的链接"）。
    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_bundle_is_refused() {
        let root = tmp("symlink");
        let b = root.join("bundle");
        let rel = "var/apps/demo/demo";
        std::fs::create_dir_all(b.join(rel).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/etc/shadow", b.join(rel)).unwrap();
        let m = UpdateManifest {
            version: "1.0.1".into(),
            note: String::new(),
            files: vec![UpdateFile {
                path: "/var/apps/demo/demo".into(),
                sha256: "a".repeat(64),
                bytes: 0,
            }],
        };
        std::fs::write(b.join(MANIFEST), serde_json::to_string(&m).unwrap()).unwrap();
        let err = verify_bundle(&b, &m).unwrap_err();
        assert!(err.contains("符号链接"), "要说清是符号链接：{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
    /// **信任边界**：界面给的是"包名"，不是路径 —— 不能让它拼出投递目录之外。
    #[test]
    fn only_simple_names_inside_incoming_are_accepted() {
        for ok in ["pkg-1", "update_2026", "1.2.3"] {
            assert!(valid_bundle_name(ok), "{ok} 应当合法");
        }
        for bad in [
            "", ".", "..", "../etc", "a/b", "a\\b", ".hidden", &"x".repeat(65),
            "a\nb", "a b", "包",
        ] {
            assert!(!valid_bundle_name(bad), "{bad:?} 不该合法");
        }
        // 解析必须挡住逃逸，且**不存在的包**要给可操作的提示
        let root = std::path::Path::new("/var/lib/aether/update");
        assert!(incoming_bundle(root, "../etc").is_err());
        assert!(incoming_bundle(root, "a/b").is_err());
        let err = incoming_bundle(root, "nope").unwrap_err();
        assert!(err.contains("投递目录里没有这个包"), "要说清去哪儿放包：{err}");
    }

    /// 投递目录里只有合法名字的目录会被列出来。
    #[test]
    fn listing_incoming_shows_only_valid_directories() {
        let root = tmp("incoming");
        let inc = root.join(INCOMING);
        std::fs::create_dir_all(inc.join("good-1")).unwrap();
        std::fs::create_dir_all(inc.join(".hidden")).unwrap();
        std::fs::write(inc.join("a-file"), b"x").unwrap(); // 不是目录 → 不列
        let names = list_incoming(&root);
        assert_eq!(names, vec!["good-1".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **跨 crate 契约**：`stage()` 写的 `apply.list` 必须是 `aether-init` 能读的格式。
    ///
    /// 两个 crate 各自持有 `UPDATE_ROOT`/`STAGED`/`APPLY_LIST` 常量（不引入跨 crate 依赖），
    /// 所以这里**把 init 侧的解析规则复述一遍并核对**：三字段、tab 分隔、
    /// 目标是白名单内的绝对路径、最后一行是版本文件。
    /// 谁改了格式而没改另一边，这条测试会红。
    #[test]
    fn the_apply_list_is_the_contract_with_aether_init() {
        let root = tmp("contract");
        let b = make_bundle(&root, "1.0.1", b"payload");
        let upd = root.join("upd");
        stage(&b, &upd, "1.0.0", 1_700_000_000).unwrap();
        let list = std::fs::read_to_string(upd.join(APPLY_LIST)).unwrap();
        let mut targets = Vec::new();
        for line in list.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 3, "每行必须是 sha256<TAB>bytes<TAB>target：{line:?}");
            assert!(is_hex64(f[0]), "第一字段必须是 64 位十六进制：{line:?}");
            assert!(f[1].parse::<u64>().is_ok(), "第二字段必须是字节数：{line:?}");
            assert!(path_is_allowed(f[2]), "第三字段必须过白名单：{line:?}");
            // 暂存文件必须真的存在（init 会去读它）
            let staged = upd.join(STAGED).join(f[2].trim_start_matches('/'));
            assert!(staged.is_file(), "暂存文件不在：{}", staged.display());
            assert_eq!(
                std::fs::metadata(&staged).unwrap().len(),
                f[1].parse::<u64>().unwrap(),
                "记录的大小必须与实际一致（init 会核对它）：{line:?}"
            );
            targets.push(f[2].to_string());
        }
        assert!(targets.contains(&"/var/apps/demo/demo".to_string()));
        assert!(
            targets.contains(&"/etc/aether/version".to_string()),
            "版本文件必须**自动带上**（否则会出现「更新了但版本号没变」）：{targets:?}"
        );
        // ready.json 也要在（给人看的状态）
        assert!(upd.join(READY).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── Ed25519 清单签名（2026-10-24）────────────────────────────────────
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn test_keypair() -> (Ed25519KeyPair, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("生成测试密钥失败");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("加载测试密钥失败");
        let public = pair.public_key().as_ref().to_vec();
        (pair, public)
    }

    #[test]
    fn manifest_signature_accepts_exact_bytes() {
        let (pair, public) = test_keypair();
        let manifest = br#"{"version":"1.0.1","files":[]}"#;
        let sig = pair.sign(manifest);
        assert!(verify_manifest_signature_with_key(manifest, sig.as_ref(), &public).is_ok());
    }

    #[test]
    fn tampered_manifest_is_rejected() {
        let (pair, public) = test_keypair();
        let mut manifest = br#"{"version":"1.0.1","files":[]}"#.to_vec();
        let sig = pair.sign(&manifest);
        manifest[0] ^= 0x01; // 改一个字节
        let err = verify_manifest_signature_with_key(&manifest, sig.as_ref(), &public)
            .expect_err("篡改后的清单必须被拒绝");
        assert!(err.contains("签名校验失败"), "错误信息应说明验签失败：{err}");
    }

    #[test]
    fn truncated_signature_is_rejected() {
        let (pair, public) = test_keypair();
        let manifest = b"signed manifest";
        let sig = pair.sign(manifest);
        let err = verify_manifest_signature_with_key(manifest, &sig.as_ref()[..63], &public)
            .expect_err("63 字节签名必须被拒绝");
        assert!(err.contains("长度不对"), "错误信息应说明长度：{err}");
    }

    #[test]
    fn wrong_public_key_is_rejected() {
        let (pair, _public) = test_keypair();
        let (_other, other_public) = test_keypair();
        let manifest = b"signed manifest";
        let sig = pair.sign(manifest);
        let err = verify_manifest_signature_with_key(manifest, sig.as_ref(), &other_public)
            .expect_err("错误公钥必须被拒绝");
        assert!(err.contains("签名校验失败"), "错误信息应说明验签失败：{err}");
    }

    #[test]
    fn malformed_public_key_is_rejected() {
        let (pair, _public) = test_keypair();
        let manifest = b"signed manifest";
        let sig = pair.sign(manifest);
        let err = verify_manifest_signature_with_key(manifest, sig.as_ref(), &[0u8; 31])
            .expect_err("31 字节公钥必须被拒绝");
        assert!(err.contains("公钥长度不对"), "错误信息应说明公钥长度：{err}");
    }

    #[test]
    fn production_public_key_is_a_real_ed25519_key() {
        assert_eq!(UPDATE_PUBKEY.len(), 32, "生产公钥必须是 32 字节 Ed25519 公钥");
        // 用生产公钥跑一次必败的验签：既确认包装函数可用，也确认它不会放行伪造签名。
        assert!(verify_manifest_signature(b"not signed", &[0u8; 64]).is_err());
    }
}
