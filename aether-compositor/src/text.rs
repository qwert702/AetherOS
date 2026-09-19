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

impl TextRenderer {
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
            let gx = cx as i32 + m.xmin;
            let gy = baseline as i32 + m.ymin;
            for row in 0..m.height {
                for col in 0..m.width {
                    let cov = bitmap[row * m.width + col] as f32 / 255.0 * alpha;
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
    pub const LOCAL_AI: &str = "本地模型在线";
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
