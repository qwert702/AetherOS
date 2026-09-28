//! 回收站：删除先进这里，可恢复。
//!
//! 为什么必须有：4.1 引入的是 **AI 能删文件** 的能力。没有回收站，"删错了"就是
//! 不可逆的数据丢失；有了它，最坏情况只是"多了一步恢复"。
//!
//! 布局（每个条目一个目录，避免元数据与内容混在一起）：
//!
//! ```text
//! <trash_root>/
//!   1759000000-1/
//!     origin     ← 原绝对路径（纯文本，恢复时用）
//!     payload    ← 原文件或目录（原样搬过来）
//! ```
//!
//! **容量策略**：条目数 + 总字节 + 存活天数，三者任一超限都从**最旧的**开始清。
//! 回收站自己没上限的话，它只是把"磁盘满"从用户目录挪到了回收站 —— 那是把问题
//! 推迟，不是解决。

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 回收站根目录。
///
/// Linux 用持久化分区上的 `/var/trash`（与审计日志同盘，跨重启保留）；
/// 开发机用临时目录。**放在用户数据区之外**是有意的：否则 AI 的写操作
/// 有可能把回收站本身也删掉。
pub fn default_root() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/var/trash")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("aether-trash")
    }
}

/// 保留策略。
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_age: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_entries: 200,
            max_bytes: 256 * 1024 * 1024, // 256 MB
            max_age: Duration::from_secs(7 * 24 * 3600), // 7 天
        }
    }
}

/// 回收站里的一个条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 条目名（`<unix 秒>-<序号>`），恢复时用它定位
    pub name: String,
    /// 原绝对路径
    pub origin: String,
    /// 移入时刻（unix 秒）
    pub created: u64,
    /// 占用字节（目录递归求和）
    pub bytes: u64,
    pub is_dir: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 生成不冲突的条目名。名字以时间戳开头，因此**字符串倒序即时间倒序**，
/// 清理时不必额外排序。
fn entry_name(root: &Path) -> Result<String> {
    let ts = now_secs();
    for seq in 1..1000u32 {
        let name = format!("{ts}-{seq}");
        if !root.join(&name).exists() {
            return Ok(name);
        }
    }
    bail!("回收站条目名冲突过多（同一秒内 1000 次删除）")
}

/// 递归求占用字节。读不到的部分按 0 计 —— 统计不该成为失败原因。
fn size_of(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten().map(|e| size_of(&e.path())).sum()
}

/// 把 `path` 移入回收站，返回条目名。
///
/// 用 `rename` 而不是"复制 + 删除"：同盘 rename 是原子的，中途断电也不会
/// 出现"原文件没了、回收站里也没有"的窗口。
pub fn trash(root: &Path, path: &Path) -> Result<String> {
    let abs = std::fs::canonicalize(path)
        .with_context(|| format!("无法访问 {}", path.display()))?;
    let abs = crate::tools::strip_verbatim_prefix(abs);

    // 拒绝删除根：`remove_dir_all("/")` 之类的事故不值得给它机会
    if abs.parent().is_none() {
        bail!("拒绝删除根目录 {}", abs.display());
    }

    std::fs::create_dir_all(root)
        .with_context(|| format!("无法创建回收站 {}", root.display()))?;
    let name = entry_name(root)?;
    let dir = root.join(&name);
    std::fs::create_dir_all(&dir)?;

    // 先写 origin 再搬 payload：中途失败最多留下一个**空条目**，
    // 而不是"数据没了但不知道原来在哪"
    std::fs::write(dir.join("origin"), abs.to_string_lossy().as_bytes())?;
    if let Err(e) = std::fs::rename(&abs, dir.join("payload")) {
        let _ = std::fs::remove_dir_all(&dir); // 回滚空条目
        return Err(anyhow::anyhow!("移入回收站失败（{}）: {e}", abs.display()));
    }
    Ok(name)
}

/// 列出回收站条目，**最新在前**。
pub fn list(root: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let dir = e.path();
            if !dir.is_dir() {
                return None;
            }
            let origin = std::fs::read_to_string(dir.join("origin")).ok()?;
            let origin = origin.trim().to_string();
            if origin.is_empty() {
                return None;
            }
            let payload = dir.join("payload");
            Some(Entry {
                created: name
                    .split('-')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                bytes: size_of(&payload),
                is_dir: payload.is_dir(),
                origin,
                name,
            })
        })
        .collect();
    // 名字 = "<时间戳>-<序号>"，倒序即最新在前
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

/// 从回收站恢复到原路径，返回恢复后的路径。
pub fn restore(root: &Path, name: &str) -> Result<PathBuf> {
    // 条目名由我们自己生成，但仍然校验 —— 它可能来自 IPC 输入
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        bail!("非法的回收站条目名: {name}");
    }
    let dir = root.join(name);
    let origin = std::fs::read_to_string(dir.join("origin"))
        .with_context(|| format!("回收站条目不存在或已损坏: {name}"))?;
    let origin = PathBuf::from(origin.trim());
    if origin.as_os_str().is_empty() {
        bail!("条目 {name} 的原路径为空");
    }
    // 不覆盖现有文件：宁可让用户先处理，也不要静默盖掉
    if origin.exists() {
        bail!("原路径已被占用，未恢复: {}", origin.display());
    }
    if let Some(parent) = origin.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法重建目录 {}", parent.display()))?;
    }
    std::fs::rename(dir.join("payload"), &origin)
        .with_context(|| format!("恢复失败: {}", origin.display()))?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(origin)
}

/// 按策略清理，返回清理掉的条目数。
pub fn purge(root: &Path, policy: &Policy) -> usize {
    let mut keep = list(root); // 最新在前
    let now = now_secs();
    let mut removed = 0;

    // 1. 超龄（用 `>=`：存活时间**达到**上限即清。用 `>` 的话 `max_age = 0`
    //    永远清不掉东西 —— 同一秒内 `now - created == 0`）
    let mut alive = Vec::with_capacity(keep.len());
    for e in keep.drain(..) {
        if now.saturating_sub(e.created) >= policy.max_age.as_secs() {
            if remove_entry(root, &e.name) {
                removed += 1;
            }
        } else {
            alive.push(e);
        }
    }
    keep = alive;

    // 2. 条目数（keep 最新在前，从尾部删 = 删最旧的）
    while keep.len() > policy.max_entries {
        let Some(e) = keep.pop() else { break };
        if remove_entry(root, &e.name) {
            removed += 1;
        }
    }

    // 3. 总字节
    let mut total: u64 = keep.iter().map(|e| e.bytes).sum();
    while total > policy.max_bytes {
        let Some(e) = keep.pop() else { break };
        total = total.saturating_sub(e.bytes);
        if remove_entry(root, &e.name) {
            removed += 1;
        }
    }
    removed
}

fn remove_entry(root: &Path, name: &str) -> bool {
    std::fs::remove_dir_all(root.join(name)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aether_trash_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        d
    }

    #[test]
    fn trash_then_restore_roundtrip() {
        let d = tmp("roundtrip");
        let root = d.join("trash");
        let f = d.join("note.txt");
        std::fs::write(&f, b"hello").unwrap();

        let name = trash(&root, &f).expect("应能移入回收站");
        assert!(!f.exists(), "原文件应已被移走");

        let entries = list(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, name);
        assert!(entries[0].origin.ends_with("note.txt"), "origin 应记录原路径");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].bytes, 5);

        let back = restore(&root, &name).expect("应能恢复");
        assert_eq!(back, f);
        assert_eq!(std::fs::read(&f).unwrap(), b"hello");
        assert!(list(&root).is_empty(), "恢复后条目应消失");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn trash_directory_works() {
        let d = tmp("dir");
        let root = d.join("trash");
        let sub = d.join("folder");
        std::fs::create_dir_all(sub.join("inner")).unwrap();
        std::fs::write(sub.join("inner/a.txt"), b"x").unwrap();

        let name = trash(&root, &sub).expect("应能移入回收站");
        assert!(!sub.exists());
        let e = &list(&root)[0];
        assert!(e.is_dir, "应识别为目录");
        assert_eq!(e.bytes, 1, "目录大小应递归求和");

        restore(&root, &name).unwrap();
        assert!(sub.join("inner/a.txt").exists(), "目录结构应完整恢复");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 恢复不能覆盖已存在的文件 —— 宁可让用户先处理，也不要静默盖掉。
    #[test]
    fn restore_refuses_to_overwrite() {
        let d = tmp("overwrite");
        let root = d.join("trash");
        let f = d.join("dup.txt");
        std::fs::write(&f, b"original").unwrap();
        let name = trash(&root, &f).unwrap();
        std::fs::write(&f, b"new content").unwrap(); // 原路径被新文件占用

        let err = restore(&root, &name).expect_err("应拒绝覆盖");
        assert!(err.to_string().contains("已被占用"), "实得 {err}");
        assert_eq!(std::fs::read(&f).unwrap(), b"new content", "新文件不应被动过");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 条目名来自外部输入，必须挡住路径穿越。
    #[test]
    fn restore_rejects_path_traversal() {
        let d = tmp("traversal");
        let root = d.join("trash");
        std::fs::create_dir_all(&root).unwrap();
        for bad in ["../etc", "a/b", "a\\b", "..", ""] {
            assert!(restore(&root, bad).is_err(), "应拒绝 {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn purge_removes_oldest_over_entry_limit() {
        let d = tmp("purge_count");
        let root = d.join("trash");
        for i in 0..5 {
            let f = d.join(format!("f{i}.txt"));
            std::fs::write(&f, b"x").unwrap();
            trash(&root, &f).unwrap();
            // 让条目名的时间戳/序号有区分度
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(list(&root).len(), 5);

        let policy = Policy { max_entries: 2, max_bytes: u64::MAX, max_age: Duration::from_secs(3600) };
        let removed = purge(&root, &policy);
        assert_eq!(removed, 3, "应清掉 3 个最旧的");
        let left = list(&root);
        assert_eq!(left.len(), 2);
        // 留下的是最新的两个（f4、f3）
        assert!(left.iter().any(|e| e.origin.ends_with("f4.txt")));
        assert!(left.iter().any(|e| e.origin.ends_with("f3.txt")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn purge_removes_over_byte_limit() {
        let d = tmp("purge_bytes");
        let root = d.join("trash");
        for i in 0..3 {
            let f = d.join(format!("big{i}.bin"));
            std::fs::write(&f, vec![0u8; 100]).unwrap();
            trash(&root, &f).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let policy = Policy { max_entries: 100, max_bytes: 250, max_age: Duration::from_secs(3600) };
        let removed = purge(&root, &policy);
        assert_eq!(removed, 1, "3×100 字节超过 250 上限，应清掉 1 个");
        let left: u64 = list(&root).iter().map(|e| e.bytes).sum();
        assert!(left <= 250, "剩余 {left} 字节应在上限内");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn purge_removes_expired() {
        let d = tmp("purge_age");
        let root = d.join("trash");
        let f = d.join("old.txt");
        std::fs::write(&f, b"x").unwrap();
        trash(&root, &f).unwrap();

        // max_age = 0：任何条目都已超龄
        let policy = Policy { max_entries: 100, max_bytes: u64::MAX, max_age: Duration::ZERO };
        assert_eq!(purge(&root, &policy), 1);
        assert!(list(&root).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 同一秒内连续删除不能互相覆盖（条目名带序号）。
    #[test]
    fn same_second_entries_do_not_collide() {
        let d = tmp("collide");
        let root = d.join("trash");
        for i in 0..3 {
            let f = d.join(format!("s{i}.txt"));
            std::fs::write(&f, b"x").unwrap();
            trash(&root, &f).unwrap();
        }
        assert_eq!(list(&root).len(), 3, "同秒内三次删除应产生三个条目");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn list_on_missing_root_is_empty_not_error() {
        let d = tmp("missing");
        assert!(list(&d.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
