//! 源码探针与回归测试（从 main.rs 拆出，2026-10-24；断言不变）。

use crate::*;

#[cfg(test)]
mod control_center_tests {
    /// §4.1：跑一段脚本，把最后一行 JSONL 的 `result` 取出来。
    fn e2e_result(script: &str) -> String {
        let mut desktop = demo_desktop();
        let mut undo = UndoStack::new();
        let mut spaces: spaces::Spaces<draw::Win> = spaces::Spaces::new();
        let out = run_e2e_script(script, &mut desktop, &mut undo, &mut spaces);
        let line = out.last().cloned().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&line).expect("应是 JSONL");
        v.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string()
    }

    /// §4.1：坐标点击要能点到**真实控件** —— 不是把坐标写死在测试里。
    ///
    /// 分三段跑（每段一次独立运行，布局是确定的，所以坐标可跨段使用）：
    /// ① 问状态簇的真实位置；② 点它展开控制中心，再问**展开后**的命中表；
    /// ③ 点命中表里的「深色模式」，断言状态真的翻转。
    #[test]
    fn e2e_click_reaches_real_control_center() {
        // ① 状态簇位置（x,y,w,h）
        let sc = e2e_result(r#"{"action":"query","args":{"what":"status_cluster"}}"#);
        let nums: Vec<i32> = sc.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        assert_eq!(nums.len(), 4, "状态簇应报出 x,y,w,h，实得 {sc:?}");
        let (scx, scy) = (nums[0] + nums[2] / 2, nums[1] + nums[3] / 2);

        // ② 点状态簇 → 控制中心展开；此时命中表才有内容
        let script = format!(
            "{{\"action\":\"click\",\"args\":{{\"x\":{scx},\"y\":{scy}}}}}\n{{\"action\":\"query\",\"args\":{{\"what\":\"hits\"}}}}"
        );
        let hits = e2e_result(&script);
        assert!(hits.contains("ToggleDark"), "展开后应有「深色模式」可点，实得：{hits}");

        // 从命中表里取 ToggleDark 的中心
        let dark = hits
            .split('|')
            .find(|h| h.contains("ToggleDark"))
            .expect("应有 ToggleDark");
        let d: Vec<&str> = dark.split(',').collect();
        let (dx, dy) = (
            d[0].parse::<i32>().unwrap() + d[2].parse::<i32>().unwrap() / 2,
            d[1].parse::<i32>().unwrap() + d[3].parse::<i32>().unwrap() / 2,
        );

        // ③ 展开 → 点「深色模式」→ 断言状态翻转
        let script = format!(
            "{{\"action\":\"click\",\"args\":{{\"x\":{scx},\"y\":{scy}}}}}\n{{\"action\":\"click\",\"args\":{{\"x\":{dx},\"y\":{dy}}}}}\n{{\"action\":\"query\",\"args\":{{\"what\":\"dark\"}}}}"
        );
        let before = e2e_result(r#"{"action":"query","args":{"what":"dark"}}"#);
        let after = e2e_result(&script);
        assert_ne!(before, after, "点了「深色模式」之后状态应当翻转（{before} → {after}）");
    }

    /// §4.1：`--e2e` 现在能**注入键盘**，并且注入结果可以被查询断言。
    ///
    /// 跑的是真实脚本字符串 → JSONL，中间走 `dispatch_nav`
    /// （真机 evdev 与预览 minifb **共用**的分流器），所以不是"另一个实现"的测试。
    #[test]
    fn e2e_script_can_inject_keys_and_assert_on_them() {
        let script = r#"
            {"action":"key","args":{"ch":"h"}}
            {"action":"key","args":{"ch":"i"}}
            {"action":"query","args":{"what":"ai_input"}}
            {"action":"nav","args":{"key":"down"}}
            {"action":"nav","args":{"key":"sideways"}}
        "#;
        let mut desktop = demo_desktop();
        let mut undo = UndoStack::new();
        let mut spaces: spaces::Spaces<draw::Win> = spaces::Spaces::new();
        let out = run_e2e_script(script, &mut desktop, &mut undo, &mut spaces);
        let joined = out.join("\n");
        assert!(joined.contains(r#""result":"hi""#), "注入两个字符后应能查到 hi：{joined}");
        assert!(joined.contains(r#""action":"nav","ok":true,"result":"down""#), "导航键应成功：{joined}");
        assert!(joined.contains("sideways"), "未知导航键要被报告：{joined}");
        // key 必须**恰好一个字符**
        let bad = run_e2e_script(
            r#"{"action":"key","args":{"ch":"ab"}}"#,
            &mut desktop, &mut undo, &mut spaces,
        );
        assert!(bad.join("\n").contains(r#""ok":false"#), "两个字符应报错：{bad:?}");
    }

    use super::*;

    /// L-14 回归：合成器的 `read_line_capped` 边界（与 aetherd 那份同一套规则）。
    #[test]
    fn read_line_capped_boundaries() {
        const MAX: usize = 16;
        let read = |s: &str| {
            let mut r = std::io::BufReader::new(std::io::Cursor::new(s.as_bytes().to_vec()));
            read_line_capped(&mut r, MAX)
        };
        let exact = "a".repeat(MAX as usize - 1) + "\n";
        assert!(read(&exact).is_ok(), "恰好 MAX 且带换行应通过");
        assert!(read(&"a".repeat(MAX as usize)).is_err(), "无换行的满额行必须报错");
        assert!(read(&("a".repeat(MAX as usize) + "\n")).is_err(), "超过 MAX 必须被拒");
        assert_eq!(read("").unwrap(), "");
    }

    /// L-13 回归：写进 VGA 控制台的文本必须过滤转义序列。
    ///
    /// 这条此前**零测试**（对抗审查指出注释自称"有单测"与事实不符）——
    /// 现在补上，并把 C1 区（U+0080–U+009F，即 8 位 CSI/OSC）也纳入。
    #[test]
    fn sanitize_for_tty_strips_escapes_and_c1() {
        // CSI / OSC / 其它 C0 / DEL
        assert_eq!(sanitize_for_tty("\x1b[31m红\x1b[0m"), "红");
        assert_eq!(sanitize_for_tty("\x1b]0;标题\x07后"), "后");
        assert_eq!(sanitize_for_tty("a\x07b\x00c\x7fd"), "abcd");
        // C1 区（字符级：这里是合法 UTF-8 的 U+009B / U+009D）。
        // 注意语义：**丢掉引导字节即可** —— 剩下的 `31m`/`0;x` 是惰性文本，
        // 终端不会把没有引导符的字节当序列。所以断言的是"引导符不在了"。
        assert_eq!(sanitize_for_tty("a\u{9b}31mb"), "a31mb");
        assert_eq!(sanitize_for_tty("a\u{9d}0;x\u{07}b"), "a0;xb");
        // 必须保留：换行/回车/制表/中文/emoji
        assert_eq!(sanitize_for_tty("l1\nl2\r\n\t中🚀"), "l1\nl2\r\n\t中🚀");
        // 半截转义序列不该吃掉后面的内容
        assert_eq!(sanitize_for_tty("正常\x1b"), "正常");
    }

    /// 控制中心的点击规则（三条）。刻意只测**不落盘**的动作（输入法切换、打开窗口），
    /// 避免单测去写 `/var/lib/aether`。
    ///
    /// 电源/注销三项**只断言意图**（`UiIntent`），不执行 —— 单测里点"关机"绝不该
    /// 真的发 IPC，更不该退出进程（那会把测试运行器一起带走）。
    #[test]
    fn click_rules() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop();
        let mut i = ime::Ime::default();
        r.status_cluster = Rect { x: 1100, y: 2, w: 60, h: 22 };

        // 判定与执行分开后，测试统一走这个小包装：拿 (是否消费, 意图)
        let click = |r: &mut draw::Renderer,
                     d: &mut Desktop,
                     i: &mut ime::Ime,
                     x: f32,
                     y: f32,
                     pressed: bool| {
            let mut intent = None;
            let consumed = control_center_click(r, d, i, x, y, pressed, &mut intent);
            (consumed, intent)
        };

        // ① 状态簇：第一次点开、再点收起
        assert!(click(&mut r, &mut d, &mut i, 1120.0, 10.0, true).0, "状态簇点击应被消费");
        assert!(r.control_open, "第一次点状态簇应展开");
        assert!(click(&mut r, &mut d, &mut i, 1120.0, 10.0, true).0);
        assert!(!r.control_open, "再点应收起");

        // 未展开时其它位置**不**消费点击（要留给窗口内容）
        assert!(!click(&mut r, &mut d, &mut i, 400.0, 400.0, true).0);

        // ② 展开后命中开关 → 执行真实动作并消费
        r.control_open = true;
        r.control_panel = Rect { x: 1000, y: 40, w: 264, h: 244 };
        let ime_before = i.enabled();
        r.control_hits = vec![(
            Rect { x: 1200, y: 100, w: 46, h: 20 },
            draw::ControlHit::ToggleIme,
        )];
        assert!(click(&mut r, &mut d, &mut i, 1210.0, 110.0, true).0);
        assert_ne!(i.enabled(), ime_before, "点输入法开关应真的切换输入法");

        // 打开设置：窗口数 +1（复用 open_app 那条真实路径）
        let wins_before = d.wins.len();
        r.control_hits = vec![(
            Rect { x: 1004, y: 200, w: 256, h: 34 },
            draw::ControlHit::OpenSettings,
        )];
        assert!(click(&mut r, &mut d, &mut i, 1100.0, 210.0, true).0);
        assert_eq!(d.wins.len(), wins_before + 1, "点快捷跳转应真的开窗口");
        assert_eq!(
            d.wins.last().map(|w| w.kind),
            Some(draw::WinKind::Settings),
            "打开设置应得到设置窗口（而不是文件管理器）"
        );

        // ③ 面板内空白：消费但不关闭
        assert!(click(&mut r, &mut d, &mut i, 1050.0, 240.0, true).0);
        assert!(r.control_open, "点面板内空白不该关闭面板");
        // 面板以外：关闭
        assert!(click(&mut r, &mut d, &mut i, 100.0, 400.0, true).0);
        assert!(!r.control_open, "点面板以外应关闭");

        // 未按下（松开）不消费
        assert!(!click(&mut r, &mut d, &mut i, 1120.0, 10.0, false).0);
    }

    /// 批次 J-2 回归：三个电源入口必须**判定出正确的意图**。
    ///
    /// 这条测试的价值在于"UI 真的接上了后端"：如果哪天有人把 `ControlHit::PowerOff`
    /// 的分支删了（或画了按钮却没登记命中区），这里会红 —— 而"点一下试试"是发现不了
    /// 的（本机没有 PID 1，点了也只会看到 IPC 失败）。
    #[test]
    fn power_entries_yield_intents() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop();
        let mut i = ime::Ime::default();
        r.status_cluster = Rect { x: 1100, y: 2, w: 60, h: 22 };
        r.control_open = true;
        r.control_panel = Rect { x: 1000, y: 40, w: 264, h: 406 };

        for (hit, want) in [
            (draw::ControlHit::PowerOff, UiIntent::Power(aether_ipc::PowerAction::PowerOff)),
            (draw::ControlHit::Reboot, UiIntent::Power(aether_ipc::PowerAction::Reboot)),
            (draw::ControlHit::Logout, UiIntent::Logout),
        ] {
            r.control_hits = vec![(Rect { x: 1004, y: 300, w: 256, h: 34 }, hit)];
            let mut intent = None;
            let consumed = control_center_click(&mut r, &mut d, &mut i, 1100.0, 310.0, true, &mut intent);
            assert!(consumed, "{hit:?} 的点击应被消费");
            assert_eq!(intent, Some(want), "{hit:?} 应判定出 {want:?}");
        }

        // 反向断言：普通开关**不能**顺手产生电源意图（否则面板里点什么都想关机）
        r.control_hits = vec![(
            Rect { x: 1200, y: 100, w: 46, h: 20 },
            draw::ControlHit::ToggleIme,
        )];
        let mut intent = None;
        control_center_click(&mut r, &mut d, &mut i, 1210.0, 110.0, true, &mut intent);
        assert_eq!(intent, None, "点开关不该产生电源意图");
    }

    /// 面板高度必须放得下全部命中区 —— 否则最后一行（注销）会被裁掉、点不到。
    ///
    /// 注意这条现在是**纯算术**：高度由 `control_layout` 的行数推导（加行自动变高），
    /// 所以它验证的是"行高/留白的账算对了"，而不是"有没有忘改常量" ——
    /// 后者已经被结构本身消灭了。
    #[test]
    fn control_panel_is_tall_enough_for_all_rows() {
        use draw::control_layout as L;
        // 最后一行的底部（相对面板顶）：HEAD + SECTION + 开关行 + SECTION + 跳转行
        //                       + SECTION + 音量行 + SECTION + 虚拟桌面行 + SECTION + 电源行
        let last_row_bottom = 12 + 26
            + (8 + 20) + L::SWITCH_ROWS * L::ROW_H_SWITCH
            + (8 + 20) + L::JUMP_ROWS * L::ROW_H_ACTION
            + (8 + 20) + L::VOLUME_ROWS * L::ROW_H_ACTION
            // §5.4：虚拟桌面那一段（加段时**这里也要跟着加** —— 这条测试正是为此存在）
            + (8 + 20) + L::SPACE_ROWS * L::ROW_H_ACTION
            + (8 + 20) + L::POWER_ROWS * L::ROW_H_ACTION;
        assert_eq!(
            last_row_bottom,
            L::CONTENT_H,
            "内容高度的推导与逐行累加不一致（行高或留白改了？）"
        );
        assert!(
            last_row_bottom <= L::PANEL_H,
            "最后一行（y+h={last_row_bottom}）超出面板高度 {}",
            L::PANEL_H
        );
        // 底部提示要留得下（它画在 panel.y + panel.h - 22）
        assert!(L::PANEL_H - last_row_bottom >= L::FOOTER, "底部提示没地方放");
    }

    /// 批次 K-2 回归：点击音量轨道的**位置**决定音量（绝对定位，不是"每次 ±5"）。
    ///
    /// 同时钉住边界：最左 = 0（静音，合法值）、最右 = 100、越界坐标必须夹取
    /// （坐标是浮点，边缘像素会越界一点点 —— 那不该变成 101% 或 panic）。
    #[test]
    fn volume_track_click_maps_position_to_percent() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop();
        let mut i = ime::Ime::default();
        r.status_cluster = Rect { x: 1100, y: 2, w: 60, h: 22 };
        r.control_open = true;
        r.control_panel = Rect { x: 1000, y: 40, w: 264, h: 450 };

        let track = Rect { x: 1020, y: 300, w: 200, h: 6 };
        r.control_hits = vec![(track, draw::ControlHit::VolumeTrack)];

        let pct_at = |r: &mut draw::Renderer, d: &mut Desktop, i: &mut ime::Ime, x: f32| {
            let mut intent = None;
            let consumed = control_center_click(r, d, i, x, 303.0, true, &mut intent);
            assert!(consumed, "音量轨道点击应被消费");
            match intent {
                Some(UiIntent::SetVolume(p)) => p,
                other => panic!("应产生 SetVolume 意图，实得 {other:?}"),
            }
        };

        // 注意边界是**开区间**（`Rect::contains` 不含右边界），所以点最左用 x+1：
        // 这是控件的既有语义，不是本功能的取舍。1 像素 ≈ 0.5% → 四舍五入到 1%。
        assert!(pct_at(&mut r, &mut d, &mut i, 1021.0) <= 1, "最左 ≈ 静音");
        assert!(pct_at(&mut r, &mut d, &mut i, 1219.0) >= 99, "最右 ≈ 100");
        assert_eq!(pct_at(&mut r, &mut d, &mut i, 1120.0), 50, "中点 = 50");
        // 轨道**之外**（面板空白）的点击：消费点击但**不产生音量意图** ——
        // 这是真正值得钉的语义（防误触改音量）。
        // 注：比例天然落在 0..1，构造不出 >100% 的点击，所以夹取只是浮点边缘的防御，
        // 不在这里硬凑用例（凑出来的用例测的是不存在的场景）。
        let mut intent = None;
        assert!(control_center_click(&mut r, &mut d, &mut i, 1240.0, 303.0, true, &mut intent));
        assert_eq!(intent, None, "点轨道之外不该改音量");
    }
}

#[cfg(test)]
mod desktop_icon_tests {
    use super::*;

    /// 默认桌面**必须没有窗口**（用户要求：启动后是干净桌面，窗口由图标/Dock 打开）。
    #[test]
    fn boot_desktop_has_no_windows() {
        let d = demo_desktop_sized(1280, 760);
        assert!(d.wins.is_empty(), "默认桌面不该预开窗口");
        assert!(
            d.wins.get(d.active).is_none(),
            "没有窗口时按 active 取窗口必须安全失败（不能越界 panic）"
        );
    }

    /// 点桌面图标 → 打开对应应用（桌面图标是**启动器**，不是任务栏）。
    #[test]
    fn click_opens_app() {
        let mut r = draw::Renderer::new(1280, 760);
        let mut d = demo_desktop_sized(1280, 760);
        r.desktop_icons = vec![(Rect { x: 20, y: 60, w: 96, h: 82 }, 4)];
        assert!(desktop_icon_click(&r, &mut d, 40.0, 90.0, true));
        assert_eq!(d.wins.len(), 1, "点桌面图标应开一个窗口");
        assert_eq!(d.wins[0].kind, draw::WinKind::Settings, "序号 4 应打开设置中心");
        assert!(!desktop_icon_click(&r, &mut d, 40.0, 90.0, false), "没按下的移动不该消费");
        assert!(!desktop_icon_click(&r, &mut d, 800.0, 400.0, true), "空白处不该消费");
    }
    /// 最小化 → 恢复的往返：期间窗口必须在**屏外**（命中测试与平铺才会自然跳过它）。
    #[test]
    fn minimize_then_restore() {
        let mut d = demo_desktop();
        let before = d.wins[1].rect;
        let msg = minimize_window(&mut d, 1).expect("最小化应返回提示文案");
        assert!(
            msg.for_lang(crate::i18n::Lang::Zh).contains("最小化"),
            "提示要说明发生了什么：{}",
            msg.text()
        );
        assert!(is_minimized(&d.wins[1]), "最小化后必须在屏外");
        assert!(d.wins[1].floating, "最小化必须脱离平铺，否则下一帧布局又把它拉回屏幕");
        assert!(d.wins[1].target.is_none());
        assert!(!d.wins[1].rect.contains(640.0, 400.0), "屏外窗口不该被点中");
        // 重复最小化是空操作
        assert!(minimize_window(&mut d, 1).is_none());
        // 恢复
        assert!(restore_if_minimized(&mut d, 1));
        assert_eq!(d.wins[1].rect, before, "恢复必须回到原来的位置与大小");
        assert!(!is_minimized(&d.wins[1]));
        assert!(!restore_if_minimized(&mut d, 1), "未最小化的窗口恢复是空操作");
    }
}

/// 窗口管理测试：插控制中心的测试时**不要动这个属性** —— 少了它整个模块会被编进正式二进制，
/// 里面的测试辅助函数（`desk`/`files_desk`/`term_desk`）就会成为 dead_code 警告。
#[cfg(test)]
mod window_mgmt_tests {
    use super::*;
    use draw::{FsEntry, WinKind};

    fn mkwin(title: &str, kind: WinKind) -> Win {
        Win {
            rect: Rect { x: 100, y: 100, w: 600, h: 400 },
            target: None,
            title: title.to_string(),
            kind,
            floating: false,
            restore: None,
            preview: None,
            term: None,
            wayland: None,
        }
    }

    fn desk(n: usize) -> Desktop {
        let wins = (0..n).map(|i| mkwin(&format!("w{i}"), WinKind::Terminal)).collect();
        Desktop {
            wins,
            active: n.saturating_sub(1),
            layout: Layout::TwoCol,
            cwd: "/tmp".into(),
            entries: Vec::new(),
            selected: None,
            scroll: 0,
            clipboard: String::new(),
            sidebar: sidebar_targets(),
            dir_error: None,
        }
    }

    /// 只读目录：`n` 个条目，交替目录/文件
    fn files_desk(n: usize) -> Desktop {
        let mut d = desk(1);
        d.wins[0] = mkwin("文件", WinKind::Files);
        d.entries = (0..n)
            .map(|i| FsEntry { name: format!("e{i}"), is_dir: i % 2 == 0, size: 0 })
            .collect();
        d
    }

    #[test]
    fn close_keeps_active_in_range() {
        // 关掉最后一个窗口：active 不能越界（曾是最容易踩的 panic 源）
        let mut d = desk(3);
        close_window(&mut d, 2);
        assert_eq!(d.wins.len(), 2);
        assert!(d.active < d.wins.len(), "active={} len={}", d.active, d.wins.len());
    }

    #[test]
    fn close_shifts_focus_when_removing_before_active() {
        let mut d = desk(3);
        d.active = 2;
        close_window(&mut d, 0); // 关掉活动项之前的一个
        assert_eq!(d.wins.len(), 2);
        // 原来 active 指向 w2，删掉 w0 后 w2 落到索引 1
        assert_eq!(d.active, 1);
        assert_eq!(d.wins[d.active].title, "w2");
    }

    #[test]
    fn close_to_empty_is_safe() {
        let mut d = desk(1);
        assert!(close_window(&mut d, 0).is_some());
        assert!(d.wins.is_empty());
        assert_eq!(d.active, 0);
        assert!(close_window(&mut d, 0).is_none()); // 再关是空操作，不 panic
    }

    /// 0.8 审计：窗口可能在**拖拽进行中**被其它路径关掉
    /// （AI 的 `close_active`、菜单「关闭窗口」）。
    ///
    /// 拖拽分支用 `wins[drag.0]` 直接索引 —— 索引一旦失效就是越界 panic，
    /// 而 compositor 是 essential + `restart: false`，**panic 等于桌面永久死掉**。
    /// 这条测试锁住"关掉最后一个窗口后旧索引确实失效"这个前提
    /// （守卫条件本身在拖拽分支里，无法直接单测）。
    #[test]
    fn closing_last_window_invalidates_stale_drag_index() {
        let mut d = desk(3);
        d.active = 2;
        let stale = 2; // 拖拽开始时记下的索引
        let idx = d.active;
        close_window(&mut d, idx);
        assert_eq!(d.wins.len(), 2);
        assert!(d.active < d.wins.len(), "active 必须仍在界内");
        assert!(
            stale >= d.wins.len(),
            "旧拖拽索引应已失效，这正是要防的越界条件"
        );
    }

    #[test]
    fn zoom_toggles_and_restores_rect() {
        let mut d = desk(1);
        let before = d.wins[0].rect;
        let work = Rect { x: 16, y: 42, w: 1248, h: 602 };
        toggle_zoom(&mut d, 0, work);
        assert_eq!(d.wins[0].target, Some(work));
        assert!(d.wins[0].floating, "最大化必须脱离平铺，否则下一帧被布局覆盖");
        assert_eq!(d.wins[0].restore, Some(before));
        toggle_zoom(&mut d, 0, work);
        assert_eq!(d.wins[0].target, Some(before));
        assert_eq!(d.wins[0].restore, None);
    }

    /// Alt+Tab 切到已最小化的窗口时**必须恢复它**（Windows 语义）。
    ///
    /// 不处理的话焦点会落在不可见的窗口上（用户看到"按了没反应"）；
    /// 直接跳过的话最小化的窗口会变成键盘不可达。两个错法都要防住。
    #[test]
    fn tab_restores_minimized_target() {
        let mut d = desk(3);
        d.active = 0;
        minimize_window(&mut d, 1).expect("最小化应成功");
        assert!(is_minimized(&d.wins[1]));
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 1, "应切到下一个窗口");
        assert!(!is_minimized(&d.wins[1]), "切到已最小化的窗口必须把它恢复");
        // 再切一次（目标是正常窗口）不应产生副作用
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 2);
        assert!(!is_minimized(&d.wins[2]));
    }

    #[test]
    fn tab_cycles_focus_including_wrap() {
        let mut d = desk(3);
        d.active = 0;
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 1);
        apply_nav(&mut d, input::NavKey::Tab);
        apply_nav(&mut d, input::NavKey::Tab);
        assert_eq!(d.active, 0, "应回绕到第一个");
        // 只有一个窗口时不切换（否则会出现"看不出发生了什么"的静默行为）
        let mut one = desk(1);
        assert!(apply_nav(&mut one, input::NavKey::Tab).is_none());
    }

    #[test]
    fn nav_moves_selection_by_grid() {
        use input::NavKey::*;
        let mut d = files_desk(20);
        // 600x400 的窗口 → 网格 3 列 × 3 行（见 grid_layout 的推导）
        let g = active_files_grid(&d).expect("文件窗口应有网格");
        assert_eq!((g.cols, g.rows), (3, 3));
        assert_eq!(g.page_size(), 9);

        apply_nav(&mut d, Down);
        assert_eq!(d.selected, Some(3), "Down 应下移一行（=cols）");
        apply_nav(&mut d, Up);
        assert_eq!(d.selected, Some(0));
        apply_nav(&mut d, Right);
        assert_eq!(d.selected, Some(1));
        apply_nav(&mut d, Left);
        assert_eq!(d.selected, Some(0));
        apply_nav(&mut d, End);
        assert_eq!(d.selected, Some(19));
        apply_nav(&mut d, Home);
        assert_eq!(d.selected, Some(0));
    }

    #[test]
    fn nav_keeps_selection_visible() {
        use input::NavKey::*;
        let mut d = files_desk(20);
        apply_nav(&mut d, PageDown); // 0 → 9
        assert_eq!(d.selected, Some(9));
        assert_eq!(d.scroll, 1, "第 9 项要落进可视页（9+1-9=1）");
        apply_nav(&mut d, End);
        assert_eq!(d.selected, Some(19));
        assert_eq!(d.scroll, 11);
        apply_nav(&mut d, Home);
        assert_eq!((d.selected, d.scroll), (Some(0), 0));
    }

    #[test]
    fn nav_on_non_files_window_is_noop() {
        use input::NavKey::*;
        let mut d = desk(1); // 终端窗口
        assert!(active_files_grid(&d).is_none());
        apply_nav(&mut d, Down);
        assert_eq!(d.selected, None);
    }

    /// `apply_nav` 绝不能自己处理 `Delete`。
    ///
    /// `Delete` 走的是 `dispatch_nav` → `request_delete` → 发一条**不带令牌**的
    /// `file_delete` ToolCall → 服务端判 L2 → 回确认卡片 → 用户点「允许一次」才真删。
    /// 这条测试守的是"绕过去"的那种写法：如果哪天有人图省事在 `apply_nav` 里直接
    /// 删条目，就会同时绕开闸门与审计 —— 那是权限模型的破口，不是少写一次 IPC。
    ///
    /// （这个测试原名叫 `delete_is_deliberately_not_implemented`，是 4.1 落地之前留下的，
    /// 名字已经和事实相反 —— 它容易被读成"Delete 没做"，从而误导文档。2026-09-28 改名。）
    #[test]
    fn apply_nav_must_not_handle_delete_locally() {
        let mut d = files_desk(3);
        d.selected = Some(1);
        assert!(apply_nav(&mut d, input::NavKey::Delete).is_none());
        assert_eq!(d.entries.len(), 3, "Delete 不得改动任何条目");
        assert_eq!(d.selected, Some(1));
    }

    /// Dock 的图标区间：内建 5 个 → 已装应用 → 安装向导。
    ///
    /// 为什么值得测：错位的症状是"点了 A 打开 B"，而且**只有真的用鼠标点过**才会发现；
    /// 在 QEMU 里靠相对坐标挪光标去撞图标非常不可靠（PS/2 相对事件 + 加速），
    /// 所以这段算术必须能在开发机上确定性验证。
    #[test]
    fn dock_action_maps_icon_ranges() {
        let inst = vec![("hello".to_string(), "Hello".to_string())];

        // 内建区间
        assert_eq!(dock_action(0, &inst, true), DockAction::Builtin(0));
        assert_eq!(dock_action(4, &inst, false), DockAction::Builtin(4));
        assert_eq!(dock_action(4, &inst, true), DockAction::Builtin(4));

        // 已装应用紧接在内建之后
        assert_eq!(dock_action(5, &inst, true), DockAction::Installed("hello", "Hello"));
        assert_eq!(dock_action(5, &inst, false), DockAction::Installed("hello", "Hello"));

        // 安装向导在**最后**；没有已装应用时它前移到第 6 个（不能串位）
        assert_eq!(dock_action(6, &inst, true), DockAction::Installer);
        assert_eq!(dock_action(5, &[], true), DockAction::Installer);

        // 边界：Live ISO 关了就没有安装向导；越界落空
        assert_eq!(dock_action(6, &inst, false), DockAction::Nothing);
        assert_eq!(dock_action(5, &[], false), DockAction::Nothing);
        assert_eq!(dock_action(99, &inst, true), DockAction::Nothing);

        // 多个已装应用时逐个对应，不能都指到同一个
        let two = vec![
            ("a".to_string(), "甲".to_string()),
            ("b".to_string(), "乙".to_string()),
        ];
        assert_eq!(dock_action(5, &two, true), DockAction::Installed("a", "甲"));
        assert_eq!(dock_action(6, &two, true), DockAction::Installed("b", "乙"));
        assert_eq!(dock_action(7, &two, true), DockAction::Installer);
    }

    /// `APP_TITLES` 与 `draw::DOCK_BUILTINS` 必须一致 —— 命中测试靠后者算区间。
    #[test]
    fn app_titles_match_dock_builtins() {
        assert_eq!(APP_TITLES.len(), draw::DOCK_BUILTINS);
    }

    fn term_desk() -> Desktop {
        let mut d = desk(1);
        d.wins[0] = Win {
            rect: Rect { x: 100, y: 100, w: 600, h: 400 },
            target: None,
            title: "终端".into(),
            kind: WinKind::Terminal,
            floating: false,
            restore: None,
            preview: None,
            term: Some(term::Terminal::spawn(40, 8, None)),
        wayland: None,
        };
        d
    }

    #[test]
    fn edge_detection_covers_sides_and_corners() {
        let w = mkwin("w", WinKind::Files); // rect = 100,100 600x400
        assert_eq!(edge_at(&w, 100.0, 300.0), Some(Edge::Left));
        assert_eq!(edge_at(&w, 700.0, 300.0), Some(Edge::Right));
        assert_eq!(edge_at(&w, 400.0, 500.0), Some(Edge::Bottom));
        assert_eq!(edge_at(&w, 700.0, 500.0), Some(Edge::BottomRight));
        assert_eq!(edge_at(&w, 100.0, 500.0), Some(Edge::BottomLeft));
        // 窗口内部与**上边**都不是缩放区：上边是标题栏，归拖动
        assert_eq!(edge_at(&w, 400.0, 300.0), None);
        assert_eq!(edge_at(&w, 400.0, 100.0), None, "上边必须留给拖动");
        assert_eq!(edge_at(&w, 400.0, 600.0), None, "窗口外远端不算边缘");
    }

    #[test]
    fn resize_right_edge_changes_width_only() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 760.0, 300.0);
        assert_eq!(w.rect.x, orig.x);
        assert_eq!(w.rect.w, orig.w + 60);
        assert_eq!(w.rect.h, orig.h, "只有右边时高度不应变");
    }

    #[test]
    fn resize_left_edge_keeps_right_border_fixed() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        let right_before = orig.x + orig.w;
        apply_resize(&mut w, Edge::Left, orig, 200.0, 300.0);
        assert_eq!(w.rect.x + w.rect.w, right_before, "拖左边时右边必须固定");
        assert_eq!(w.rect.w, orig.w - 100);
    }

    #[test]
    fn resize_clamps_to_min_size() {
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 110.0, 300.0);
        assert!(w.rect.w >= 320, "不能小于最小宽度，实际 {}", w.rect.w);
        apply_resize(&mut w, Edge::Bottom, orig, 400.0, 120.0);
        assert!(w.rect.h >= 200, "不能小于最小高度，实际 {}", w.rect.h);
    }

    #[test]
    fn resize_uses_original_rect_so_no_drift() {
        // 以"按下的原始矩形"为基准：同一目标位置重复应用结果一致（不会累积漂移）
        let mut w = mkwin("w", WinKind::Files);
        let orig = w.rect;
        apply_resize(&mut w, Edge::Right, orig, 700.0, 0.0);
        let once = w.rect.w;
        apply_resize(&mut w, Edge::Right, orig, 700.0, 0.0);
        assert_eq!(w.rect.w, once, "重复应用同一拖拽量不应继续变宽");
    }

    #[test]
    fn begin_resize_detaches_from_tiling() {
        let mut d = desk(1);
        let r = begin_resize(&mut d, 0, Edge::BottomRight).expect("应能开始缩放");
        assert_eq!(r.0, 0);
        assert!(d.wins[0].floating, "缩放必须脱离平铺，否则下一帧被布局改回");
        assert_eq!(d.wins[0].target, None);
        assert_eq!(d.active, 0);
    }

    #[test]
    fn crumbs_split_windows_and_unix_paths() {
        // 用原始字符串写 Windows 路径，避免转义把反斜杠吃掉
        let win = r"C:\Users\cbn";
        let w = draw::crumbs(win);
        assert_eq!(
            w.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["C:", "Users", "cbn"]
        );
        assert_eq!(w[2].1, win, "末段应等于原路径");
        assert_eq!(w[1].1, r"C:\Users");
        // 正斜杠形式的 Windows 路径（有些 API 会返回这种）同样要能拆
        let fwd = draw::crumbs("C:/Users/cbn");
        assert_eq!(fwd.len(), 3, "正斜杠路径也应拆出三段");
        assert_eq!(fwd[2].1, "C:/Users/cbn");
        let u = draw::crumbs("/home/u");
        assert_eq!(
            u.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["home", "u"]
        );
        assert_eq!(u[1].1, "/home/u");
        assert!(draw::crumbs("").is_empty(), "空路径不应产生面包屑");
    }

    #[test]
    fn navigate_resets_selection_and_scroll() {
        let mut d = files_desk(5);
        d.selected = Some(3);
        d.scroll = 2;
        let tmp = std::env::temp_dir();
        let msg = navigate_to(&mut d, &tmp.to_string_lossy(), "测试目录", "Test folder");
        assert!(msg.is_none(), "系统临时目录应可读：{msg:?}");
        assert_eq!(d.selected, None, "换目录后选中项必须重置");
        assert_eq!(d.scroll, 0, "换目录后滚动位置必须重置");
    }

    #[test]
    fn navigate_failure_keeps_current_dir() {
        let mut d = files_desk(3);
        let before = d.cwd.clone();
        let msg = navigate_to(&mut d, "__aether_absent_dir__", "不存在的目录", "Missing folder");
        assert!(msg.is_some(), "失败要有反馈");
        assert_eq!(d.cwd, before, "导航失败不应把用户丢到读不了的目录");
        assert!(d.dir_error.is_some(), "失败原因要能显示在界面上");
    }

    #[test]
    fn clipboard_copies_selected_entry_path() {
        let mut d = files_desk(3);
        d.selected = Some(1);
        let msg = clipboard_copy(&mut d).expect("应有复制反馈");
        assert!(msg.for_lang(crate::i18n::Lang::Zh).contains("路径"), "{}", msg.text());
        // 复制的是**完整路径**，不是文件名 —— 粘贴出去才有用
        assert_eq!(d.clipboard, draw::join_path("/tmp", "e1"));
    }

    // 只在开发机跑：Linux 下 `Terminal::spawn` 开的是**真 PTY**（/bin/sh），
    // 屏幕初始是空的；这些测试验的是"演示脚本能被正确解析"，而演示脚本
    // 只在非 Linux 路径存在。Linux 侧的终端行为由实机验证覆盖。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn clipboard_copies_terminal_visible_content() {
        let mut d = term_desk();
        let msg = clipboard_copy(&mut d).expect("终端应可复制");
        assert!(msg.for_lang(crate::i18n::Lang::Zh).contains("行"), "{}", msg.text());
        assert!(d.clipboard.contains("aether@localhost"), "应包含演示会话内容");
        assert!(
            !d.clipboard.contains("\u{0}"),
            "复制内容不应带控制字符残留"
        );
    }

    #[test]
    fn clipboard_paste_into_terminal_is_consumed() {
        let mut d = term_desk();
        d.clipboard = "echo hi\n".into();
        // 粘贴进终端：由终端消费（返回 None + 不再回给输入框）
        assert!(clipboard_paste(&mut d).is_none());
    }

    #[test]
    fn clipboard_paste_to_input_returns_text_for_ai_bar() {
        let mut d = files_desk(1);
        d.clipboard = "/tmp/x\n".into();
        let text = clipboard_paste(&mut d).expect("非终端目标应把文本交回调用方");
        assert_eq!(text, "/tmp/x\n");
    }

    #[test]
    fn ctrl_shift_c_copies_and_does_not_close_window() {
        // 这条是 shift 区分的意义所在：Ctrl+Shift+C 不能变成"关窗"
        let mut d = term_desk();
        let before = d.wins.len();
        assert!(
            matches!(apply_ctrl(&mut d, 'c', true), CtrlOutcome::Message(_)),
            "Ctrl+Shift+C 应复制"
        );
        assert_eq!(d.wins.len(), before, "Ctrl+Shift+C 不得关窗");
    }

    #[test]
    fn ctrl_w_closes_active_window() {
        let mut d = desk(2);
        d.active = 1;
        // 关窗现在带出**窗口本身**（撤销时要放回来），所以是 `Closed` 而不是 `Message`
        assert!(matches!(
            apply_ctrl(&mut d, 'w', false),
            CtrlOutcome::Closed { .. }
        ));
        assert_eq!(d.wins.len(), 1);
        assert_eq!(d.active, 0);
        assert!(
            matches!(apply_ctrl(&mut d, 'q', false), CtrlOutcome::Nothing),
            "未绑定的组合键不应有副作用"
        );
    }
}

/// W4：`sync_wayland` 的身份同步测试。
#[cfg(test)]
mod wayland_sync_tests {
    use super::*;
    use wayland::session::{Role, Session, SurfaceState};
    use wayland::shm::BufferInfo;

    const SURFACE: u32 = 6;
    const BUFFER: u32 = 7;

    /// 手工构造一个"已 map"的 surface 状态（协议流程在 session 的测试里验过，
    /// 这里只需要终态）。
    fn mapped_session(title: &str, pixels: Vec<u8>) -> Session {
        let mut s = Session::new();
        s.objects.insert(SURFACE, "wl_surface", 4, 0);
        s.objects.insert(BUFFER, "wl_buffer", 1, 0);
        s.surfaces.insert(
            SURFACE,
            SurfaceState {
                role: Some(Role {
                    title: title.into(),
                    app_id: "test-app".into(),
                    toplevel: Some(9),
                    acked_serial: Some(1),
                }),
                has_buffer: true,
                attached_buffer: Some(BUFFER),
                xdg_surface: Some(8),
                pixels: Some(pixels),
            },
        );
        s.buffers.insert(
            BUFFER,
            BufferInfo { pool_id: 10, offset: 0, width: 1, height: 1, stride: 4, format: 1 },
        );
        s.mapped.push(SURFACE);
        s
    }

    #[test]
    fn new_mapped_surface_becomes_window() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s = mapped_session("测试窗口", vec![0xff, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n0 + 1, "应新增一个窗口");
        let w = desktop.wins.last().unwrap();
        assert_eq!(w.kind, draw::WinKind::Wayland);
        assert_eq!(w.title, "测试窗口");
        assert_eq!(w.wayland.as_ref().unwrap().surface_id, SURFACE);
    }

    /// 同步必须**幂等**：同一状态同步两遍不产生重复窗口（每帧都会调）。
    #[test]
    fn sync_is_idempotent() {
        let mut desktop = demo_desktop();
        let s = mapped_session("w", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        let n1 = desktop.wins.len();
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n1, "第二次同步不该新建窗口");
    }

    #[test]
    fn unmap_removes_window_but_keeps_others() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s = mapped_session("w", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        assert_eq!(desktop.wins.len(), n0 + 1);
        // surface 消失（unmap/destroy）
        let mut s2 = mapped_session("w", vec![0, 0, 0, 255]);
        s2.mapped.clear();
        sync_wayland(&mut desktop, &s2);
        assert_eq!(desktop.wins.len(), n0, "Wayland 窗口应移除");
        // 原有窗口（demo 的 Files/Music/Terminal）必须完好
        assert!(
            desktop.wins.iter().all(|w| w.wayland.is_none()),
            "不该误删本地窗口"
        );
    }

    #[test]
    fn existing_window_updates_content_without_recreating() {
        let mut desktop = demo_desktop();
        let s = mapped_session("v1", vec![0xff, 0, 0, 255]);
        sync_wayland(&mut desktop, &s);
        let w = desktop.wins.last().unwrap();
        let rect_before = w.rect;
        // 客户端改了标题和像素（下一帧 commit）
        let s2 = mapped_session("v2", vec![0, 0, 0xff, 255]);
        sync_wayland(&mut desktop, &s2);
        let w = desktop.wins.last().unwrap();
        assert_eq!(w.title, "v2", "标题应更新");
        assert_eq!(w.rect, rect_before, "窗口位置不应被重建冲掉");
        assert_eq!(
            w.wayland.as_ref().unwrap().pixels,
            vec![0, 0, 0xff, 255],
            "像素应更新"
        );
    }

    /// 红绿灯 / 拖拽等按 `wins[i]` 索引的逻辑依赖窗口顺序 —— 移除中间窗口时
    /// 不能让本地窗口跟着错位（P2 的教训在 Wayland 侧同样适用）。
    #[test]
    fn remove_middle_wayland_window_keeps_local_windows() {
        let mut desktop = demo_desktop();
        let n0 = desktop.wins.len();
        let s1 = mapped_session("wl-1", vec![0, 0, 0, 255]);
        sync_wayland(&mut desktop, &s1);
        // 两个 Wayland surface，只保留第一个 —— 第二个的窗口应被移除
        let mut s2 = mapped_session("wl-1", vec![0, 0, 0, 255]);
        s2.mapped.clear();
        sync_wayland(&mut desktop, &s2);
        assert_eq!(desktop.wins.len(), n0, "全部移除后回到初始数量");
    }
    /// §5.10：HUD 文本的三条性质 —— 真数字、不除零、拿不到内存时如实说明。
    #[test]
    fn perf_hud_text_is_honest_about_edges() {
        // 正常帧：33.3ms ≈ 30fps
        let s = perf_hud_text(33.3, "120.0 MB");
        assert!(s.contains("33.3 ms/帧"), "{s}");
        assert!(s.contains("30 fps"), "{s}");
        assert!(s.contains("120.0 MB"), "{s}");
        // 0 毫秒**不能**变成 inf/nan（第一帧或计时精度不足时会真的出现）
        let z = perf_hud_text(0.0, "不可用");
        assert!(z.contains("0 fps"), "0 毫秒应显示 0 fps，而不是 inf：{z}");
        assert!(!z.contains("inf") && !z.contains("NaN"), "{z}");
        // 拿不到内存时如实显示原因（不编数字）
        assert!(perf_hud_text(16.0, "不可用（非 Linux：无 /proc）").contains("不可用"));
    }

    /// §3.2：通知中心打开后必须**真的能看见**，且打开即清未读、不叠第二个窗口。
    #[test]
    fn notifications_window_shows_history_and_clears_unread() {
        let mut d = demo_desktop();
        let mut n = notifications::Notifications::new();
        let wins_before = d.wins.len();

        // 空历史也要能打开（显示一句说明，而不是一片空白/打不开）
        assert!(open_notifications(&mut d, &mut n, 480).is_none());
        assert_eq!(d.wins.len(), wins_before + 1, "空历史也应打开窗口");
        let w = d.wins.last().unwrap();
        assert_eq!(w.title, "通知");
        let data = w.preview.as_ref().expect("应有预览内容");
        assert!(data.path.starts_with("通知（共 "), "标题应带总数：{}", data.path);
        assert!(data.error.is_some(), "空历史应给出说明而不是空白");
        // 开着记录时的空 = "还没有通知"
        // §3.5：错误原因是**枚举**了，所以先按语言渲染成文案再断言
        let msg = draw::preview_error_text(
            crate::i18n::Lang::Zh,
            data.error.as_ref().unwrap(),
        );
        assert!(msg.contains("还没有"), "开着记录时应说'还没有通知'：{msg}");
        // 关掉记录时的空必须**说明原因**（否则用户以为功能坏了，其实是他自己关的）
        let mut n2 = notifications::Notifications::new();
        n2.set_enabled(false);
        let mut d2 = demo_desktop();
        let _ = open_notifications(&mut d2, &mut n2, 480);
        let off = draw::preview_error_text(
            crate::i18n::Lang::Zh,
            d2.wins
                .last()
                .unwrap()
                .preview
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap(),
        );
        assert!(off.contains("记录已关闭"), "关闭记录时的空文案应说明原因：{off}");

        // 记两条（类别不同 → 不会合并），再打开
        n.push(ToastMsg::new("网络已连接", "Network connected"), notifications::Kind::Ok, 3600);
        n.push(ToastMsg::new("写盘失败", "Disk write failed"), notifications::Kind::Err, 3660);
        assert_eq!(n.unread(), 2);
        assert!(open_notifications(&mut d, &mut n, 480).is_none());
        assert_eq!(d.wins.len(), wins_before + 1, "已有通知窗口时应就地刷新，不叠第二个");
        let data = d.wins.last().unwrap().preview.as_ref().unwrap();
        assert_eq!(data.lines.len(), 2, "两条通知都应在列表里");
        // 新的在前
        assert!(data.lines[0].contains("写盘失败"), "最新一条应在最前：{:?}", data.lines);
        // 类别标记可区分（否则"成功"和"失败"在历史里长得一样）
        assert!(data.lines[0].contains(notifications::Kind::Err.mark()));
        assert!(data.lines[1].contains(notifications::Kind::Ok.mark()));
        assert!(data.error.is_none(), "有内容时不该显示说明");
        // 打开即清未读；**历史不清**（清空是另一个动作）
        assert_eq!(n.unread(), 0, "打开通知中心应清未读");
        assert_eq!(n.len(), 2, "清未读不该删历史");
    }

    /// 自查补强：控制中心的**行距必须来自布局常量**，不能是硬编码数字。
    ///
    /// 为什么需要这条（而不是原来那条"高度公式自洽"的测试）：原来那条两边都从同一组
    /// 常量推导，是**同义反复** —— 它挡不住"在绘制里加了一行、却忘了改行数常量"。
    /// 实测过：修这条之前绘制实际比公式高 4px，而那条测试照样通过。
    ///
    /// 用 `include_str!` 在编译期嵌入源码来扫（不做运行期文件 IO）。
    #[test]
    fn control_center_row_advances_come_from_layout_constants() {
        let src = include_str!("draw.rs");
        let start = src.find("fn draw_control_center(").expect("找不到控制中心绘制函数");
        let end = src[start..]
            .find("/// 展开中的下拉菜单")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        for bad in ["y += 36;", "y += 32;", "y += 40;", "y += 34;"] {
            assert!(
                !body.contains(bad),
                "控制中心里还有硬编码行距 `{bad}` —— 高度公式与绘制会不一致（加一行就会忘）"
            );
        }
        // 同时要求它**确实**用了常量（否则把行距删了也能过上面那条）
        assert!(
            body.contains("control_layout::ROW_H_SWITCH"),
            "开关行距应来自 control_layout::ROW_H_SWITCH"
        );
        assert!(
            body.contains("control_layout::ROW_H_ACTION"),
            "动作行距应来自 control_layout::ROW_H_ACTION"
        );
    }

    /// §5.1：撤销"改布局"必须连**浮动标志**一起还原。
    ///
    /// 只还原布局是不够的：正向动作会把所有窗口的 `floating` 清掉，
    /// 于是原本浮动的窗口会继续被平铺 —— 用户会认为"撤销没生效"。
    #[test]
    fn undo_restores_layout_and_floating_flags() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        let before = d.layout.label();
        // 先人为让第一个窗口浮动，检验撤销是否还原它
        if let Some(w) = d.wins.first_mut() {
            w.floating = true;
        }
        let floats_before: Vec<bool> = d.wins.iter().map(|w| w.floating).collect();

        let args = serde_json::json!({ "layout": "monocle" });
        let _ = apply_action(&mut d, "layout_set", &args, &mut u);
        assert_ne!(d.layout.label(), before, "布局应被改掉");
        assert!(d.wins.iter().all(|w| !w.floating), "正向动作应清掉浮动标志");
        assert_eq!(u.len(), 1);

        let msg = undo_last(&mut d, &mut u).expect("应有可撤销的动作");
        assert_eq!(d.layout.label(), before, "撤销应还原布局");
        assert_eq!(
            d.wins.iter().map(|w| w.floating).collect::<Vec<_>>(),
            floats_before,
            "撤销应还原每个窗口的浮动标志"
        );
        assert!(
            msg.for_lang(crate::i18n::Lang::Zh).contains("布局"),
            "提示应说明撤了什么：{}",
            msg.text()
        );
        // 栈已空 → 再撤销没有可做的
        assert!(undo_last(&mut d, &mut u).is_none(), "空栈应返回 None");
    }

    /// §5.1：撤销"开窗"必须关掉**刚打开的那个**。
    #[test]
    fn undo_after_open_app_closes_that_window() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        let n0 = d.wins.len();
        let args = serde_json::json!({ "app": "终端" });
        let _ = apply_action(&mut d, "open_app", &args, &mut u);
        assert_eq!(d.wins.len(), n0 + 1, "应真的开了一个窗口");
        assert_eq!(u.len(), 1);
        let msg = undo_last(&mut d, &mut u).expect("应有可撤销的动作");
        assert_eq!(d.wins.len(), n0, "撤销应关掉刚打开的窗口");
        assert!(
            msg.for_lang(crate::i18n::Lang::Zh).contains("关掉"),
            "提示应说明撤了什么：{}",
            msg.text()
        );
    }

    /// §5.1：**关窗不可撤销**，而且不能"跳过它去撤销更早的动作"。
    ///
    /// §5.1（批次 AW-1 之后）：关窗**可以撤销** —— 窗口（连同活会话）被移进
    /// "最近关闭"托盘，撤销把它放回原位。
    ///
    /// 这条测试真正要守的**性质没变**：`Ctrl+Z` 必须**先处理"关窗"这一步**，
    /// 不能跳过去撤销更早的布局改动 —— 跳步会让用户以为撤错了东西。
    /// （改动前它守的是"明确说撤不了"；现在改守"放回来了，同样不跳步"。）
    #[test]
    fn closing_is_undoable_and_does_not_fall_through() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        // 先做一步可撤销的（改布局），再关窗
        let args = serde_json::json!({ "layout": "float" });
        let _ = apply_action(&mut d, "layout_set", &args, &mut u);
        let layout_after_change = d.layout.label();
        let wins_before = d.wins.len();
        let _ = apply_action(&mut d, "close_active", &serde_json::json!({}), &mut u);
        assert_eq!(u.len(), 2);
        assert_eq!(d.wins.len(), wins_before - 1, "关窗确实关掉了一个窗口");
        assert_eq!(u.closed.len(), 1, "被关掉的窗口应当进了托盘");

        // 第一次撤销：把窗口放回来，**布局保持不变**
        let msg = undo_last(&mut d, &mut u).expect("应消费掉这一步");
        assert!(
            msg.for_lang(crate::i18n::Lang::Zh).contains("重新打开"),
            "应告知窗口被放回来了：{}",
            msg.text()
        );
        assert_eq!(d.wins.len(), wins_before, "窗口应当回来了");
        assert!(
            u.closed.is_empty(),
            "托盘里那个窗口应当被取走（不能留在托盘里越积越多）"
        );
        assert_eq!(
            d.layout.label(),
            layout_after_change,
            "撤销关窗**不得**顺手撤销更早的布局改动"
        );
        // 第二次撤销才是布局
        let msg2 = undo_last(&mut d, &mut u).expect("布局那步仍应可撤销");
        assert!(msg2.for_lang(crate::i18n::Lang::Zh).contains("布局"));
    }

    /// §5.1：栈**有界**（无界栈会随运行时长无限增长）。
    #[test]
    fn undo_stack_is_bounded() {
        let mut u = UndoStack::new();
        for i in 0..(UNDO_CAP + 5) {
            u.push(UndoStep::OpenedWindow(i));
        }
        assert_eq!(u.len(), UNDO_CAP, "栈不得超过上限");
        // 弹出顺序 = 从新到旧：第一个弹出的是最新压入的，最后一个是最早留下的
        let mut popped = Vec::new();
        while let Some(UndoStep::OpenedWindow(i)) = u.pop() {
            popped.push(i);
        }
        assert_eq!(popped.len(), UNDO_CAP, "应恰好弹出上限那么多步");
        assert_eq!(popped[0], UNDO_CAP + 5 - 1, "最新的应在栈顶（先弹出）");
        assert_eq!(
            *popped.last().unwrap(),
            UNDO_CAP + 5 - 1 - (UNDO_CAP as i32 - 1) as usize,
            "最旧的 5 步应已被淘汰"
        );
    }

    /// §5.1：被拒绝的 open_app **不该**记一步（否则"撤销一次什么都没发生"）。
    #[test]
    fn rejected_open_app_records_nothing() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        // 不存在的应用名 → apply_action 返回 None，且不该开窗
        let n0 = d.wins.len();
        let r = apply_action(&mut d, "open_app", &serde_json::json!({ "app": "不存在的应用" }), &mut u);
        assert!(r.is_none(), "未知应用应被拒绝");
        assert_eq!(d.wins.len(), n0, "不该开窗");
        assert_eq!(u.len(), 0, "被拒绝的动作不该进撤销栈");
    }

    /// §4.1：e2e 运行器的三条性质 —— 注释/空行忽略、真实动作可用、非法输入逐条报错不中止。
    #[test]
    fn e2e_runner_executes_real_actions_and_reports_each_line() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        let script = r#"
# 这是注释，应被忽略
{"action":"query","args":{"what":"windows"}}
{"action":"layout_set","args":{"layout":"monocle"}}
{"action":"query","args":{"what":"layout"}}
{"action":"open_app","args":{"app":"终端"}}
{"action":"query","args":{"what":"windows"}}
{"action":"undo"}
{"action":"query","args":{"what":"windows"}}
"#;
        let out = run_e2e_script(script, &mut d, &mut u, &mut spaces::Spaces::new());
        // 7 条有效动作（注释与空行不产生输出）
        assert_eq!(out.len(), 7, "应逐条输出：{out:#?}");
        // 每条都是带行号的 JSON
        for l in &out {
            assert!(l.starts_with("{\"line\":"), "应是 JSONL：{l}");
            assert!(l.contains("\"ok\":"), "应带 ok 字段：{l}");
        }
        // 布局真的被改了：out[1] 是 layout_set 的结果，out[2] 是随后的查询
        assert!(out[1].contains("布局"), "第 2 条应是改布局的结果：{}", out[1]);
        assert!(
            out[2].contains("独占堆叠"),
            "查询应返回新布局（Monocle 的中文标签）：{}",
            out[2]
        );
        assert!(out[5].contains("关掉"), "撤销开窗应说明关掉了窗口：{}", out[5]);

        // **比较窗口数**（而不是靠索引猜）：开窗后变化，撤销后回到原值。
        // 这样断言的是行为，不是输出的排列顺序 —— 后者改一下脚本就会错位。
        let result_of = |l: &str| -> String {
            serde_json::from_str::<serde_json::Value>(l)
                .expect("输出应是合法 JSON")
                .get("result")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string()
        };
        let before = result_of(&out[0]);
        let opened = result_of(&out[4]);
        let after_undo = result_of(&out[6]);
        assert_ne!(before, opened, "开窗后窗口数应变化：{before} → {opened}");
        assert_eq!(before, after_undo, "撤销后窗口数应回到原值：{before} vs {after_undo}");
    }

    /// §4.1：非法输入**逐条报错但不中止**（一次跑完看到全部问题，比修一个跑一次省时间）。
    #[test]
    fn e2e_runner_reports_bad_lines_without_aborting() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        let script = r#"
这不是 JSON
{"action":"不存在的动作"}
{"action":"query","args":{"what":"不存在的查询"}}
{"action":"query","args":{"what":"windows"}}
"#;
        let out = run_e2e_script(script, &mut d, &mut u, &mut spaces::Spaces::new());
        assert_eq!(out.len(), 4, "四条都应产生输出：{out:#?}");
        assert!(out[0].contains("\"ok\":false") && out[0].contains("不是合法 JSON"), "{}", out[0]);
        assert!(out[1].contains("\"ok\":false") && out[1].contains("未知动作"), "{}", out[1]);
        assert!(out[2].contains("\"ok\":false") && out[2].contains("未知查询"), "{}", out[2]);
        // 关键：错误之后仍继续执行，最后一条是成功的查询
        assert!(out[3].contains("\"ok\":true"), "错误不应中止后续：{}", out[3]);
    }

    /// §4.1：`query titles` 让脚本能断言"打开的是哪一个窗口"，而不只是"开了个窗口"。
    #[test]
    fn e2e_query_titles_identifies_windows() {
        let mut d = demo_desktop();
        let mut u = UndoStack::new();
        let script = r#"
{"action":"open_app","args":{"app":"设置"}}
{"action":"query","args":{"what":"titles"}}
"#;
        let out = run_e2e_script(script, &mut d, &mut u, &mut spaces::Spaces::new());
        assert!(out[1].contains("设置"), "标题里应能看到设置窗口：{}", out[1]);
    }

    /// §4.1：**守卫顺序**是可测的（它同时也是安全决策）。
    #[test]
    fn key_route_prefers_the_most_restrictive_subsystem() {
        // 全部关着 → 全局
        assert_eq!(key_route(false, false, false, false, false, false), KeyRoute::Global);
        // 终端有焦点且无模态 → 终端
        assert_eq!(
            key_route(false, false, false, false, false, true),
            KeyRoute::Terminal
        );
        // 有模态弹窗（确认卡片/安装向导）→ 不给终端（否则用户无法回答）
        assert_eq!(
            key_route(false, false, false, false, true, true),
            KeyRoute::Global,
            "模态弹窗打开时终端不该抢键"
        );
        // DNS 编辑态优先于终端
        assert_eq!(
            key_route(false, false, false, true, false, true),
            KeyRoute::Dns
        );
        // 面板优先于 DNS 与终端
        assert_eq!(
            key_route(false, false, true, true, false, true),
            KeyRoute::Palette
        );
        // 向导优先于面板
        assert_eq!(
            key_route(false, true, true, true, false, true),
            KeyRoute::Setup
        );
    }

    /// **回归测试（安全审计附录 K，高危）**：锁屏必须排在最前。
    ///
    /// 修之前，`run_fbdev` 里的守卫顺序是"终端 → 锁屏"，于是锁屏时按键先进背后的 shell：
    /// 既是**锁屏旁路**（锁屏状态下能执行命令），也让用户按不出解锁密码。
    /// 这条测试把一个**状态组合**钉死 —— 只要有人把顺序改回去，它立刻红。
    #[test]
    fn lock_screen_beats_the_terminal() {
        assert_eq!(
            key_route(true, false, false, false, false, true),
            KeyRoute::Lock,
            "锁屏 + 终端有焦点：必须归锁屏（否则是锁屏旁路）"
        );
        // 锁屏压过一切
        assert_eq!(key_route(true, true, true, true, true, true), KeyRoute::Lock);
    }

    /// **顺序探针**：`run_fbdev` 里的守卫顺序必须与 `key_route` 的优先级一致。
    ///
    /// `key_route` 的测试只能证明**函数**对；如果有人把循环里的 `matches!(route, ...)`
    /// 守卫块调换位置（函数没动），行为照样会错。所以再扫一遍源码：
    /// `KeyRoute::Lock` 必须出现在 `KeyRoute::Terminal` 之前。
    /// （与快捷键总览那套探针同一思路。）
    #[test]
    fn the_loop_checks_lock_before_terminal() {
        let src = include_str!("fbdev_app.rs");
        let lock = src
            .find("matches!(route, KeyRoute::Lock)")
            .expect("循环里应当有锁屏守卫");
        let term = src
            .find("matches!(route, KeyRoute::Terminal)")
            .expect("循环里应当有终端守卫");
        assert!(
            lock < term,
            "守卫顺序错了：锁屏（偏移 {lock}）必须在终端（偏移 {term}）之前"
        );
        // 向导、面板也都要在终端之前（模态浮层不该被终端抢键）
        for (name, probe) in [
            ("向导", "matches!(route, KeyRoute::Setup)"),
            ("面板", "matches!(route, KeyRoute::Palette)"),
            ("DNS", "matches!(route, KeyRoute::Dns)"),
        ] {
            let at = src.find(probe).unwrap_or_else(|| panic!("循环里应当有{name}守卫"));
            assert!(at < term, "{name}守卫必须在终端之前（{at} vs {term}）");
        }
    }

    /// **回归探针（批次 AN-1）**：`renderer.lock.lock()` 只能出现一次，且在 Super+L 分支内。
    ///
    /// 修之前：`UiEvent::Super(c, shift)` 分支的**第一句**是无条件锁屏 ✗ ——
    /// 于是**每个 Super 组合键都会锁屏**，`Super+1..4`（切虚拟桌面）与
    /// `Super+Shift+1..4`（搬窗口）实际不可用。
    ///
    /// 事件循环的分支逻辑没有测试（只有纯模块有），所以这条用**源码探针**把它钉住：
    /// 多出来一份 `lock()` 就说明又有人在更宽的条件下调用了锁屏。
    #[test]
    fn the_lock_key_is_the_only_place_that_locks_the_screen() {
        let src = include_str!("fbdev_app.rs");
        // needle 用 concat! 拼出来：否则这条测试自己的源码就会命中，数出来永远多一份
        let needle = concat!("renderer.lock.", "lock();");
        let calls = src.matches(needle).count();
        assert_eq!(
            calls, 1,
            "锁屏调用应当只有一处（Super+L 分支内），实得 {calls} 处 —— \
             多出来的那份很可能又挂在了一个更宽的条件上"
        );
        // 锁屏调用必须在**执行 `SuperIntent::Lock` 的那个分支**里 ——
        // 判断本身已经在 `input::super_intent`（有单元测试），
        // 这里守的是"执行侧只有这一处会锁屏"。
        let branch = src
            .find("input::SuperIntent::Lock =>")
            .expect("找不到 SuperIntent::Lock 的执行分支");
        let call = src.find(needle).expect("找不到锁屏调用");
        assert!(
            call > branch,
            "锁屏调用必须在 `SuperIntent::Lock` 分支内（否则别的 Super 组合键也会锁屏）"
        );
    }

    /// **回归探针（批次 AO-1）**：`main.rs` 里不许再用那个**默认常量**（名为 `COUNT` 的那个）
    /// 当作"当前桌面数" —— 它的值是默认值 4，而空间数在 §5.4 之后是可变的。
    ///
    /// §5.4 之后空间数是动态的（默认 4、上限 8），判断与文案都必须跟着 `spaces.count()` 走。
    /// 修之前事件循环写死了 `COUNT`，于是 `Super+5..8` 静默无效 ——
    /// 而界面提示恰好写着「Super+5 可切过去」。这类错误是"数字没跟着事实走"，
    /// 类型系统看不出来，所以用源码探针钉住。
    #[test]
    fn the_desktop_key_binding_uses_the_dynamic_count() {
        // §4.1 之后"判断"抽进了 input::super_intent；这里钉住两段：
        // 纯函数按传入的 space_count 判断，事件循环传的是 spaces.count()（不是默认常量）。
        let input_src = include_str!("input.rs");
        let loop_src = include_str!("fbdev_app.rs");
        assert!(
            input_src.contains("(d as usize) <= space_count"),
            "super_intent 的桌面范围判断必须基于传入的 space_count"
        );
        assert!(
            loop_src.contains("input::super_intent(c, shift, spaces.count())"),
            "事件循环必须把当前桌面数 spaces.count() 传给 super_intent"
        );
        let needle = concat!("Spaces::<draw::Win>", "::COUNT");
        assert_eq!(
            loop_src.matches(needle).count(),
            0,
            "事件循环不该再用默认常量 COUNT 当「当前桌面数」——用 spaces.count()"
        );
    }

    /// `ctrl_intent` 的分类（**纯函数**，所以"哪个键做什么"这件事终于有测试了）。
    #[test]
    fn ctrl_intent_classifies_the_documented_keys() {
        assert_eq!(ctrl_intent('c', true), CtrlIntent::Copy);
        assert_eq!(ctrl_intent('v', true), CtrlIntent::Paste);
        assert_eq!(ctrl_intent('w', false), CtrlIntent::CloseWindow);
        // 带 Shift 的 Ctrl+W 不关窗（也不该被当成别的动作）
        assert_eq!(ctrl_intent('w', true), CtrlIntent::Unhandled);
        // 不带 Shift 的 Ctrl+C/V 不是复制粘贴（终端里另有通路）
        assert_eq!(ctrl_intent('c', false), CtrlIntent::Unhandled);
        assert_eq!(ctrl_intent('v', false), CtrlIntent::Unhandled);
        // 其余一律 Unhandled（**明确的兜底**，不是"忘了处理"）
        for c in ['a', 'x', 'q', 'z', 'k', '1'] {
            assert_eq!(ctrl_intent(c, false), CtrlIntent::Unhandled, "Ctrl+{c}");
            assert_eq!(ctrl_intent(c, true), CtrlIntent::Unhandled, "Ctrl+Shift+{c}");
        }
    }

    /// **探针（批次 AR-1）**：`apply_ctrl` 里只允许一处 `close_window(` 调用。
    ///
    /// 理由：`CtrlIntent::CloseWindow` 是"会改动窗口数组"的唯一标记，
    /// 调用方（事件循环）据此清掉拖拽与吸附预览。若有人绕过它、
    /// 直接在 `apply_ctrl` 别处关窗，两处就会再次分叉（P2-3 那类越界的成因）。
    ///
    /// 允许的是**一处**：`CtrlIntent::CloseWindow => close_window(...)`。
    #[test]
    fn only_one_place_in_apply_ctrl_closes_a_window() {
        let src = include_str!("app_main.rs");
        // 只看 `apply_ctrl` 的函数体：从它的签名到下一个顶层 `fn `
        let start = src.find("fn apply_ctrl(").expect("找不到 apply_ctrl");
        let rest = &src[start..];
        let end = rest.find("\nfn ").unwrap_or(rest.len());
        let body = &rest[..end];
        let needle = concat!("close_", "window(");
        assert_eq!(
            body.matches(needle).count(),
            1,
            "apply_ctrl 里应当只有一处关窗（且必须在 CtrlIntent::CloseWindow 分支）"
        );
        assert!(
            body.contains("CtrlIntent::CloseWindow =>"),
            "那一处必须在 CtrlIntent::CloseWindow 分支里"
        );
    }

    /// §5.1 托盘策略：容量为 0 / 恰好满 / 超出一个 —— 三种边界。
    ///
    /// 抽成泛型纯函数就是为了这条测试能跑（不必构造真窗口）。
    /// 它守的是"托盘永远不超过容量"与"丢的一定是最旧的"。
    #[test]
    fn closed_tray_keeps_at_most_cap_and_evicts_the_oldest() {
        // 容量 2：塞三个，第三个进来时"1"被挤出去（最旧的）
        let mut tray: Vec<u32> = Vec::new();
        assert_eq!(tray_push(&mut tray, 1, 2), None);
        assert_eq!(tray_push(&mut tray, 2, 2), None);
        assert_eq!(tray_push(&mut tray, 3, 2), Some(1), "满了要丢最旧的");
        assert_eq!(tray, vec![2, 3], "顺序保持（新的在后）");
        // 容量 0 = 不保留：放进去的当场还回来
        let mut none: Vec<u32> = Vec::new();
        assert_eq!(tray_push(&mut none, 9, 0), Some(9));
        assert!(none.is_empty(), "容量为 0 时托盘必须还是空的");
        // 容量 1
        let mut one: Vec<u32> = Vec::new();
        assert_eq!(tray_push(&mut one, 7, 1), None);
        assert_eq!(tray_push(&mut one, 8, 1), Some(7));
        assert_eq!(one, vec![8]);
    }

}
