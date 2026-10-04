//! 显示输出的如实报告（FEATURE-GAPS §3.8 的最小切片）。
//!
//! ## 范围
//!
//! **不驱动**第二个输出。本模块只读 `/sys/class/graphics/fb*` 把真实情况列出来，
//! 让用户与排查者看到"这台机器上到底有几个输出、系统在用哪一个"。
//!
//! 真正的多显示器要改的是渲染与坐标体系（布局/命中/拖拽/吸附/虚拟桌面全部跨输出），
//! 属重写级别 —— 这条边界写在文档里，也由本模块在界面上如实呈现。
//!
//! ## 与息屏的关系（容易被读错的一点）
//!
//! 息屏会**同时**熄灭 fb0..fb2（见 `aether-compositor` 里枚举的 blank 文件），
//! 但**绘制只发生在 fb0**。报告要把两件事分开，不能笼统写成"支持多显示器"。

use std::path::Path;

/// 合成器实际驱动的输出下标（对应 `/dev/fb0`，见 `aether-compositor/src/fbdev.rs`）。
///
/// 这个常量是"报告"与"实现"之间唯一的约定：实现改了（比如以后真的支持多输出），
/// 这里必须跟着改，否则报告会说谎。
pub const DRIVEN_INDEX: u32 = 0;

/// 一个 framebuffer 输出。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Output {
    /// 下标（`/dev/fb<N>` 的 N）。
    pub index: u32,
    /// 驱动给的名字，例如 `bochs-drmdrmfb`。
    pub name: String,
    pub xres: u32,
    pub yres: u32,
    pub bpp: u32,
    pub stride: u32,
}

/// 解析 sysfs 的 `virtual_size`（形如 `1024,768`）。
///
/// **只取前两个逗号字段**：真实格式就是 `W,H`，但如果哪天内核多给一个字段，
/// 这份"报告"不该因此变成 `?x?` —— 它只是给人看的诊断，取到宽高就够了。
/// 但**每一段仍必须是纯数字**：`1024,768abc` 这种要如实失败，不能猜。
pub fn parse_virtual_size(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().split(',');
    let a = it.next()?.trim().parse::<u32>().ok()?;
    let b = it.next()?.trim().parse::<u32>().ok()?;
    Some((a, b))
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// 扫描显示输出（`sys_graphics` 是 `/sys/class/graphics`，测试可注入临时目录）。
///
/// 目录名必须是 `fb<N>`；缺字段的按 0 处理（**如实呈现"读不到"**，
/// 而不是替它编一个 1024x768）。
pub fn scan(sys_graphics: &Path) -> Vec<Output> {
    let Ok(rd) = std::fs::read_dir(sys_graphics) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir_name = e.file_name().to_string_lossy().to_string();
        let Some(idx_str) = dir_name.strip_prefix("fb") else {
            continue;
        };
        let Ok(index) = idx_str.parse::<u32>() else {
            continue;
        };
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let (xres, yres) = read_trim(&dir.join("virtual_size"))
            .and_then(|s| parse_virtual_size(&s))
            .unwrap_or((0, 0));
        out.push(Output {
            index,
            name: read_trim(&dir.join("name")).unwrap_or_default(),
            xres,
            yres,
            bpp: read_trim(&dir.join("bits_per_pixel"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            stride: read_trim(&dir.join("stride"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        });
    }
    out.sort_by_key(|o| o.index);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aether-display-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 建一个 framebuffer 目录（`files` 是要写的属性）。
    fn fb(root: &Path, index: u32, files: &[(&str, &str)]) {
        let d = root.join(format!("fb{index}"));
        std::fs::create_dir_all(&d).unwrap();
        for (k, v) in files {
            std::fs::write(d.join(k), v).unwrap();
        }
    }

    #[test]
    fn virtual_size_parsing_covers_the_awkward_cases() {
        assert_eq!(parse_virtual_size("1024,768"), Some((1024, 768)));
        assert_eq!(parse_virtual_size(" 1920,1080 \n"), Some((1920, 1080)));
        assert_eq!(parse_virtual_size("1024,768,32"), Some((1024, 768)), "多余字段忽略");
        for bad in ["", "1024", ",768", "1024,", "a,b", "1024;768"] {
            assert_eq!(parse_virtual_size(bad), None, "{bad:?} 该解析失败");
        }
    }

    /// 扫描：按 fb 下标排序，忽略非 fb 目录与坏名字。
    #[test]
    fn scan_lists_framebuffers_in_index_order() {
        let root = tmp("scan");
        fb(&root, 1, &[("name", "second-drmfb"), ("virtual_size", "1280,1024"), ("bits_per_pixel", "32")]);
        fb(&root, 0, &[("name", "bochs-drmdrmfb"), ("virtual_size", "1024,768"), ("bits_per_pixel", "32"), ("stride", "4096")]);
        // 非 fb 目录、坏名字都跳过
        std::fs::create_dir_all(root.join("fbx")).unwrap();
        std::fs::create_dir_all(root.join("console")).unwrap();
        std::fs::write(root.join("fb0/not-a-dir"), "x").unwrap();
        let out = scan(&root);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].index, 0, "要按下标排序");
        assert_eq!(out[0].name, "bochs-drmdrmfb");
        assert_eq!((out[0].xres, out[0].yres, out[0].bpp, out[0].stride), (1024, 768, 32, 4096));
        assert_eq!(out[1].index, 1);
        assert_eq!(out[1].name, "second-drmfb");
        // 驱动下标由 DRIVEN_INDEX 表达（服务端把它发给界面，界面自己比）
        assert_eq!(out[0].index, DRIVEN_INDEX);
        assert_ne!(out[1].index, DRIVEN_INDEX, "第二个输出不在被驱动的下标上");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 属性读不到时**如实写 `?`**，不替它编一个分辨率。
    #[test]
    fn missing_attributes_are_reported_honestly_not_invented() {
        let root = tmp("missing");
        fb(&root, 0, &[("name", "mystery-fb")]); // 没有 virtual_size / bits_per_pixel
        let out = scan(&root);
        assert_eq!(out.len(), 1);
        // **读不到就如实是 0**，不替它编一个分辨率（界面据此显示 `?`）
        assert_eq!((out[0].xres, out[0].yres, out[0].bpp, out[0].stride), (0, 0, 0, 0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scanning_a_missing_directory_is_empty_not_an_error() {
        assert!(scan(Path::new("/sys/class/graphics-nope")).is_empty());
    }

    /// 驱动下标是"报告与实现之间唯一的约定"—— 钉住它，改实现时必须一起改。
    #[test]
    fn the_driven_index_is_part_of_the_contract() {
        assert_eq!(DRIVEN_INDEX, 0, "fbdev.rs 打开的是 /dev/fb0；改实现要同步改这里");
        let root = tmp("contract");
        fb(&root, 0, &[("virtual_size", "800,600"), ("bits_per_pixel", "32")]);
        let out = scan(&root);
        assert_eq!(out[0].index, DRIVEN_INDEX);
        let _ = std::fs::remove_dir_all(&root);
    }
}
