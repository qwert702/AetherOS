//! 控制中心布局常量（2026-11-13 从 draw.rs 抽出，模块路径仍是 `draw::control_layout`）。

/// 开关行（深色模式 / 输入法 / 24 小时制）
pub const SWITCH_ROWS: i32 = 3;
/// 快捷跳转行（设置 / 终端 / 通知 —— §3.2 加了通知中心入口）
pub const JUMP_ROWS: i32 = 3;
/// 电源行（关机 / 重启 / 注销）
pub const POWER_ROWS: i32 = 3;
/// 音量行（批次 K-2）：一条可点击设定的音量轨道
pub const VOLUME_ROWS: i32 = 1;
/// 虚拟桌面行（§5.4）：新增桌面 / 移除当前桌面
pub const SPACE_ROWS: i32 = 2;

/// 开关行高（含行距）
pub const ROW_H_SWITCH: i32 = 36;
/// 动作行高（快捷跳转 / 电源）
pub const ROW_H_ACTION: i32 = 32;
/// 顶部内边距 + AI 三态行
const HEAD: i32 = 12 + 26;
/// 一次分隔 + 组标题
const SECTION: i32 = 8 + 20;
/// 底部提示留白
pub const FOOTER: i32 = 22;

/// 内容总高（不含底部留白）
pub const CONTENT_H: i32 = HEAD
    + SECTION
    + SWITCH_ROWS * ROW_H_SWITCH
    + SECTION
    + JUMP_ROWS * ROW_H_ACTION
    + SECTION
    + VOLUME_ROWS * ROW_H_ACTION
    + SECTION
    + SPACE_ROWS * ROW_H_ACTION
    + SECTION
    + POWER_ROWS * ROW_H_ACTION;

/// 面板高度 = 内容 + 底部提示留白
pub const PANEL_H: i32 = CONTENT_H + FOOTER;
