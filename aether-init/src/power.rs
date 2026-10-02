//! 电源动作（关机 / 重启）：PID 1 专属。
//!
//! ## 为什么电源必须由 PID 1 做
//!
//! `reboot(2)` 是整机级系统调用，而 `aether-init` 是 PID 1 —— 只有它能保证
//! "停服务 → 落盘 → 断电"这个顺序完整执行。合成器只画界面、aetherd 只做鉴权
//! 与审计，谁都不直接碰电源。
//!
//! ## 请求如何到达这里
//!
//! IPC 线程（`ipc.rs`）收到 `Request::SystemPower` 后**只置一个标志**（本模块的
//! `request()`），真正的执行发生在监督主循环里（`main.rs::run_loop`）。这样做的理由：
//! 关机要停掉所有服务，而那需要 `Manager` 的锁 —— 在 IPC 线程里做会与监督循环
//! 抢锁，且"IPC 线程正在关机"这种状态没人能看见。让主循环做，日志顺序也自然。
//!
//! ## 为什么用静态标志而不是 channel
//!
//! 只传递"要关机了"这一个信号，且**天然需要去重**（用户可能连点两次）：
//! `compare_exchange` 一次搞定，不需要额外的接收端与线程。
//!
//! ## 失败怎么办
//!
//! `reboot(2)` 成功时**不返回**；返回即失败。此时**不能退出**（PID 1 退出 =
//! 内核 panic），只能把原因打出来并继续监督循环 —— 用户至少还能看到日志。

use aether_ipc::PowerAction;
use std::sync::atomic::{AtomicU8, Ordering};

/// 无待执行动作。
const NONE: u8 = 0;
const POWER_OFF: u8 = 1;
const REBOOT: u8 = 2;

/// 待执行的电源动作（IPC 线程写、监督循环取）。
static PENDING: AtomicU8 = AtomicU8::new(NONE);

fn code(action: PowerAction) -> u8 {
    match action {
        PowerAction::PowerOff => POWER_OFF,
        PowerAction::Reboot => REBOOT,
    }
}

fn decode(c: u8) -> Option<PowerAction> {
    match c {
        POWER_OFF => Some(PowerAction::PowerOff),
        REBOOT => Some(PowerAction::Reboot),
        _ => None,
    }
}

/// 受理一个电源请求。返回 `false` 表示**已有**待执行动作（重复请求，已忽略）。
///
/// 去重是必要的：用户在确认卡片上连点两下、或合成器超时重发，都不该让
/// "关机"这条路径被并发执行两次（那会在停服务的中途插入第二次停止）。
pub fn request(action: PowerAction) -> bool {
    PENDING
        .compare_exchange(NONE, code(action), Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

/// 取出待执行动作（一次性；取出后标志归零）。
pub fn take() -> Option<PowerAction> {
    decode(PENDING.swap(NONE, Ordering::SeqCst))
}

/// 是否已有待执行动作（**测试与诊断用**）。
///
/// 生产路径不读它：监督循环只关心"取出来执行"（`take`）。
#[cfg(test)]
pub fn pending() -> Option<PowerAction> {
    decode(PENDING.load(Ordering::SeqCst))
}

/// 落盘并断电/重启。**成功不返回**；返回的字符串是失败原因。
///
/// 单独抽出来是为了让"危险的那一步"**只有一份实现**：GUI 路径（`execute`）与
/// 控制台路径（`main.rs::power_cli`）都调它。本仓库反复踩过"两套实现各修一半"的坑，
/// 这里不重犯。
pub fn sync_and_reboot(action: PowerAction) -> String {
    #[cfg(target_os = "linux")]
    {
        // sync 是系统调用，必须 unsafe；它没有返回值也没有失败语义
        unsafe { libc::sync() };
        let cmd = match action {
            PowerAction::PowerOff => libc::RB_POWER_OFF,
            PowerAction::Reboot => libc::RB_AUTOBOOT,
        };
        // 成功则不返回。失败时回来报告 —— 不 panic（PID 1 panic 等于整机不可用）
        unsafe { libc::reboot(cmd) };
        format!(
            "reboot({}) 失败：{}",
            action.as_str(),
            std::io::Error::last_os_error()
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("本平台不支持电源动作（仅 Linux 有 reboot 语义）：{}", action.as_str())
    }
}

/// 执行关机/重启：**停服务 → `sync()` → `reboot(2)`**。
///
/// 返回事件列表。注意：**成功时不返回**（进程随断电消失），所以调用方拿到返回值
/// 就意味着"没关成" —— 事件里会带失败原因。
///
/// `dry_run = true` 只走到"打印将要做什么"为止（`--dry-run` 开发自检与单测用）：
/// 测试不能真把开发机重启了。
pub fn execute(
    action: PowerAction,
    manager: &mut crate::manager::Manager,
    dry_run: bool,
) -> Vec<String> {
    let mut events = vec![format!("电源动作：{}（停服务 → sync → reboot）", action.as_str())];

    // 1) 先停服务：让它们有机会落盘、关连接（顺序 = 启动序的逆序）
    events.extend(manager.stop_all());

    // 2) 落盘并断电/重启（放在停服务之后：服务退出时可能刚写过文件）
    events.push("同步磁盘（sync）".into());
    if dry_run {
        events.push(format!("dry-run：不执行 reboot({})", action.as_str()));
        return events;
    }
    events.push(sync_and_reboot(action));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 电源标志是**进程级静态**，测试并行会互相干扰 → 串行化。
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 重复请求必须被忽略（连点两下不该关机两次）。
    #[test]
    fn duplicate_requests_are_ignored() {
        let _g = lock();
        // 清干净：上一个测试可能留下状态
        while take().is_some() {}
        assert!(request(PowerAction::PowerOff), "第一次请求应被受理");
        assert!(!request(PowerAction::PowerOff), "重复请求应被忽略");
        assert!(!request(PowerAction::Reboot), "不同动作也不能插队（已经在关机了）");
        assert_eq!(pending(), Some(PowerAction::PowerOff), "先到的动作优先");
        // 取走之后可以再次受理（例如 reboot 失败后用户重试）
        assert_eq!(take(), Some(PowerAction::PowerOff));
        assert_eq!(take(), None, "取过就没了");
        assert!(request(PowerAction::Reboot));
        while take().is_some() {}
    }

    /// `execute(dry_run=true)` 必须**只打印不执行** —— 否则开发机的测试会重启开发机。
    #[test]
    fn dry_run_never_reboots() {
        let _g = lock();
        let specs = crate::unit::load_dir(std::path::Path::new("no-such-dir"))
            .map(|(s, _)| s)
            .unwrap_or_default();
        // 用空服务表：这个测试只关心"不会真的 reboot"，不关心服务
        let mut mgr = crate::manager::Manager::new(specs).expect("空服务表应可构造");
        let events = execute(PowerAction::PowerOff, &mut mgr, true);
        assert!(
            events.iter().any(|e| e.contains("dry-run")),
            "dry-run 必须明确说明没执行：{events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("sync")),
            "sync 步骤要出现在事件里（顺序可审计）：{events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("poweroff")),
            "事件里要能看出是哪个动作：{events:?}"
        );
    }
}
