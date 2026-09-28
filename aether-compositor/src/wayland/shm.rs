//! wl_shm 共享内存的像素读取。
//!
//! 分两层：
//! - **数据从哪来**（fd → mmap）：平台相关，真机阶段接（Linux 下 `mmap` fd）。
//!   spike 阶段用 `InMemory` 模拟 —— 协议流程测试里直接塞字节。
//! - **怎么读像素**（offset/stride/format → RGBA）：纯逻辑，**这里做**。
//!   格式转换错一个字节序，客户端画红的上屏就是蓝的 —— 而且没有报错。
//!
//! ## 字节序（最容易错的地方）
//!
//! `WL_SHM_FORMAT_XRGB8888` 定义为 32 位值 `0x00RRGGBB`，按 **host 字节序**
//! 解释。x86/ARM 都是小端，所以内存里从低地址到高地址是：
//!
//! ```text
//! [BB] [GG] [RR] [00]
//! ```
//!
//! 常见的错误是当成 RGBA 数组去读 —— 那会得到红蓝互换的图像。

/// `wl_shm` 的像素格式枚举值（wayland.xml 里定义的常量）。
pub mod format {
    pub const ARGB8888: u32 = 0;
    pub const XRGB8888: u32 = 1;
}

/// 像素读取错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShmError {
    /// buffer 超出 pool 的边界 —— 客户端算错 stride/size 时会发生
    OutOfBounds { need: usize, pool_size: usize },
    /// 服务端不支持的格式（spike 只做 8888 系列）
    UnsupportedFormat(u32),
    /// 尺寸非法（负数 / 零宽高在协议上合法但读不出像素）
    BadGeometry { width: i32, height: i32, stride: i32 },
}

impl std::fmt::Display for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmError::OutOfBounds { need, pool_size } => {
                write!(f, "buffer 需要 {need} 字节，pool 只有 {pool_size}")
            }
            ShmError::UnsupportedFormat(fmt) => write!(f, "不支持的像素格式 {fmt:#x}"),
            ShmError::BadGeometry { width, height, stride } => {
                write!(f, "非法几何：{width}×{height}，stride {stride}")
            }
        }
    }
}

/// 从 pool 数据里读出一个 buffer 的像素，统一转成 **RGBA8888**（字节序 R,G,B,A）。
///
/// 返回 `width * height * 4` 字节。**stride 可以大于 width×4** —— 这是常态：
/// 客户端为了对齐/性能，行末经常有 padding，逐行读而不是整块拷。
pub fn read_pixels(pool: &[u8], info: &BufferInfo) -> Result<Vec<u8>, ShmError> {
    let BufferInfo { offset, width, height, stride, format, .. } = *info;
    if width <= 0 || height <= 0 || stride < width * 4 {
        return Err(ShmError::BadGeometry { width, height, stride });
    }
    let stride = stride as usize;
    let need = offset + stride * (height as usize - 1) + width as usize * 4;
    if need > pool.len() {
        return Err(ShmError::OutOfBounds { need, pool_size: pool.len() });
    }
    if format != format::XRGB8888 && format != format::ARGB8888 {
        return Err(ShmError::UnsupportedFormat(format));
    }

    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height as usize {
        let line = &pool[offset + row * stride..];
        for px in 0..width as usize {
            let b = line[px * 4];
            let g = line[px * 4 + 1];
            let r = line[px * 4 + 2];
            let a = if format == format::ARGB8888 { line[px * 4 + 3] } else { 255 };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(out)
}

/// buffer 的参数（像素数据在所属 pool 里）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    pub pool_id: u32,
    pub offset: usize,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(width: i32, height: i32, stride: i32, format: u32) -> BufferInfo {
        BufferInfo { pool_id: 1, offset: 0, width, height, stride, format }
    }

    /// 一个像素：按小端把 (r, g, b, a) 打进 8888 格式。
    fn px(fmt: u32, r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
        match fmt {
            format::XRGB8888 => [b, g, r, 0],
            format::ARGB8888 => [b, g, r, a],
            _ => unreachable!(),
        }
    }

    #[test]
    fn xrgb_roundtrip_single_pixel() {
        let data = px(format::XRGB8888, 0xff, 0x80, 0x10, 0).to_vec();
        let out = read_pixels(&data, &info(1, 1, 4, format::XRGB8888)).unwrap();
        assert_eq!(out, vec![0xff, 0x80, 0x10, 0xff], "XRGB 的 alpha 恒为 255");
    }

    /// 字节序是这里最容易错的：客户端画"纯红"，读出来必须是红不是蓝。
    #[test]
    fn pure_red_reads_back_as_red() {
        // 客户端写 0x00FF0000（XRGB 纯红）→ 小端字节 [00, 00, FF, 00]
        let data = [0u8, 0, 0xff, 0];
        let out = read_pixels(&data, &info(1, 1, 4, format::XRGB8888)).unwrap();
        assert_eq!(out, vec![0xff, 0x00, 0x00, 0xff], "R 在前");
    }

    #[test]
    fn pure_blue_reads_back_as_blue() {
        // 0x000000FF（纯蓝）→ 小端 [FF, 00, 00, 00]
        let data = [0xffu8, 0, 0, 0];
        let out = read_pixels(&data, &info(1, 1, 4, format::XRGB8888)).unwrap();
        assert_eq!(out, vec![0x00, 0x00, 0xff, 0xff]);
    }

    #[test]
    fn argb_preserves_alpha() {
        let data = px(format::ARGB8888, 10, 20, 30, 0x40).to_vec();
        let out = read_pixels(&data, &info(1, 1, 4, format::ARGB8888)).unwrap();
        assert_eq!(out, vec![10, 20, 30, 0x40], "ARGB 的 alpha 原样保留");
    }

    /// stride > width×4 是常态（行末有 padding）—— 必须逐行读。
    #[test]
    fn stride_padding_is_skipped_per_row() {
        let w = 2usize;
        let stride = 16; // 2 像素 8 字节 + 8 字节 padding
        let mut data = Vec::new();
        for row in 0..2u8 {
            data.extend_from_slice(&px(format::XRGB8888, row * 10, 0, 0, 0));
            data.extend_from_slice(&px(format::XRGB8888, row * 10 + 1, 0, 0, 0));
            data.extend_from_slice(&[0xaa; 8]); // padding，绝不能混进像素
        }
        let out = read_pixels(&data, &info(w as i32, 2, stride, format::XRGB8888)).unwrap();
        assert_eq!(out.len(), 2 * 2 * 4);
        // 第一行：r=0 和 r=1；第二行：r=10 和 r=11
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..8], &[1, 0, 0, 255]);
        assert_eq!(&out[8..12], &[10, 0, 0, 255]);
        assert_eq!(&out[12..16], &[11, 0, 0, 255]);
        assert!(!out.contains(&0xaa), "padding 泄漏进了像素");
    }

    #[test]
    fn offset_points_into_the_pool() {
        // 前面是别的数据，buffer 从 offset 起才开始
        let mut data = vec![0xee; 8];
        data.extend_from_slice(&px(format::XRGB8888, 1, 2, 3, 0));
        let mut i = info(1, 1, 4, format::XRGB8888);
        i.offset = 8;
        let out = read_pixels(&data, &i).unwrap();
        assert_eq!(out, vec![1, 2, 3, 255]);
    }

    #[test]
    fn out_of_bounds_is_rejected() {
        let data = vec![0u8; 8]; // 只够 2 像素
        let err = read_pixels(&data, &info(3, 1, 12, format::XRGB8888))
            .expect_err("3 像素放不进 8 字节");
        assert!(matches!(err, ShmError::OutOfBounds { .. }), "实得: {err}");
    }

    /// 越界校验必须考虑 stride —— 但只对**非最后一行**：最后一行只需 width×4。
    #[test]
    fn bounds_check_uses_stride_not_width() {
        // 1 行 1 像素、stride=64：最后一行不需要完整 stride，4 字节就够
        let data = vec![0u8; 4];
        assert!(read_pixels(&data, &info(1, 1, 64, format::XRGB8888)).is_ok());
        // 2 行 1 像素、stride=64：第二行从 64 起 → 需要 64+4=68 字节
        let err = read_pixels(&data, &info(1, 2, 64, format::XRGB8888))
            .expect_err("第二行从 stride 偏移开始，4 字节不够");
        assert!(matches!(err, ShmError::OutOfBounds { .. }), "实得: {err}");
    }

    #[test]
    fn bad_geometry_is_rejected() {
        let data = vec![0u8; 64];
        for (w, h, s) in [(0, 1, 4), (-1, 1, 4), (1, 0, 4), (2, 1, 4)] {
            let err = read_pixels(&data, &info(w, h, s, format::XRGB8888));
            assert!(matches!(err, Err(ShmError::BadGeometry { .. })), "{w}×{h}/{s} 应被拒");
        }
        // stride < width×4 也不合法（像素会互相覆盖）
        let err = read_pixels(&data, &info(2, 1, 4, format::XRGB8888));
        assert!(matches!(err, Err(ShmError::BadGeometry { .. })));
    }

    #[test]
    fn unsupported_format_is_rejected() {
        let data = vec![0u8; 4];
        let err = read_pixels(&data, &info(1, 1, 4, 0x36474747 /* RGB565 之类 */))
            .expect_err("非 8888 格式应被拒");
        assert!(matches!(err, ShmError::UnsupportedFormat(_)), "实得: {err}");
    }

    /// 真实场景：一张 4×2 的图，每个像素颜色不同，逐像素核对。
    #[test]
    fn multi_pixel_image() {
        let mut data = Vec::new();
        let mut expect = Vec::new();
        for y in 0..2i32 {
            for x in 0..4i32 {
                let (r, g, b) = ((x * 60) as u8, (y * 120) as u8, 0x77);
                data.extend_from_slice(&px(format::XRGB8888, r, g, b, 0));
                expect.extend_from_slice(&[r, g, b, 255]);
            }
        }
        let out = read_pixels(&data, &info(4, 2, 16, format::XRGB8888)).unwrap();
        assert_eq!(out, expect);
    }
}
