//! 文本渲染：fontdue 光栅化 + 亚像素混合，天然抗锯齿。
//!
//! 字体按平台择优加载：Linux（ISO 内）用打包进 rootfs 的文泉驿微米黑
//! （中文屏显，覆盖中英文）；Windows 预览期回退到系统微软雅黑。

use crate::draw::blend_pixel;

pub struct TextRenderer {
    regular: fontdue::Font,
    bold: fontdue::Font,
    /// 40px 下的行高基准，用于按目标字号换算
    base_ascent: f32,
}

const REGULAR_FONTS: &[&str] = &[
    // Linux：打包进 rootfs 的中文屏显字体（由 platform/build-iso.sh 从宿主拷入）
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    // Windows 预览
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/msyh.ttf",
    "C:/Windows/Fonts/simsun.ttc",
];
// 注意：wqy-microhei 无独立粗体，Linux 下粗体仍用同一 CJK 字体，
// 避免粗体回退到无中文字形的 DejaVu-Bold 导致中文标题缺字。
const BOLD_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "C:/Windows/Fonts/msyhbd.ttc",
    "C:/Windows/Fonts/msyhbd.ttf",
];

fn load_font(paths: &[&str]) -> Option<fontdue::Font> {
    for path in paths {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        match fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: false,
            },
        ) {
            Ok(font) => {
                eprintln!("aether-compositor: 字体已加载 {path}");
                return Some(font);
            }
            Err(e) => {
                // 解析失败要显式报出来：曾出现字体文件在位但因格式不被支持而静默缺字
                eprintln!("aether-compositor: 字体解析失败 {path}: {e}");
            }
        }
    }
    eprintln!("aether-compositor: 无可用字体，尝试过 {paths:?}");
    None
}

/// 小字号覆盖增益：fontdue 是无 hinting 的解析式光栅化，≤14px 时细笔画
/// 覆盖率偏低、中文发灰发虚。对覆盖率做低段线性增益换回清晰度——
/// 只改覆盖率曲线，不动字形几何（基线修复的 ymin 约定不受影响）。
fn sharpen(cov: f32, px: f32) -> f32 {
    if px >= 14.0 || cov <= 0.0 {
        return cov;
    }
    // 14px → 增益 1.0，11px → 1.45（每小 1px +0.15）
    let gain = 1.0 + 0.15 * (14.0 - px).max(0.0);
    (cov * gain).min(1.0)
}

impl TextRenderer {
    /// 打印字号相关的真实度量（垂直居中与行高换算的依据）。
    /// 换字体或升级 fontdue 后用 `--fonttest` 复跑，确认偏移量仍然成立。
    pub fn dump_metrics(&self, sizes: &[f32]) {
        eprintln!("aether-compositor: base_ascent={}", self.base_ascent);
        for &px in sizes {
            let m = self.regular.horizontal_line_metrics(px);
            let (lm, _) = self.regular.rasterize('国', px);
            let ink_c = lm.ymin as f32 + lm.height as f32 / 2.0; // 墨迹中心相对基线（+y 向上）
            eprintln!(
                "  {px}px: ascent={:?} descent={:?} line_gap={:?} | 国 ymin={} h={} → 墨迹中心距基线 {:.1}px",
                m.map(|m| m.ascent), m.map(|m| m.descent), m.map(|m| m.line_gap),
                lm.ymin, lm.height, ink_c
            );
        }
    }

    /// 在高度 `height` 的条带内垂直居中一行文本，返回 draw()/draw_bold() 的 y。
    ///
    /// draw() 的 y 是"行顶部"（baseline = y + ascent）。要让**字形墨迹**居中，
    /// 需从条带中心减掉 ascent，再加回字形墨迹中心到基线的距离——后者用
    /// "国"（CJK 字面基本占满 em 方框）代表，混排以中文为主时这是正确的基准。
    /// 换字体后用 `--fonttest` 复跑 dump_metrics 即可确认。
    pub fn vcenter(&self, top: f32, height: f32, px: f32) -> f32 {
        let px = px.round();
        let ascent = self
            .regular
            .horizontal_line_metrics(px)
            .map(|m| m.ascent)
            .unwrap_or(self.base_ascent * px / 40.0);
        let (m, _) = self.regular.rasterize('国', px);
        let ink_c = m.ymin as f32 + m.height as f32 / 2.0;
        top + height / 2.0 - ascent + ink_c
    }

    pub fn load() -> Option<Self> {
        let regular = load_font(REGULAR_FONTS)?;
        let bold = load_font(BOLD_FONTS).unwrap_or_else(|| {
            load_font(REGULAR_FONTS).expect("regular font already loaded once")
        });
        let base_ascent = regular
            .horizontal_line_metrics(40.0)
            .map(|m| m.ascent)
            .unwrap_or(32.0);
        Some(Self { regular, bold, base_ascent })
    }

    fn render(
        &self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        x: f32,
        y: f32,
        text: &str,
        px: f32,
        rgb: [u8; 3],
        alpha: f32,
        bold: bool,
    ) -> f32 {
        // 光栅化字号取整：非整数 px（过渡字号 12.5/13.5）的 AA 字形明显发虚。
        // 渲染与测量在各自入口做同一取整，布局不因取整漂移。
        let px = px.round();
        let scale = px / 40.0;
        let baseline = y + self.base_ascent * scale;
        let mut cx = x;
        for ch in text.chars() {
            if ch == ' ' {
                cx += px * 0.28;
                continue;
            }
            let font = if bold { &self.bold } else { &self.regular };
            let (m, bitmap) = font.rasterize(ch, px);
            if m.width == 0 || m.height == 0 {
                cx += m.advance_width;
                continue;
            }
            let gx = cx.round() as i32 + m.xmin;
            // fontdue 约定：ymin = 位图**底边**相对基线的偏移（+y 向上，负 = 低于基线）。
            // 屏幕坐标 y 向下，故位图顶行 = baseline - ymin - height。
            // 曾误写成 baseline + ymin（把底边当顶边），导致字形整体坠到基线下方：
            // 句点/下划线飞到半空、中英文基线错位。
            let gy = baseline.round() as i32 - m.ymin - m.height as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let cov = sharpen(bitmap[row * m.width + col] as f32 / 255.0, px) * alpha;
                    if cov <= 0.003 {
                        continue;
                    }
                    let px_x = gx + col as i32;
                    let px_y = gy + row as i32;
                    if px_x < 0 || px_y < 0 || px_x >= w as i32 || px_y >= h as i32 {
                        continue;
                    }
                    blend_pixel(buf, px_y as usize * w + px_x as usize, rgb, cov);
                }
            }
            cx += m.advance_width;
        }
        cx
    }

    /// 在 (x, y) 处绘制常规文本（y 为文字行顶部），返回结束后的 x 坐标。
    pub fn draw(
        &self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        x: f32,
        y: f32,
        text: &str,
        px: f32,
        rgb: [u8; 3],
        alpha: f32,
    ) -> f32 {
        self.render(buf, w, h, x, y, text, px, rgb, alpha, false)
    }

    /// 粗体版本（标题、品牌、时钟）。
    pub fn draw_bold(
        &self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        x: f32,
        y: f32,
        text: &str,
        px: f32,
        rgb: [u8; 3],
        alpha: f32,
    ) -> f32 {
        self.render(buf, w, h, x, y, text, px, rgb, alpha, true)
    }

    /// 测量文本宽度（像素）。
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        self.measure_with(text, px, false)
    }

    pub fn measure_bold(&self, text: &str, px: f32) -> f32 {
        self.measure_with(text, px, true)
    }

    fn measure_with(&self, text: &str, px: f32, bold: bool) -> f32 {
        // 与 render() 同步取整，保证测量宽度 == 实际绘制宽度
        let px = px.round();
        let mut w = 0.0;
        for ch in text.chars() {
            if ch == ' ' {
                w += px * 0.28;
                continue;
            }
            let font = if bold { &self.bold } else { &self.regular };
            w += font.rasterize(ch, px).0.advance_width;
        }
        w
    }
}

/// 品牌与界面文案（统一收口，后续做 i18n 时从这里抽走）
pub mod strings {
    pub const BRAND: &str = "Aether";
    pub const MENUS: &[&str] = &["文件", "编辑", "显示", "帮助"];
    /// 每个菜单的下拉项，索引与 MENUS 对齐
    pub const MENU_ITEMS: &[&[&str]] = &[
        &["新建窗口", "关闭窗口", "退出"],
        &["撤销", "重做", "复制", "粘贴"],
        &["两列", "三列", "独占堆叠", "自由布局"],
        &["关于 Aether"],
    ];
    pub const AI_BAR_HINT: &str = "询问 Aether，或说出你想做的事";
    pub const WIN_FILES: &str = "文件";
    pub const WIN_TERM: &str = "终端";
    pub const WIN_BROWSER: &str = "浏览器";
    pub const WIN_MUSIC: &str = "音乐";
    pub const WIN_SETTINGS: &str = "设置";
    /// Dock 第 6 图标（仅 Live ISO 会话显示）：打开安装向导
    pub const INSTALLER: &str = "安装";
    /// 文件窗口侧栏项
    pub const SIDEBAR_ITEMS: &[&str] = &["文档", "图片", "音乐", "项目"];
    /// 文件窗口网格标签（演示内容；网格按可用空间取前 N 项，
    /// 数量要够填满最高布局，否则窗口下半部会留下大片死灰）
    pub const FILE_NAMES: &[&str] = &[
        "工作", "设计稿", "截图", "报告", "预算表", "会议记录", "素材库", "归档", "下载", "备份",
        "笔记", "模板", "字体", "图标包", "草稿", "清单", "照片", "视频", "安装包", "旧项目",
    ];
    /// 音乐窗口曲目（演示内容）：(曲名, 艺人, 时长)。
    /// 数量要够把这扇窗填满，否则列表下方会留一大片空灰。
    pub const TRACKS: &[(&str, &str, &str)] = &[
        ("以太漂移", "Aether Ensemble", "4:12"),
        ("深空回声", "Nova Field", "3:48"),
        ("极光边界", "Aurora Line", "5:06"),
        ("静默轨道", "Orbit Minor", "3:21"),
        ("信号衰减", "Nova Field", "4:55"),
        ("夜航", "Aether Ensemble", "6:03"),
        ("低轨道", "Orbit Minor", "3:37"),
        ("冷启动", "Nova Field", "4:28"),
        ("视界线", "Aurora Line", "5:41"),
        ("微光层", "Aether Ensemble", "2:58"),
        ("长夜频道", "Nova Field", "4:09"),
        ("归航信号", "Aurora Line", "3:52"),
    ];
    /// 播放条文案
    pub const NOW_PLAYING: &str = "正在播放";
    /// 文件窗口状态栏右值
    pub const DISK_FREE: &str = "剩余 42.6 GB";
}

/// 从 UTC 时间戳推算北京时间字符串 HH:MM（预览期简化处理，系统化后走本地化时钟服务）
pub fn clock_str() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + 8 * 3600;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{h:02}:{m:02}")
}

/// 供 draw 模块调用的便捷封装（常规字重）。
pub fn draw_text(
    tr: &TextRenderer,
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: f32,
    y: f32,
    text: &str,
    px: f32,
    rgb: [u8; 3],
    alpha: f32,
) -> f32 {
    tr.draw(buf, w, h, x, y, text, px, rgb, alpha)
}
