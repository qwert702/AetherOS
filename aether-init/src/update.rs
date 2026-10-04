//! 应用暂存的更新（FEATURE-GAPS §3.1 的应用阶段）。
//!
//! ## 为什么由 PID 1 做
//!
//! 应用更新要**覆盖系统路径**，只能在"其他服务都还没起来"时做 ——
//! 否则会出现"旧进程拿着新二进制"。位置：`mount_persist()` 之后、`run_loop()` 之前。
//!
//! ## 就绪协议
//!
//! `aetherd` 暂存时**最后**写 `apply.list`，每行 `<sha256>\t<bytes>\t<target>`。
//! **该文件存在 = 暂存完整**。这样 init 不需要 JSON 解析器，也不必把两个 crate
//! 的更新逻辑耦合成共享 crate。
//!
//! tab 作分隔符是安全的：路径校验**拒绝一切控制字符**（含 `\t`），
//! 所以路径里不可能出现 tab。**这条依赖写在这里** —— 哪天放宽控制字符检查，格式要跟着改。
//!
//! ## 为什么这里不再验一遍哈希
//!
//! 暂存目录只有 root 能写，哈希在 `stage()` 时已经验过。init 只做**大小核对**
//! （能发现截断）+ 原子替换。**若将来允许非 root 写暂存目录，这条必须改。**
//!
//! ## 可重入
//!
//! 重复应用同一份暂存是安全的（再拷一次同样的内容），
//! 所以"应用到一半掉电"不会留下不可恢复的状态 —— 标记还在，下次启动继续。

#![cfg_attr(not(target_os = "linux"), allow(dead_code))] // 调用点在 Linux 启动路径里
use std::path::{Path, PathBuf};

/// 更新根目录（与 `aetherd::update::UPDATE_ROOT` 是同一处；
/// 两个 crate 各持常量，靠测试与文档保证一致，不引入跨 crate 依赖）。
pub const UPDATE_ROOT: &str = "/var/lib/aether/update";
pub const STAGED: &str = "staged";
/// 待应用表（**存在即表示暂存完整**）。
pub const APPLY_LIST: &str = "apply.list";
/// 应用记录（追加）。
pub const APPLIED_LOG: &str = "applied.log";

/// 一条待应用记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyEntry {
    pub sha256: String,
    pub bytes: u64,
    pub target: String,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// 目标路径是否**看起来**安全（与 aetherd 侧同一套规则 —— 重复一遍是刻意的）。
///
/// PID 1 直接按这个路径写文件，而它读的是磁盘上的文本：
/// 不该假设"上游一定校验过"。
pub fn target_is_plausible(path: &str) -> bool {
    const PREFIXES: &[&str] = &["/var/apps/", "/etc/aether/", "/var/lib/aether/"];
    path.starts_with('/')
        && !path.contains("..")
        && !path.chars().any(|c| c.is_control())
        && !path.contains("//")
        && PREFIXES.iter().any(|p| path.starts_with(p))
        && !PREFIXES.contains(&path)
}

/// 解析整张待应用表。**任何一行不合法就整张拒绝** —— 不做"应用能应用的那些"。
pub fn parse_apply_list(text: &str) -> Result<Vec<ApplyEntry>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        let (sha, bytes, target, extra) = (f.next(), f.next(), f.next(), f.next());
        if extra.is_some() {
            return Err(format!("第 {} 行：字段过多（路径里不该有 tab）", i + 1));
        }
        let (sha, bytes, target) = match (sha, bytes, target) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => {
                return Err(format!(
                    "第 {} 行格式不对（应为 sha256<TAB>bytes<TAB>target）",
                    i + 1
                ))
            }
        };
        if !is_hex64(sha) {
            return Err(format!("第 {} 行：sha256 形态不对", i + 1));
        }
        let bytes: u64 = bytes
            .parse()
            .map_err(|_| format!("第 {} 行：bytes 不是数字", i + 1))?;
        if !target_is_plausible(target) {
            return Err(format!("第 {} 行：目标路径不可信（{target:?}）", i + 1));
        }
        out.push(ApplyEntry {
            sha256: sha.to_string(),
            bytes,
            target: target.to_string(),
        });
    }
    if out.is_empty() {
        return Err("待应用表里没有任何条目".to_string());
    }
    Ok(out)
}

/// 有没有待应用的更新。
pub fn has_pending(root: &Path) -> bool {
    root.join(APPLY_LIST).is_file()
}

/// 暂存文件在暂存目录里的位置。
pub fn staged_path(root: &Path, target: &str) -> PathBuf {
    root.join(STAGED).join(target.trim_start_matches('/'))
}

/// 目标路径在**给定系统根**下的实际位置（生产传 `/`，测试传临时目录）。
pub fn target_under(sys_root: &Path, target: &str) -> PathBuf {
    sys_root.join(target.trim_start_matches('/'))
}

/// 应用暂存的更新（系统根 = `/`）。
pub fn apply_staged(root: &Path, now_unix: i64) -> Result<Vec<String>, String> {
    apply_staged_with(root, Path::new("/"), now_unix)
}

/// 应用暂存的更新，`sys_root` 决定"落在哪个根下面"（测试用）。
///
/// 成功返回给人看的事件；失败返回错误且**保留标记**（下次启动重试）。
pub fn apply_staged_with(
    root: &Path,
    sys_root: &Path,
    now_unix: i64,
) -> Result<Vec<String>, String> {
    let list = root.join(APPLY_LIST);
    let text = std::fs::read_to_string(&list)
        .map_err(|e| format!("读不到 {}：{e}", list.display()))?;
    // ① **先整表校验**：不留"应用了一半才发现有一行是坏的"这种状态
    let entries = parse_apply_list(&text)?;

    // ② 逐个原子替换
    let mut events = Vec::new();
    for e in &entries {
        let src = staged_path(root, &e.target);
        let md = std::fs::metadata(&src)
            .map_err(|err| format!("{}：暂存文件不见了（{err}）", e.target))?;
        if md.len() != e.bytes {
            return Err(format!(
                "{}：暂存文件大小不符（记录 {} / 实际 {}）—— 拒绝应用",
                e.target,
                e.bytes,
                md.len()
            ));
        }
        let dst = target_under(sys_root, &e.target);
        let dir = dst
            .parent()
            .ok_or_else(|| format!("{}：路径没有父目录", e.target))?;
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("建目录失败 {}：{err}", dir.display()))?;
        // 临时文件放**目标同目录**：跨文件系统的 rename 会失败，
        // 而"先写别处再搬"在掉电时会丢内容
        let tmp = dir.join(format!(
            ".{}.aether-update-tmp",
            dst.file_name().and_then(|s| s.to_str()).unwrap_or("file")
        ));
        std::fs::copy(&src, &tmp).map_err(|err| format!("写 {} 失败：{err}", tmp.display()))?;
        std::fs::rename(&tmp, &dst).map_err(|err| {
            let _ = std::fs::remove_file(&tmp);
            format!("替换 {} 失败：{err}", e.target)
        })?;
        events.push(format!("已更新 {}", e.target));
    }

    // ③ 收尾：记一条账 → 清暂存 → **最后**删标记
    let line = format!("applied_unix={now_unix} files={}\n", entries.len());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(APPLIED_LOG))
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
    let staged = root.join(STAGED);
    if staged.exists() {
        std::fs::remove_dir_all(&staged).map_err(|e| format!("清暂存失败：{e}"))?;
    }
    std::fs::remove_file(&list).map_err(|e| format!("删标记失败：{e}"))?;
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-init-update-{tag}-{}-{}",
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

    /// 造一份暂存：`staged/<target>` 放好内容，`apply.list` 写好记录。
    fn stage(root: &Path, target: &str, body: &[u8]) {
        let p = staged_path(root, target);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let prev = std::fs::read_to_string(root.join(APPLY_LIST)).unwrap_or_default();
        std::fs::write(
            root.join(APPLY_LIST),
            format!("{prev}{}\t{}\t{}\n", "a".repeat(64), body.len(), target),
        )
        .unwrap();
    }

    #[test]
    fn parsing_rejects_anything_suspicious() {
        let good = format!("{}\t3\t/var/apps/demo/demo\n", "a".repeat(64));
        assert_eq!(parse_apply_list(&good).unwrap().len(), 1);
        assert!(parse_apply_list(&format!("# 说明\n\n{good}")).is_ok());
        // 目标不在白名单 / 带 .. / 前缀本身 / 相对路径
        for bad in [
            "/etc/shadow",
            "/etc/aether/../../shadow",
            "/etc/aether/",
            "/var/apps/",
            "var/apps/a",
            "",
        ] {
            let line = format!("{}\t1\t{bad}\n", "a".repeat(64));
            assert!(parse_apply_list(&line).is_err(), "{bad:?} 该被拒");
        }
        // 字段太少/太多、哈希形态、bytes 不是数字
        assert!(parse_apply_list("/var/apps/a\n").is_err());
        assert!(parse_apply_list(&format!("{}\t1\n", "a".repeat(64))).is_err());
        assert!(parse_apply_list(&format!("{}\t1\t/var/apps/a\textra\n", "a".repeat(64))).is_err());
        assert!(parse_apply_list("abc\t1\t/var/apps/a\n").is_err());
        assert!(parse_apply_list(&format!("{}\tx\t/var/apps/a\n", "a".repeat(64))).is_err());
        // 只有注释 = 没有条目 → 也拒绝（不当成"有更新"）
        assert!(parse_apply_list("# 只有注释\n").is_err());
    }

    #[test]
    fn no_marker_means_nothing_to_do() {
        let root = tmp("nomarker");
        assert!(!has_pending(&root));
        assert!(apply_staged_with(&root, &root.join("sys"), 1).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正常应用：文件落到目标、标记被清掉、账记上。
    #[test]
    fn applies_files_then_clears_the_marker() {
        let root = tmp("apply");
        let sys = root.join("sys");
        stage(&root, "/var/apps/demo/demo", b"new binary");
        stage(&root, "/etc/aether/version", b"1.0.1\n");
        assert!(has_pending(&root));
        let events = apply_staged_with(&root, &sys, 1_700_000_000).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            std::fs::read(sys.join("var/apps/demo/demo")).unwrap(),
            b"new binary"
        );
        assert_eq!(std::fs::read(sys.join("etc/aether/version")).unwrap(), b"1.0.1\n");
        assert!(!has_pending(&root), "应用后标记要清掉");
        assert!(!root.join(STAGED).exists(), "暂存目录也要清掉");
        let log = std::fs::read_to_string(root.join(APPLIED_LOG)).unwrap();
        assert!(log.contains("applied_unix=1700000000") && log.contains("files=2"), "{log}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **坏行 → 整张拒绝，且一个文件都不写**（不留"应用了一半"的状态）。
    #[test]
    fn a_bad_line_aborts_before_any_write() {
        let root = tmp("badline");
        let sys = root.join("sys");
        stage(&root, "/var/apps/demo/demo", b"good");
        // 追加一行坏目标
        let mut t = std::fs::read_to_string(root.join(APPLY_LIST)).unwrap();
        t.push_str(&format!("{}\t1\t/etc/shadow\n", "b".repeat(64)));
        std::fs::write(root.join(APPLY_LIST), t).unwrap();
        let err = apply_staged_with(&root, &sys, 1).unwrap_err();
        assert!(err.contains("不可信"), "要说清是路径不可信：{err}");
        assert!(!sys.join("var/apps/demo/demo").exists(), "拒绝时不许写任何文件");
        assert!(has_pending(&root), "标记要保留，下次启动还能重试");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 暂存文件被截断（大小不符）→ 拒绝应用并保留标记。
    #[test]
    fn a_truncated_staged_file_is_refused() {
        let root = tmp("truncated");
        let sys = root.join("sys");
        stage(&root, "/var/apps/demo/demo", b"0123456789");
        // 把暂存内容改短（模拟写到一半掉电）
        std::fs::write(staged_path(&root, "/var/apps/demo/demo"), b"012").unwrap();
        let err = apply_staged_with(&root, &sys, 1).unwrap_err();
        assert!(err.contains("大小不符"), "{err}");
        assert!(!sys.join("var/apps/demo/demo").exists());
        assert!(has_pending(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **可重入**：重复应用同一份暂存是安全的（掉电后重试的语义）。
    #[test]
    fn applying_twice_is_safe() {
        let root = tmp("twice");
        let sys = root.join("sys");
        stage(&root, "/var/apps/demo/demo", b"payload");
        apply_staged_with(&root, &sys, 1).unwrap();
        // 再把同样的暂存造出来（模拟"标记没删掉/又放了一份"）
        stage(&root, "/var/apps/demo/demo", b"payload");
        apply_staged_with(&root, &sys, 2).unwrap();
        assert_eq!(std::fs::read(sys.join("var/apps/demo/demo")).unwrap(), b"payload");
        let log = std::fs::read_to_string(root.join(APPLIED_LOG)).unwrap();
        assert_eq!(log.lines().count(), 2, "每次应用都该记一条账");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目标目录不存在时会建出来（首次更新一个新应用时就是这种情况）。
    #[test]
    fn missing_target_directories_are_created() {
        let root = tmp("mkdir");
        let sys = root.join("sys");
        stage(&root, "/var/apps/brand-new/bin/app", b"x");
        apply_staged_with(&root, &sys, 1).unwrap();
        assert_eq!(std::fs::read(sys.join("var/apps/brand-new/bin/app")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&root);
    }
}
