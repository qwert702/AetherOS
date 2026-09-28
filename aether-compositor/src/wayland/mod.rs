//! Wayland 协议实现（3.1 spike）。
//!
//! **路线：自研 `wl_display` 子集，而不是 smithay。**
//!
//! 理由（详见 `docs/PHASE3-DECISION-2026-09-28.md` §1.2）：
//! - **避开 Mesa 交叉编译到 musl 静态**这个最大的坑（smithay 通常要 EGL/GBM）
//! - 可直接复用现有的 **evdev 输入层**（`input.rs`）—— 不需要 libinput/seatd
//!
//! **范围刻意划小**（否则会变无底洞）：
//!
//! | 做 | 不做 |
//! |---|---|
//! | `wl_display` / `wl_registry` / `wl_callback` | `wl_seat` / `wl_pointer` / `wl_keyboard`（输入） |
//! | `wl_compositor` / `wl_surface` / `wl_region` | `wl_output`（多显示器） |
//! | `wl_shm` / `wl_shm_pool` / `wl_buffer` | `wl_subsurface`、多缓冲协议扩展 |
//! | `xdg_wm_base` / `xdg_surface` / `xdg_toplevel` | 装饰、popup、layer-shell |
//!
//! spike 的验收只有一条：**一个 `wl_shm` 客户端能画出一块矩形并 map 成顶层窗口。**
//!
//! 分层：
//!
//! ```text
//! wire     ← 消息编解码（纯逻辑，可单测）
//! object   ← 对象表（纯逻辑，可单测）
//! protocol ← 接口与签名定义（纯数据）
//! server   ← socket 与生命周期（Linux-only）
//! ```

pub mod object;
pub mod wire;
