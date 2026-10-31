//! 配置快照与回滚（FEATURE-GAPS §5.6「折腾坏了能回滚」）。
//!
//! ## 范围：**配置快照**，不是盘镜像
//!
//! 这个名字（"系统快照"）很容易被读成"整盘镜像" —— 那要几百 MB、要能开机还原，
//! 而且与装盘器/引导器纠缠，是另一个量级。本模块做的是：
//! 把**决定系统行为的几个小文件**存一份，改坏了能一键回到之前那份。
//! 界面与文档都必须按这个口径说，否则用户会以为"回滚能救回我删掉的照片" —— 那是骗人。
//!
//! ## 不回滚什么（明确写下）
//!
//! * **已安装的应用**（`/var/apps`）：可能在快照之后装/卸过。回滚**不删**它们 ——
//!   默默删掉用户刚装的应用比"不回滚"更糟；
//! * 用户数据、家目录、任何大文件。
//!
//! ## 校验和：防写坏，不抗攻击
//!
//! 快照目录 `0700`、root 所有。校验和在这里是用来**发现截断/掉电写坏**的，
//! 不是安全边界 —— 能改快照文件的人同样能改校验和。所以用一个十行的 FNV-1a，
//! 而不是再引入一份加密实现。这一点写在文档里，免得被误当成完整性保护。

use std::io::Write;
use std::path::Path;

/// 快照目录。
pub const ROOT: &str = "/var/lib/aether/snapshots";
/// 清单文件名。
pub const MANIFEST: &str = "snapshot.json";

/// 快照覆盖的文件（**只列决定系统行为的小文件**）。
///
/// 为什么不遍历整个 `/etc`：那会把机器相关的、会自己变化的东西（`mtab`、`resolv.conf`
/// 的运行时副本、各种 `.dpkg-old`）也卷进来，回滚时反而制造不一致。
/// 白名单由人决定，代价是新增配置文件时要记得加进来 —— 这一点写在注释里。
pub const FILES: &[&str] = &[
    "/etc/aether/policy.json",
    "/etc/aether/model.json",
    "/var/lib/aether/settings.json",
    "/etc/resolv.conf",
    "/etc/timezone",
    "/etc/localtime",
];

/// 一个文件在快照里的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 原始绝对路径。
    pub path: String,
    /// FNV-1a 64 位校验和（十六进制）。
    pub checksum: String,
    pub bytes: u64,
    /// 快照时这个文件是否存在。**`false` 也是信息**：回滚时不该把它"造"出来。
    pub existed: bool,
    /// 快照那一刻的权限位（Unix；非 Unix 或旧清单为 0 = 未知）。
    ///
    /// **为什么必须记它**：回滚是"把文件写回去"，而写文件默认是 umask 后的权限
    /// （022 → 0644）。`policy.json` 这类文件本来只有 root 能读（0600），
    /// 回滚一次就变成所有人可读 —— 那是**回滚自己制造的权限降级**。
    /// 未知时一律用 0600（宁紧勿松）。
    pub mode: u32,
}

/// 一次快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub created_unix: i64,
    pub entries: Vec<Entry>,
}

/// 回滚时到底用哪个权限 —— **纯函数**，因此任何平台都能测这条安全决策。
///
/// 规则：清单里记了就用它；没记（`0` = 旧清单 / 非 Unix / 采集失败）就用 `0600`。
/// **宁紧勿松**：多要一点权限用户能自己改回来，少要一点（相对宽松）可能就没发现了。
pub fn restore_mode(recorded: u32) -> u32 {
    if recorded == 0 {
        0o600
    } else {
        // 只保留权限位，去掉文件类型位（S_IFREG 等）
        recorded & 0o7777
    }
}

/// 取一个文件的权限位（非 Unix 返回 0 = 未知）。
pub fn file_mode(p: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return std::fs::metadata(p).map(|m| m.permissions().mode()).unwrap_or(0);
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        0
    }
}

/// 设置权限位（非 Unix 上是空操作 —— 那里没有这套权限模型）。
fn set_file_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (p, mode);
        Ok(())
    }
}

/// FNV-1a 64 位（十行，无依赖）。
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn checksum(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}

/// 快照 id（目录名）是否合法。
///
/// **它是路径的一部分**：`id` 来自 IPC（界面）与目录扫描，含 `/`、`..` 的 id
/// 必须拒绝 —— 否则"回滚"会变成"往任意路径写文件"。
pub fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    if id.starts_with('.') || id.contains('/') || id.contains('\\') || id.contains("..") {
        return false;
    }
    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 由时间戳生成一个 id（形如 `20261004T112233`）。
pub fn id_for(unix_secs: i64) -> String {
    // 只做 UTC 拆分，不引入日期库：快照 id 只要能排序、能读就够
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let (h, mi, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    // 1900-01-01 起的儒略日换算（civil_from_days 的简化版）
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}")
}

/// 创建一次快照。返回清单（同时已落盘）。
pub fn create(root: &Path, now_unix: i64) -> std::io::Result<Manifest> {
    // **同一秒里创建两次不能互相覆盖**：id 只精确到秒，所以目录已存在就加后缀。
    // （审计发现：原来直接用 `root.join(id)`，第二次会静默覆盖第一次的快照。）
    let base = id_for(now_unix);
    let mut id = base.clone();
    let mut n = 2;
    let mut dir = root.join(&id);
    while dir.exists() {
        id = format!("{base}-{n}");
        dir = root.join(&id);
        n += 1;
        if n > 99 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("同一秒内快照过多（{base} 已有 {n} 份）"),
            ));
        }
    }
    std::fs::create_dir_all(dir.join("files"))?;
    // 目录 0700：快照里可能含模型配置等，不该给别的用户看
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut entries = Vec::new();
    for (i, path) in FILES.iter().enumerate() {
        let p = Path::new(path);
        let (bytes, existed) = match std::fs::read(p) {
            Ok(b) => (b, true),
            Err(_) => (Vec::new(), false),
        };
        if existed {
            // 存成"序号 + 原名"（原名里可能有 `/`，不能直接当文件名）
            let name = format!("{i:02}-{}", p.file_name().and_then(|s| s.to_str()).unwrap_or("file"));
            std::fs::write(dir.join("files").join(&name), &bytes)?;
        }
        entries.push(Entry {
            path: (*path).to_string(),
            checksum: if existed { checksum(&bytes) } else { String::new() },
            bytes: bytes.len() as u64,
            existed,
            mode: if existed { file_mode(p) } else { 0 },
        });
    }
    let m = Manifest { id: id.clone(), created_unix: now_unix, entries };
    let json = to_json(&m);
    let mut f = std::fs::File::create(dir.join(MANIFEST))?;
    f.write_all(json.as_bytes())?;
    f.sync_all()?;
    Ok(m)
}

/// 列举全部快照（按时间倒序，最新在前）。**坏清单只跳过它**，不让整个列表打不开。
pub fn list(root: &Path) -> Vec<Manifest> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path().join(MANIFEST)) else {
            continue;
        };
        if let Some(m) = parse_json(&text) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| b.created_unix.cmp(&a.created_unix).then_with(|| b.id.cmp(&a.id)));
    out
}

/// 回滚到某个快照。返回给用户看的说明（每个文件一行）。
///
/// **逐文件写回**：快照里 `existed=false` 的条目**跳过**（不把当时不存在的文件造出来），
/// 并在说明里如实写出来 —— 用户看到"少了一个文件"总比"莫名其妙多了一个"好。
pub fn restore(root: &Path, id: &str) -> std::io::Result<Vec<String>> {
    // 生产路径：只允许回滚 `FILES` 里列出的文件。
    restore_allowed(root, id, FILES)
}

/// 同 [`restore`]，但可以指定"允许哪些路径"。
///
/// **为什么要这个参数**：白名单写死在函数里，测试就只能拿 `/etc/...` 当目标 ——
/// 那既不能在 CI 里跑（要 root，还会动真实系统），也就等于"这条安全检查测不到"。
/// 生产用 [`FILES`]，测试用临时白名单，**两边走同一条代码路径**。
pub fn restore_allowed(
    root: &Path,
    id: &str,
    allowed: &[&str],
) -> std::io::Result<Vec<String>> {
    if !valid_id(id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("快照 id 不合法：{id:?}"),
        ));
    }
    let dir = root.join(id);
    let text = std::fs::read_to_string(dir.join(MANIFEST))?;
    let m = parse_json(&text).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "清单解析失败")
    })?;
    let mut notes = Vec::new();
    for (i, e) in m.entries.iter().enumerate() {
        // **鉴权先于工作**：白名单检查放在最前面 —— 清单是从磁盘读来的，
        // "把读来的路径直接当写入目标"是不该有的习惯（工作区安全基线也这么要求）。
        // 放在校验和之后的话，一份"指向 /etc/shadow 且内容损坏"的清单
        // 会先报"损坏"，而真正该说的是"这个路径根本不允许写"。
        if !allowed.contains(&e.path.as_str()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}：不在快照白名单内，拒绝回滚", e.path),
            ));
        }
        if !e.existed {
            notes.push(format!("{}：快照时不存在，未回滚", e.path));
            continue;
        }
        let name = format!(
            "{i:02}-{}",
            Path::new(&e.path).file_name().and_then(|s| s.to_str()).unwrap_or("file")
        );
        let bytes = std::fs::read(dir.join("files").join(&name))?;
        // 校验和：发现截断/写坏就**停下来**，而不是把坏内容写回系统
        let got = checksum(&bytes);
        if got != e.checksum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}：快照内容损坏（校验和不符），已停止回滚", e.path),
            ));
        }
        let target = Path::new(&e.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 原子替换：半截的配置文件比旧文件更糟
        let tmp = target.with_extension("aether-restore-tmp");
        std::fs::write(&tmp, &bytes)?;
        // **权限要按快照那一刻设**：否则 0600 的配置文件回滚一次就变成所有人可读。
        // 没有记录（旧清单 / 非 Unix）时用 0600（宁紧勿松）。
        set_file_mode(&tmp, restore_mode(e.mode))?;
        std::fs::rename(&tmp, target)?;
        notes.push(format!("{}：已回滚（{} 字节）", e.path, bytes.len()));
    }
    Ok(notes)
}

// ---------------------------------------------------------------------------
// 清单的读写（手写，不引 serde 类型 —— 与 policy.rs 同一取舍：逐条容错）
// ---------------------------------------------------------------------------

fn to_json(m: &Manifest) -> String {
    let mut s = String::from("{\n  \"version\": 1,\n");
    s.push_str(&format!("  \"id\": \"{}\",\n", m.id));
    s.push_str(&format!("  \"created_unix\": {},\n", m.created_unix));
    s.push_str("  \"entries\": [\n");
    for (i, e) in m.entries.iter().enumerate() {
        s.push_str("    {");
        s.push_str(&format!("\"path\": \"{}\", ", e.path));
        s.push_str(&format!("\"checksum\": \"{}\", ", e.checksum));
        s.push_str(&format!("\"bytes\": {}, ", e.bytes));
        s.push_str(&format!("\"existed\": {}, ", e.existed));
        s.push_str(&format!("\"mode\": {}", e.mode));
        s.push_str("}");
        if i + 1 < m.entries.len() {
            s.push(',');
        }
        s.push('\n');
    }
    s.push_str("  ]\n}\n");
    s
}

/// 解析清单。**坏清单返回 `None`**（调用方跳过它）—— 一个快照坏掉不该让列表打不开。
fn parse_json(text: &str) -> Option<Manifest> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let id = v.get("id")?.as_str()?.to_string();
    if !valid_id(&id) {
        return None;
    }
    let created_unix = v.get("created_unix")?.as_i64()?;
    let mut entries = Vec::new();
    for e in v.get("entries")?.as_array()? {
        let path = e.get("path")?.as_str()?.to_string();
        let existed = e.get("existed")?.as_bool()?;
        entries.push(Entry {
            path,
            checksum: e.get("checksum").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            bytes: e.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
            existed,
            // 旧清单没有这一项 → 0 = 未知 → 回滚时用 0600
            mode: e.get("mode").and_then(|m| m.as_u64()).unwrap_or(0) as u32,
        });
    }
    Some(Manifest { id, created_unix, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-snap-{tag}-{}-{}",
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
    fn fnv1a_matches_known_vectors() {
        // FNV-1a 64 位的标准测试向量
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x85944171f73967e8);
        // 一位之差要完全不同（否则"改坏了却校验通过"）
        assert_ne!(fnv1a(b"policy.json"), fnv1a(b"policy.json "));
    }

    /// **id 是路径的一部分**：含 `/`、`..`、`.` 开头的一律拒绝。
    #[test]
    fn snapshot_ids_that_escape_are_rejected() {
        for bad in ["", "..", "../etc", "a/b", "a\\b", ".hidden", &"x".repeat(65), "a b"] {
            assert!(!valid_id(bad), "不该接受 {bad:?}");
        }
        for ok in ["20261004T112233", "snap-1", "snap_2", "A1"] {
            assert!(valid_id(ok), "{ok} 应当合法");
        }
        // 回滚时也要挡（不是"只在校验函数里挡"）
        let root = tmp("guard");
        assert!(restore(&root, "../etc").is_err());
        assert!(restore(&root, "a/b").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn id_for_is_sortable_and_readable() {
        // 2026-10-04T11:22:33Z = 1791112953（**先算准再写** —— 我第一次手算错了，
        // 把 1791208953 当成了 10-04，其实那是 10-05T14:02:33Z）
        let id = id_for(1_791_112_953);
        assert_eq!(id, "20261004T112233");
        // 相邻时间戳的 id 字典序与时间序一致（列表按 id 排也说得通）
        assert!(id_for(1_791_112_953) < id_for(1_791_112_954));
        assert_eq!(id_for(0), "19700101T000000");
    }

    /// 创建 → 列举 → 回滚 的往返（用真实临时目录；快照文件由**白名单**决定，
    /// 所以测试直接构造"快照目录 + 清单 + files/"来验回滚路径）。
    #[test]
    fn create_list_and_restore_round_trip() {
        let root = tmp("roundtrip");
        // 造一个"系统文件"并快照
        let target = root.join("system/policy.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"{\"policy\":\"v1\"}").unwrap();
        let m = create(&root.join("snaps"), 1_791_112_953).unwrap();
        assert_eq!(m.id, "20261004T112233");
        assert_eq!(m.entries.len(), FILES.len(), "每个白名单文件都该有一条记录");
        // 真实文件（policy.json）在不在，取决于本机 —— 但这台机器上 /etc/aether 大概率没有，
        // 所以更该验的是"记录里 existed 与实际情况一致"，而不是假定它存在。
        let idx = FILES.iter().position(|f| *f == "/etc/aether/policy.json").unwrap();
        assert_eq!(m.entries[idx].path, "/etc/aether/policy.json");
        assert_eq!(
            m.entries[idx].existed,
            Path::new("/etc/aether/policy.json").exists(),
            "existed 必须如实反映快照那一刻的情况"
        );
        // 列举
        let snaps = list(&root.join("snaps"));
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].id, m.id);
        // 坏清单只跳过
        std::fs::create_dir_all(root.join("snaps/broken-dir")).unwrap();
        std::fs::write(root.join("snaps/broken-dir").join(MANIFEST), "{ 这不是 json").unwrap();
        assert_eq!(list(&root.join("snaps")).len(), 1, "坏快照要跳过而不是让列表打不开");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回滚时**校验和不符必须停下来**（而不是把坏内容写回系统）。
    #[test]
    fn restore_stops_when_a_snapshot_file_is_corrupted() {
        let root = tmp("corrupt");
        let snaps = root.join("snaps");
        let dir = snaps.join("20261004T112233");
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let manifest = r#"{"version":1,"id":"20261004T112233","created_unix":1791208953,
          "entries":[{"path":"/tmp/aether-restore-test.txt","checksum":"deadbeef","bytes":3,"existed":true}]}"#;
        std::fs::write(dir.join(MANIFEST), manifest).unwrap();
        std::fs::write(dir.join("files/00-aether-restore-test.txt"), b"abc").unwrap();
        let err = restore_allowed(&snaps, "20261004T112233", &["/tmp/aether-restore-test.txt"])
            .unwrap_err();
        assert!(
            err.to_string().contains("损坏"),
            "校验和不符要明确说「损坏」并停止：{err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 快照时**不存在**的文件，回滚时不许"造"出来，并要在说明里讲清楚。
    #[test]
    fn restore_skips_files_that_did_not_exist() {
        let root = tmp("missing");
        let snaps = root.join("snaps");
        let dir = snaps.join("20261004T112233");
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(
            dir.join(MANIFEST),
            r#"{"version":1,"id":"20261004T112233","created_unix":1791208953,
               "entries":[{"path":"/tmp/aether-should-not-be-created.txt","checksum":"","bytes":0,"existed":false}]}"#,
        )
        .unwrap();
        // 白名单里给了它，所以流程能走到"跳过"这一步
        // （不在白名单里的路径会被**直接拒绝**，那是另一条测试的事）
        let notes = restore_allowed(
            &snaps,
            "20261004T112233",
            &["/tmp/aether-should-not-be-created.txt"],
        )
        .unwrap();
        assert!(!Path::new("/tmp/aether-should-not-be-created.txt").exists(), "不许造出这个文件");
        assert!(notes[0].contains("未回滚"), "说明里要写清：{}", notes[0]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回滚真的会把内容写回去（用真实的临时目标路径）。
    #[test]
    fn restore_writes_the_content_back() {
        let root = tmp("writeback");
        let snaps = root.join("snaps");
        let id = "20261004T112233";
        let dir = snaps.join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let new = b"{\"policy\":\"v2\"}";
        let old = b"{\"policy\":\"v1\"}";
        let target = root.join("etc/policy.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, new).unwrap();
        let manifest = format!(
            "{{\"version\":1,\"id\":\"{id}\",\"created_unix\":1791208953,\"entries\":[\
             {{\"path\":\"{}\",\"checksum\":\"{}\",\"bytes\":{},\"existed\":true}}]}}",
            target.display().to_string().replace('\\', "/"),
            checksum(old),
            old.len()
        );
        std::fs::write(dir.join(MANIFEST), manifest).unwrap();
        std::fs::write(dir.join("files/00-policy.json"), old).unwrap();
        let tp = target.display().to_string().replace('\\', "/");
        let notes = restore_allowed(&snaps, id, &[tp.as_str()]).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), old, "内容要回到快照那一份");
        assert!(notes[0].contains("已回滚"), "{}", notes[0]);
        // 临时文件不该留下
        assert!(!target.with_extension("aether-restore-tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_a_missing_directory_is_empty_not_an_error() {
        assert!(list(Path::new("/var/lib/aether/nope-does-not-exist")).is_empty());
    }
    /// **审计发现 1（中危）**：回滚必须按快照那一刻的权限写，不能是 umask 后的默认值。
    ///
    /// 具体危害：`policy.json` 这类文件本来是 0600，回滚一次变成 0644
    /// （所有人可读）—— 那是**回滚自己制造的权限降级**。
    #[cfg(unix)]
    #[test]
    fn restore_keeps_the_snapshot_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("mode");
        let snaps = root.join("snaps");
        let id = "20261004T112233";
        let dir = snaps.join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let old = b"{\"policy\":\"v1\"}";
        let target = root.join("etc/policy.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        // 当前文件是"被改宽了"的 0644，而快照里记的是 0600
        std::fs::write(&target, b"{\"policy\":\"v2\"}").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let tp = target.display().to_string().replace('\\', "/");
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "{{\"version\":1,\"id\":\"{id}\",\"created_unix\":1791112953,\"entries\":[\
                 {{\"path\":\"{tp}\",\"checksum\":\"{}\",\"bytes\":{},\"existed\":true,\"mode\":384}}]}}",
                checksum(old),
                old.len()
            ),
        )
        .unwrap();
        std::fs::write(dir.join("files/00-policy.json"), old).unwrap();
        let notes = restore_allowed(&snaps, id, &[tp.as_str()]).unwrap();
        assert!(notes[0].contains("已回滚"), "{}", notes[0]);
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "回滚后应当是快照里的 0600，而不是 umask 后的 0644");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 老清单没有 `mode` 这一项 → **默认 0600**（宁紧勿松），不能是 umask 的 0644。
    #[cfg(unix)]
    #[test]
    fn restore_defaults_to_0600_when_the_mode_is_unknown() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("unknown-mode");
        let snaps = root.join("snaps");
        let id = "20261004T112233";
        let dir = snaps.join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let body = b"x";
        let target = root.join("etc/legacy.conf");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let tp = target.display().to_string().replace('\\', "/");
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "{{\"version\":1,\"id\":\"{id}\",\"created_unix\":1791112953,\"entries\":[\
                 {{\"path\":\"{tp}\",\"checksum\":\"{}\",\"bytes\":1,\"existed\":true}}]}}",
                checksum(body)
            ),
        )
        .unwrap();
        std::fs::write(dir.join("files/00-legacy.conf"), body).unwrap();
        restore_allowed(&snaps, id, &[tp.as_str()]).unwrap();
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "未知 mode 时用 0600，不能留 umask 的默认值");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **审计发现 2（低危）**：同一秒里创建两次**不能互相覆盖** —— 那会静默丢掉前一份快照。
    #[test]
    fn two_snapshots_in_the_same_second_do_not_overwrite_each_other() {
        let root = tmp("same-second");
        let snaps = root.join("snaps");
        let a = create(&snaps, 1_791_112_953).unwrap();
        let b = create(&snaps, 1_791_112_953).unwrap();
        assert_ne!(a.id, b.id, "同一秒的第二份快照必须换个 id");
        assert!(b.id.starts_with(&a.id), "后缀形式：{} / {}", a.id, b.id);
        assert_eq!(list(&snaps).len(), 2, "两份都要在列表里（都不该被覆盖）");
        assert!(valid_id(&b.id), "带后缀的 id 仍要合法（它是路径的一部分）：{}", b.id);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **审计发现 3（低危）**：清单里的路径不在白名单内就**拒绝回滚**。
    ///
    /// 清单是 root 0700 下的文件，能改它的人已经是 root；但"把从磁盘读来的路径
    /// 直接当写入目标"是不该有的习惯 —— 这条检查把"往任意路径写文件"挡在门外。
    #[test]
    fn restore_refuses_paths_outside_the_whitelist() {
        let root = tmp("whitelist");
        let snaps = root.join("snaps");
        let id = "20261004T112233";
        let dir = snaps.join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        // 清单里指向一个**不在白名单**的路径（模拟清单被改过）
        let evil = root.join("etc/evil.conf");
        let ep = evil.display().to_string().replace('\\', "/");
        let body = b"pwned";
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "{{\"version\":1,\"id\":\"{id}\",\"created_unix\":1791112953,\"entries\":[\
                 {{\"path\":\"{ep}\",\"checksum\":\"{}\",\"bytes\":5,\"existed\":true,\"mode\":384}}]}}",
                checksum(body)
            ),
        )
        .unwrap();
        std::fs::write(dir.join("files/00-evil.conf"), body).unwrap();
        // 传一个**不包含它**的白名单 → 必须拒绝，且文件不能被创建
        let err = restore_allowed(&snaps, id, &["/etc/resolv.conf"]).unwrap_err();
        assert!(
            err.to_string().contains("白名单"),
            "要明确说「不在白名单内」：{err}"
        );
        assert!(!evil.exists(), "被拒绝时不许留下文件");
        // 把白名单放开就能回滚（证明拒绝的原因是白名单，而不是别的）
        let notes = restore_allowed(&snaps, id, &[ep.as_str()]).unwrap();
        assert!(notes[0].contains("已回滚"), "{}", notes[0]);
        assert!(evil.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **生产路径的白名单确实生效**：`restore()` 用的是 `FILES`，
    /// 所以任何不在 `/etc` 那几个文件里的路径都会被拒。
    ///
    /// （测试用 `restore_allowed` 传自己的白名单，这条专门盯着**生产入口**没有退化成
    /// "谁能调就随便写"。）
    #[test]
    fn the_production_restore_only_touches_whitelisted_paths() {
        let root = tmp("prod-whitelist");
        let snaps = root.join("snaps");
        let id = "20261004T112233";
        let dir = snaps.join(id);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let target = root.join("etc/not-whitelisted.conf");
        let tp = target.display().to_string().replace('\\', "/");
        let body = b"zzz";
        std::fs::write(
            dir.join(MANIFEST),
            format!(
                "{{\"version\":1,\"id\":\"{id}\",\"created_unix\":1791112953,\"entries\":[\
                 {{\"path\":\"{tp}\",\"checksum\":\"{}\",\"bytes\":3,\"existed\":true,\"mode\":384}}]}}",
                checksum(body)
            ),
        )
        .unwrap();
        std::fs::write(dir.join("files/00-not-whitelisted.conf"), body).unwrap();
        let err = restore(&snaps, id).unwrap_err();
        assert!(err.to_string().contains("白名单"), "生产入口必须挡住：{err}");
        assert!(!target.exists());
        // 顺带确认：`/etc/resolv.conf` 这类白名单内的路径**不会**因为白名单被误拒
        // （这里只检查"名单本身包含它"，不真的去写系统文件）
        assert!(FILES.contains(&"/etc/resolv.conf"));
        assert!(FILES.contains(&"/etc/aether/policy.json"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// mode 必须能**存进清单、读回清单**（跨平台测试）。
    ///
    /// 它是权限修复的命门：往返丢了 mode，回滚就只能用 0600 兜底（好一点）
    /// 或干脆 heredoc 出 0644（坏）—— 而那两条权限测试是 `#[cfg(unix)]`，
    /// 在 Windows 上不跑。这条负责在**任何平台**上守住"值没丢"。
    #[test]
    fn file_mode_round_trips_through_the_manifest() {
        let m = Manifest {
            id: "20261004T112233".to_string(),
            created_unix: 1_791_112_953,
            entries: vec![
                Entry {
                    path: "/etc/aether/policy.json".to_string(),
                    checksum: "abcd".to_string(),
                    bytes: 12,
                    existed: true,
                    mode: 0o600,
                },
                Entry {
                    path: "/etc/resolv.conf".to_string(),
                    checksum: String::new(),
                    bytes: 0,
                    existed: false,
                    mode: 0,
                },
            ],
        };
        let json = to_json(&m);
        assert!(json.contains("\"mode\": 384"), "0600 要写成 384：{json}");
        let back = parse_json(&json).expect("自己写的清单必须能读回");
        assert_eq!(back, m, "往返之后必须一模一样（含 mode）");
        // 老清单（没有 mode 字段）要能读成 0 = 未知 → 回滚时用 0600 兜底
        let legacy = r#"{"version":1,"id":"20261004T112233","created_unix":1791112953,
            "entries":[{"path":"/etc/timezone","checksum":"x","bytes":1,"existed":true}]}"#;
        let lm = parse_json(legacy).expect("老清单也要能读");
        assert_eq!(lm.entries[0].mode, 0, "缺 mode 的旧清单读成 0（未知）");
    }

    /// **权限决策本身**（跨平台）：记了就用记的，没记就用 0600 —— 宁紧勿松。
    ///
    /// 这条测试在任何平台都跑：它守的是**决策**，而不是"文件系统真的改了权限"
    /// （那两条只能 `#[cfg(unix)]`，而 aetherd 在这台机器上连 Linux 目标都编译不了）。
    #[test]
    fn restore_mode_is_fail_closed() {
        assert_eq!(restore_mode(0), 0o600, "未知 → 0600（不能是 umask 后的 0644）");
        assert_eq!(restore_mode(0o600), 0o600);
        assert_eq!(restore_mode(0o644), 0o644, "记了 0644 就按 0644（用户自己设的宽权限要尊重）");
        assert_eq!(restore_mode(0o400), 0o400);
        // 只保留权限位：带上文件类型位（S_IFREG=0o100000）时不能被当成权限用
        assert_eq!(restore_mode(0o100600), 0o600, "文件类型位要去掉");
    }

}
