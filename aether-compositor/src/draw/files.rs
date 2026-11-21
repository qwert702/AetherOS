//! 文件/路径/预览数据：目录条目、路径工具、文本预览读取与面包屑。
use super::*;

/// 文件系统条目 —— 文件管理器的数据源。
#[derive(Clone, Debug)]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

impl FsEntry {
    /// 人类可读的大小（目录不显示大小）。
    pub fn size_label(&self) -> String {
        if self.is_dir {
            return "—".into();
        }
        let n = self.size;
        if n < 1024 {
            format!("{n} B")
        } else if n < 1024 * 1024 {
            format!("{:.1} KB", n as f64 / 1024.0)
        } else if n < 1024 * 1024 * 1024 {
            format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

/// 默认起始目录：Linux 取家目录，Windows 预览取 USERPROFILE。
pub fn default_cwd() -> String {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    if cfg!(windows) { "C:/".into() } else { "/".into() }
}

/// 读取目录内容：目录在前，同类按名称（不区分大小写）排序。
///
/// 隐藏文件（以 `.` 开头）默认不列出 —— 与 Finder/资源管理器的默认行为一致，
/// 也避免一进家目录就被 `.cargo`/`.config` 刷屏。
///
/// 读失败时返回空列表与原因字符串：**调用方必须把原因显示出来**，
/// 否则用户看到的是"空目录"，与"没权限"无法区分。
pub fn read_dir_entries(path: &str) -> (Vec<FsEntry>, Option<String>) {
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => return (Vec::new(), Some(format!("{e}"))),
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let meta = ent.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        out.push(FsEntry { name, is_dir, size });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    (out, None)
}

/// 路径拼接（处理结尾斜杠，Windows 下也兼容正斜杠）。
pub fn join_path(base: &str, name: &str) -> String {
    if base.ends_with('/') || base.ends_with('\\') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// 路径是否已在根（无法再向上）。
pub fn path_is_root(path: &str) -> bool {
    let t = path.trim_end_matches(['/', '\\']);
    t.is_empty() || t.ends_with(':')
}

/// 取路径的上级；已在根时返回原值。
pub fn parent_of(path: &str) -> String {
    let t = path.trim_end_matches(['/', '\\']);
    if t.is_empty() || t.ends_with(':') {
        return path.to_string();
    }
    match t.rfind(['/', '\\']) {
        Some(0) => t[..1].to_string(),
        Some(i) if t[..i].ends_with(':') => t[..=i].to_string(),
        Some(i) => t[..i].to_string(),
        None => path.to_string(),
    }
}

/// 取路径末段作为窗口标题；根目录返回原路径。
pub fn path_leaf(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rsplit(['/', '\\']).next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => path.to_string(),
    }
}

/// 预览失败的原因（§3.5）。
///
/// 与 `aether_ipc::DnsError` 同一套理由：这一层是**数据层**，
/// 让它出中英两套文案就得依赖 `i18n`（分层坏掉）或维护对照表（必然漂移）。
/// 所以它只回答「**为什么**预览不了」，措辞交给界面（`preview_error_text`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    /// 目标是目录
    IsDir,
    /// 文件太大（带**已经格式化好的**大小文字，避免这一层再依赖 `text::human_bytes` 之外的规则）
    TooLarge(String),
    /// 看起来是二进制
    Binary,
    /// 读文件失败（系统的错误文本原样带过来 —— 那不是我们的话，不该改）
    Io(String),
    /// **界面自己的**说明文字（已经按语言取好）。
    ///
    /// 存在的理由：通知中心复用 `PreviewData` 表达"空态"（"还没有通知" /
    /// "通知记录已关闭…"），那两句是**界面**说的，不是数据层说的 ——
    /// 所以由界面按语言取好后放进这个变体，数据层只负责带着它走。
    Message(String),
}

/// 把 `PreviewError` 渲染成**给用户看的一句话**（§3.5）。
///
/// 抽成纯函数：中英文两套文案都有测试，且"几种原因是否都覆盖"由编译器保证（`match` 穷尽）。
pub fn preview_error_text(lang: crate::i18n::Lang, e: &PreviewError) -> String {
    use crate::i18n::{pick, Lang};
    match e {
        PreviewError::IsDir => pick(
            lang,
            "这是一个目录",
            "This is a folder",
        )
        .to_string(),
        PreviewError::TooLarge(size) => {
            if lang == Lang::En {
                format!("File too large ({size}) — preview not supported yet")
            } else {
                format!("文件过大（{size}），暂不支持预览")
            }
        }
        PreviewError::Binary => pick(
            lang,
            "二进制文件，无法以文本预览",
            "Binary file — cannot preview as text",
        )
        .to_string(),
        // 系统错误与界面自带文案都原样显示：它们不是本模块的话，不该改
        PreviewError::Io(msg) => msg.clone(),
        PreviewError::Message(msg) => msg.clone(),
    }
}

/// 文本预览内容。
#[derive(Clone, Debug)]


pub struct PreviewData {
    /// 被打开文件的完整路径（标题栏显示）
    pub path: String,
    /// 已按行切好的内容（限长）
    pub lines: Vec<String>,
    /// 读取失败的原因；有值时窗口内显示它而不是内容（§3.5：**枚举**，文案由界面渲染）
    pub error: Option<PreviewError>,
    /// 是否因超长被截断
    pub truncated: bool,
    /// 滚动偏移（2.4：长文件能翻页，不再只能看前 N 行）。
    /// 夹取逻辑在 `textview::ScrollView` 里只有一份。
    pub scroll: crate::textview::ScrollView,
}

/// 预览单次读取的字节上限：再大就不是"看一眼"而是"打开大文件"了，
/// 而合成器是单线程渲染，读大文件会直接卡住整个界面。
pub(crate) const PREVIEW_MAX_BYTES: u64 = 64 * 1024;
/// 预览最多渲染的行数（超出部分只显示提示）
pub(crate) const PREVIEW_MAX_LINES: usize = 400;

/// 读取一个文件用于预览。二进制文件与超大文件都要给出明确原因，
/// 不能静默显示空白 —— 那和"文件是空的"无法区分。
pub fn read_preview(path: &str) -> PreviewData {
    let meta = std::fs::metadata(path);
    if let Ok(m) = &meta {
        if m.is_dir() {
            return PreviewData { path: path.into(), lines: Vec::new(), error: Some(PreviewError::IsDir), truncated: false, scroll: Default::default() };
        }
        if m.len() > PREVIEW_MAX_BYTES {
            return PreviewData {
                path: path.into(),
                lines: Vec::new(),
                error: Some(PreviewError::TooLarge(
                    FsEntry { name: String::new(), is_dir: false, size: m.len() }.size_label(),
                )),
                truncated: false,
                scroll: Default::default(),
            };
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            // 非 UTF-8 多半是二进制：不要用 lossy 糊一屏乱码，直接说明
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    return PreviewData { path: path.into(), lines: Vec::new(), error: Some(PreviewError::Binary), truncated: false, scroll: Default::default() };
                }
            };
            let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            let truncated = lines.len() > PREVIEW_MAX_LINES;
            if truncated {
                lines.truncate(PREVIEW_MAX_LINES);
            }
            PreviewData { path: path.into(), lines, error: None, truncated, scroll: Default::default() }
        }
        Err(e) => PreviewData {
            path: path.into(),
            lines: Vec::new(),
            error: Some(PreviewError::Io(e.to_string())), truncated: false, scroll: Default::default() },
    }
}

/// 路径 → 面包屑：(显示名, 目标路径)。Windows 盘符与 Unix 根都处理。
///
/// 纯函数，便于单测 —— 路径解析是最容易出边界 bug 的地方（盘符、根、结尾分隔符）。
pub fn crumbs(path: &str) -> Vec<(String, String)> {
    let sep = if path.contains('\\') { '\\' } else { '/' };
    let mut out = Vec::new();
    let mut acc = String::new();
    for (i, part) in path.split(sep).filter(|s| !s.is_empty()).enumerate() {
        if i == 0 {
            acc = if part.ends_with(':') {
                format!("{part}{sep}")
            } else {
                format!("{sep}{part}")
            };
        } else if acc.ends_with(sep) {
            acc.push_str(part);
        } else {
            acc = format!("{acc}{sep}{part}");
        }
        out.push((part.to_string(), acc.clone()));
    }
    out
}

/// 文本预览窗口一屏能显示多少行。
///
/// **必须与 `draw_preview_content` 的算法一致** —— 主循环用它算翻页步长，
/// 渲染用它算可见区间；两处不一致会导致翻页跳过或重复内容。
pub fn preview_visible_lines(rect: Rect) -> usize {
    const LINE_H: i32 = 18;
    ((rect.h - 20) / LINE_H).max(1) as usize
}

