//! 跨进程剪贴板（2.2）：aetherd 持有的全局剪贴板。
//!
//! 为什么是进程级 `static` 而不是塞进 `ToolCtx`：`ToolCtx` 每个 agent 回合都会重建，
//! 第 1 回合写入的内容到第 2 回合就没了 —— 剪贴板的生命周期必须与守护进程一致。
//! aetherd 是单用户守护进程，进程级状态与它的定位一致。
//!
//! 隐私边界（诚实版）：剪贴板经常被用来复制密码。读它 = 读取用户可能敏感的数据，
//! 所以 `clipboard_read` 工具标记为 `sensitive_output`（读到即强制本地推理，不上云），
//! 且每次读写都落审计日志。但与 ui.key 同理：能连上 7311 的本机进程仍可读写，
//! 彻底解决要等 IPC 用户级认证（M4）。

use std::sync::Mutex;

/// 长度上限：剪贴板里放一部小说都不该被拒，但也不该被当成内存存储。
pub const MAX_BYTES: usize = 64 * 1024;

static CLIPBOARD: Mutex<String> = Mutex::new(String::new());

/// 写入剪贴板，返回写入的字节数。超长整体拒绝（不做截断 ——
/// 截断会让用户在不知情的情况下复制出残缺数据，那是比拒绝更糟的失败）。
pub fn set(text: &str) -> Result<usize, String> {
    let bytes = text.len();
    if bytes > MAX_BYTES {
        return Err(format!(
            "剪贴板内容 {bytes} 字节，超过上限 {MAX_BYTES}（拒绝整段，不做截断）"
        ));
    }
    *CLIPBOARD
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = text.to_string();
    Ok(bytes)
}

/// 读取剪贴板（空则返回空串）。
pub fn get() -> String {
    CLIPBOARD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_roundtrip() {
        assert!(set("hello 剪贴板").is_ok());
        assert_eq!(get(), "hello 剪贴板");
    }

    #[test]
    fn set_overwrites_previous() {
        set("first").unwrap();
        set("second").unwrap();
        assert_eq!(get(), "second");
    }

    #[test]
    fn oversized_is_rejected_wholesale() {
        let big = "x".repeat(MAX_BYTES + 1);
        assert!(set(&big).is_err(), "超长必须整体拒绝");
        // 拒绝后原内容不受影响
        set("keep").unwrap();
        assert!(set(&big).is_err());
        assert_eq!(get(), "keep");
    }

    #[test]
    fn exact_cap_is_allowed() {
        let exact = "y".repeat(MAX_BYTES);
        assert!(set(&exact).is_ok());
    }

    #[test]
    fn utf8_counted_by_bytes_not_chars() {
        // 两个中文 = 6 字节；按字符计数会放行超限内容
        let s = "中".repeat(MAX_BYTES / 3 + 1);
        assert!(set(&s).is_err());
    }
}
