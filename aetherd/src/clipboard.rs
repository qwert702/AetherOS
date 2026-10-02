//! 跨进程剪贴板（2.2）：aetherd 持有的全局剪贴板。
//!
//! 为什么是进程级 `static` 而不是塞进 `ToolCtx`：`ToolCtx` 每个 agent 回合都会重建，
//! 第 1 回合写入的内容到第 2 回合就没了 —— 剪贴板的生命周期必须与守护进程一致。
//! aetherd 是单用户守护进程，进程级状态与它的定位一致。
//!
//! 隐私边界（诚实版）：剪贴板经常被用来复制密码。读它 = 读取用户可能敏感的数据，
//! 所以这里做了四件事：
//! 1. `clipboard_read` 工具标记为 `sensitive_output`（读到即强制本地推理，不上云）；
//! 2. 每次读写都落审计日志；
//! 3. **IPC 端点要求已注册的 UI 通道**（见 `server.rs`），未注册连接一律 403 ——
//!    2026-09-28 第四轮审查前这条入口是零门槛的（P1-1）；
//! 4. 内容有存活上限（见 `TTL`），不永久驻留进程内存。
//!
//! 仍未解决：IPC 尚无用户级认证（M4 规划），所以"能读到 `ui.key` 的本机进程"仍可读写。
//! 这是"提高门槛"，不是完整的多方授权。
//!
//! 2026-10-02 审计（H-2）补上了第 3 条的漏口：门槛此前只加在
//! `Request::ClipboardGet/Set` 两个变体上，而同一份数据经
//! `ToolCall{tool:"clipboard_read"}` 仍可拿到（实测：同一未注册连接走协议端点 403、
//! 走工具路径成功）。现在两个入口共用 `tools::TRUSTED_CHANNEL_TOOLS` 判定。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 长度上限：剪贴板里放一部小说都不该被拒，但也不该被当成内存存储。
pub const MAX_BYTES: usize = 64 * 1024;

/// 内容存活时长：超时后视为空。
///
/// 为什么需要：aetherd 以 root 运行，内容留在进程内存里没有上限，等于给
/// "AI 能读到的那一份敏感数据"开了个永久窗口。用户自己的粘贴走**合成器的本地
/// 剪贴板**，不受这里影响 —— 这里只是"AI 能读到的那一份"。
///
/// 取值与 `perm::DENY_TTL` / `perm::TOKEN_TTL` 一致（300 秒），便于记忆。
pub const TTL: Duration = Duration::from_secs(300);

struct Entry {
    text: String,
    set_at: Instant,
}

static CLIPBOARD: Mutex<Option<Entry>> = Mutex::new(None);

/// 写入剪贴板，返回写入的字节数。超长整体拒绝（不做截断 ——
/// 截断会让用户在不知情的情况下复制出残缺数据，那是比拒绝更糟的失败）。
pub fn set(text: &str) -> Result<usize, String> {
    let bytes = text.len();
    if bytes > MAX_BYTES {
        return Err(format!(
            "剪贴板内容 {bytes} 字节，超过上限 {MAX_BYTES}（拒绝整段，不做截断）"
        ));
    }
    *CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Entry { text: text.to_string(), set_at: Instant::now() });
    Ok(bytes)
}

/// 读取剪贴板（空或已过期则返回空串）。
///
/// 过期时**顺手清掉**：留着不但没意义，还让"内容已不在"这件事变得不可断言。
pub fn get() -> String {
    get_with_ttl(TTL)
}

fn get_with_ttl(ttl: Duration) -> String {
    let mut slot = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    match slot.as_ref() {
        Some(e) if e.set_at.elapsed() < ttl => e.text.clone(),
        Some(_) => {
            *slot = None;
            String::new()
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 剪贴板是进程级全局状态，而测试默认并行 —— 不串行化会互相覆盖。
    static LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn set_then_get_roundtrip() {
        let _g = lock();
        assert!(set("hello 剪贴板").is_ok());
        assert_eq!(get(), "hello 剪贴板");
    }

    #[test]
    fn set_overwrites_previous() {
        let _g = lock();
        set("first").unwrap();
        set("second").unwrap();
        assert_eq!(get(), "second");
    }

    #[test]
    fn oversized_is_rejected_wholesale() {
        let _g = lock();
        let big = "x".repeat(MAX_BYTES + 1);
        assert!(set(&big).is_err(), "超长必须整体拒绝");
        // 拒绝后原内容不受影响
        set("keep").unwrap();
        assert!(set(&big).is_err());
        assert_eq!(get(), "keep");
    }

    #[test]
    fn exact_cap_is_allowed() {
        let _g = lock();
        let exact = "y".repeat(MAX_BYTES);
        assert!(set(&exact).is_ok());
    }

    #[test]
    fn utf8_counted_by_bytes_not_chars() {
        let _g = lock();
        // 两个中文 = 6 字节；按字符计数会放行超限内容
        let s = "中".repeat(MAX_BYTES / 3 + 1);
        assert!(set(&s).is_err());
    }

    /// 内容有存活上限：过期后读为空，且**确实被清掉**（不是每次重新判断）。
    #[test]
    fn content_expires_after_ttl() {
        let _g = lock();
        set("secret").unwrap();
        assert_eq!(get_with_ttl(Duration::ZERO), "", "TTL 为零时任何内容都算过期");
        assert_eq!(get(), "", "过期读取应顺手清空，之后不再复活");
    }
}
