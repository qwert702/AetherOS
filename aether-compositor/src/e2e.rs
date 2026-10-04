//! 无界面 e2e 的注入脚本（§4.1）。
//!
//! 目标：让"新功能没人点 = 没验证"这件事有解 —— 用一段**文本脚本**描述按键/鼠标，
//! 由执行器喂给**真实的分流函数**（`dispatch_nav` 等），再对状态做断言。
//!
//! 设计要点：
//! * **解析是纯函数**（`parse`）：脚本格式可以完整单测，不需要界面；
//! * **执行走既有函数**：不另写一套按键处理（真机 evdev 与预览 minifb 两条路径
//!   已经在 `dispatch_nav` 上共用分流 —— 见该函数文档）；
//! * **帧级点击**：`Renderer::render_frame` 会把可点区域填进 `control_hits`，
//!   所以在内存缓冲上画一帧就能拿到真实坐标 —— 那部分接进主循环是下一步。

use crate::input::NavKey;

/// 脚本里的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `nav:up` / `nav:down` / `nav:left` / `nav:right` / `nav:home` / `nav:end` /
    /// `nav:pageup` / `nav:pagedown` / `nav:delete`
    Nav(NavKey),
    /// `key:a` —— 往 AI 指令条输入一个字符
    Key(char),
    /// `wait:3` —— 空跑 3 帧（给"下一帧才生效"的逻辑留位置）
    Wait(u32),
}

/// 解析一行；空行与 `#` 开头返回 `Ok(None)`。
pub fn parse_line(line: &str) -> Result<Option<Step>, String> {
    let s = line.trim();
    if s.is_empty() || s.starts_with('#') {
        return Ok(None);
    }
    let (kind, arg) = match s.split_once(':') {
        Some((k, v)) => (k.trim(), v.trim()),
        None => (s, ""),
    };
    match kind {
        "nav" => {
            let key = match arg.to_ascii_lowercase().as_str() {
                "up" => NavKey::Up,
                "down" => NavKey::Down,
                "left" => NavKey::Left,
                "right" => NavKey::Right,
                "home" => NavKey::Home,
                "end" => NavKey::End,
                "pageup" | "pgup" => NavKey::PageUp,
                "pagedown" | "pgdn" => NavKey::PageDown,
                "delete" | "del" => NavKey::Delete,
                other => return Err(format!("未知的导航键：{other}")),
            };
            Ok(Some(Step::Nav(key)))
        }
        "key" => {
            let mut it = arg.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(Some(Step::Key(c))),
                _ => Err(format!("key: 后面要**恰好一个**字符：{arg:?}")),
            }
        }
        "wait" => arg
            .parse::<u32>()
            .map(|n| Some(Step::Wait(n)))
            .map_err(|_| format!("wait: 后面要是帧数：{arg:?}")),
        other => Err(format!("未知的步骤：{other}")),
    }
}

/// 解析整段脚本（逐行；出错时带上行号）。
pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut out = Vec::new();
    for (i, line) in script.lines().enumerate() {
        match parse_line(line) {
            Ok(Some(s)) => out.push(s),
            Ok(None) => {}
            Err(e) => return Err(format!("第 {} 行：{e}", i + 1)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nav_keys_and_ignores_comments() {
        let steps = parse("
            # 切到文件列表再往下选
            nav:down
            nav:pgdn
            key:x
            wait:2
        ")
        .expect("应能解析");
        assert_eq!(
            steps,
            vec![
                Step::Nav(NavKey::Down),
                Step::Nav(NavKey::PageDown),
                Step::Key('x'),
                Step::Wait(2),
            ]
        );
    }

    #[test]
    fn rejects_unknown_steps_and_says_which_line() {
        let e = parse("nav:down\nfly:up").unwrap_err();
        assert!(e.contains("第 2 行"), "要指出行号：{e}");
        assert!(parse("nav:sideways").is_err());
        // key: 必须恰好一个字符（两个字符要报错，而不是默默取第一个）
        assert!(parse("key:ab").is_err());
        assert!(parse("wait:soon").is_err());
        // 空的与注释行不算步骤
        assert!(parse("\n# 注释\n").unwrap().is_empty());
    }
}
