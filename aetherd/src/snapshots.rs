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
}

/// 一次快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub created_unix: i64,
    pub entries: Vec<Entry>,
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
    let id = id_for(now_unix);
    let dir = root.join(&id);
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
        s.push_str(&format!("\"existed\": {}", e.existed));
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
        let err = restore(&snaps, "20261004T112233").unwrap_err();
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
        let notes = restore(&snaps, "20261004T112233").unwrap();
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
        let notes = restore(&snaps, id).unwrap();
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
}
