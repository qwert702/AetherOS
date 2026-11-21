//! wl_shm 共享内存：fd → mmap 取数据，offset/stride/format → RGBA 读像素。
//!
//! 分两层：
//! - **数据从哪来**（fd → mmap）：[`map_pool_fd`]。Linux 下 `mmap` 收到的
//!   pool fd；非 Linux 平台返回明确的不支持错误（stub，不 panic）。
//! - **怎么读像素**（offset/stride/format → RGBA）：纯逻辑，[`read_pixels`]。
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
    /// 当前平台不支持 fd → mmap 这条路径（Windows stub）。
    FdTransportUnsupported,
    /// `mmap` 失败，携带原始 errno。
    MapFailed(i32),
    /// pool 长度非法（0 字节或超出合理上限）。
    BadPoolSize(usize),
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
            ShmError::FdTransportUnsupported => {
                write!(f, "当前平台不支持 SCM_RIGHTS fd 传递与 mmap（仅 Linux 支持）")
            }
            ShmError::MapFailed(errno) => write!(f, "mmap shm pool 失败，errno={errno}"),
            ShmError::BadPoolSize(size) => write!(f, "非法 pool 长度 {size} 字节"),
        }
    }
}

/// 从 pool 数据里读出一个 buffer 的像素，统一转成 **RGBA8888**（字节序 R,G,B,A）。
///
/// 返回 `width * height * 4` 字节。**stride 可以大于 width×4** —— 这是常态：
/// 客户端为了对齐/性能，行末经常有 padding，逐行读而不是整块拷。
pub fn read_pixels(pool: &[u8], info: &BufferInfo) -> Result<Vec<u8>, ShmError> {
    let BufferInfo { offset, width, height, stride, format, .. } = *info;

    // ① 几何先挡住：负数 / 零 / 超上限。
    //
    // 2026-11-21：此前 `stride < width * 4` 是 **i32 乘法** —— width ≥ 2^29
    // 时 debug 构建直接 panic、release 构建回绕成负数，检查被绕过；后面的
    // `offset + stride * (height-1) + width * 4` 也没有 checked 运算，
    // 配合负 offset（协议层曾直接 `as usize`）可以让 need 回绕成小数后越界。
    // 现在的规则：先拒绝任何会在后续乘法里溢出的输入。
    const MAX_DIM: i32 = 16384; // 8K 单边
    const MAX_PIXELS: usize = 4096 * 4096; // 64 MiB 的 RGBA
    if width <= 0 || height <= 0 || width > MAX_DIM || height > MAX_DIM {
        return Err(ShmError::BadGeometry { width, height, stride });
    }
    let bpp = 4usize;
    let row_bytes = (width as usize)
        .checked_mul(bpp)
        .ok_or(ShmError::BadGeometry { width, height, stride })?;
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .filter(|p| *p <= MAX_PIXELS)
        .ok_or(ShmError::BadGeometry { width, height, stride })?;

    // ② stride 必须非负，且不小于一行像素的字节数（否则像素会互相覆盖）
    let stride = usize::try_from(stride)
        .map_err(|_| ShmError::BadGeometry { width, height, stride })?;
    if stride < row_bytes {
        return Err(ShmError::BadGeometry { width, height, stride: stride as i32 });
    }

    // ③ 边界一律 checked：任何一步溢出都当作"几何非法"，而不是回绕后放行
    let bad = ShmError::BadGeometry { width, height, stride: stride as i32 };
    let need = stride
        .checked_mul(height as usize - 1)
        .and_then(|tail| tail.checked_add(row_bytes))
        .and_then(|n| offset.checked_add(n))
        .ok_or_else(|| bad.clone())?;
    if need > pool.len() {
        return Err(ShmError::OutOfBounds { need, pool_size: pool.len() });
    }
    if format != format::XRGB8888 && format != format::ARGB8888 {
        return Err(ShmError::UnsupportedFormat(format));
    }

    let mut out = Vec::with_capacity(pixels * 4);
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

/// pool 长度上限（spike）：256 MiB。超过即拒绝 mmap，防止恶意客户端用
/// 一个巨大 size 把合成器地址空间/内存撑爆。
const MAX_POOL_SIZE: usize = 256 * 1024 * 1024;

/// 把 `SCM_RIGHTS` 收到的 pool fd 映射成字节数据。
///
/// Linux：`mmap(MAP_SHARED)` → 拷进 `Vec<u8>` → `munmap` + `close(fd)`。
///
/// **spike 阶段的刻意取舍**：当前 `Session` 需要 `Clone`（连接线程每批消息
/// 克隆一份同步给渲染主循环），而 mmap 映射区不能直接随 Clone 复制，所以
/// 这里读出后解除映射。生产化阶段应改为持有映射区（`Arc` 包裹）做零拷贝，
/// 避免每帧整池拷贝。fd 在成功与失败路径上都会被关闭。
///
/// 非 Linux：返回 [`ShmError::FdTransportUnsupported`]，不触碰 fd、不 panic。
pub fn map_pool_fd(fd: i32, size: usize) -> Result<Vec<u8>, ShmError> {
    if size == 0 || size > MAX_POOL_SIZE {
        close_fd(fd);
        return Err(ShmError::BadPoolSize(size));
    }
    map_pool_fd_impl(fd, size)
}

#[cfg(target_os = "linux")]
fn map_pool_fd_impl(fd: i32, size: usize) -> Result<Vec<u8>, ShmError> {
    use libc::{close, mmap, munmap, MAP_FAILED, MAP_SHARED, PROT_READ};
    // 合成器只从 pool 读像素（读出后拷进 Vec 再解除映射），因此映射权限取
    // PROT_READ：既兼容客户端以 O_RDWR 打开的常规 pool，也兼容只读 fd。
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            size,
            PROT_READ,
            MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == MAP_FAILED {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        unsafe { close(fd) };
        return Err(ShmError::MapFailed(errno));
    }
    let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }.to_vec();
    unsafe {
        munmap(ptr, size);
        close(fd);
    }
    Ok(data)
}

#[cfg(not(target_os = "linux"))]
fn map_pool_fd_impl(_fd: i32, _size: usize) -> Result<Vec<u8>, ShmError> {
    Err(ShmError::FdTransportUnsupported)
}

/// 关闭一个收到的 fd。
///
/// Linux：真正 `close`；非 Linux：不可能进来真实 fd（stub 路径直接报错），
/// 这里是空操作，避免平台相关类型泄漏到公共签名。
pub fn close_fd(fd: i32) {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::close(fd);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = fd;
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

    #[test]
    fn zero_pool_size_is_rejected() {
        let err = map_pool_fd(-1, 0).expect_err("0 长度必须拒绝");
        assert!(matches!(err, ShmError::BadPoolSize(0)), "实得: {err}");
    }

    /// 非 Linux：fd 映射必须返回明确的"平台不支持"错误，而不是 panic 或静默成功。
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_fd_mapping_is_explicit_error() {
        let err = map_pool_fd(42, 64).expect_err("Windows stub 必须报错");
        assert!(matches!(err, ShmError::FdTransportUnsupported), "实得: {err}");
    }

    /// Linux-only：真实文件 fd 经 mmap 读回字节（收到的 fd 由映射路径关闭）。
    #[cfg(target_os = "linux")]
    #[test]
    fn maps_real_file_fd_into_bytes() {
        use std::fs::File;
        use std::io::Write as _;
        use std::os::unix::io::IntoRawFd;
        let tmp = std::env::temp_dir().join("aether-shm-probe");
        {
            let mut f = File::create(&tmp).unwrap();
            f.write_all(&[1u8, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        }
        let file = File::open(&tmp).unwrap();
        let raw = file.into_raw_fd();
        let data = map_pool_fd(raw, 8).unwrap();
        assert_eq!(data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let _ = std::fs::remove_file(tmp);
    }

    /// **2026-11-21 回归**：会溢出的几何必须被拒绝 —— 不是 panic，也不是回绕放行。
    ///
    /// 修复前 `width * 4` 在 i32 上溢出（debug panic / release 变负数使检查失效），
    /// `need` 也没有 checked 运算，配合巨大 offset 可以越界切片。
    #[test]
    fn overflow_prone_geometry_is_rejected() {
        let data = vec![0u8; 64];
        // ① 超宽 / 超单边上限：`width * 4` 会溢出 i32
        for (w, h, s) in [(1 << 29, 1, 8), (1 << 30, 1, 4), (16385, 1, 4)] {
            let err = read_pixels(&data, &info(w, h, s, format::XRGB8888))
                .expect_err("超宽必须拒绝");
            assert!(
                matches!(err, ShmError::BadGeometry { .. }),
                "{w}×{h}/{s} 应报几何非法，实得：{err}"
            );
        }
        // ② 像素总数超上限（4096×4096）
        let err = read_pixels(&data, &info(4097, 4097, 4097 * 4, format::XRGB8888))
            .expect_err("超过 4096×4096 的像素总数必须拒绝");
        assert!(matches!(err, ShmError::BadGeometry { .. }), "实得：{err}");
        // ③ 负 stride
        let err = read_pixels(&data, &info(1, 1, -4, format::XRGB8888))
            .expect_err("负 stride 必须拒绝");
        assert!(matches!(err, ShmError::BadGeometry { .. }), "实得：{err}");
        // ④ 巨大 offset：旧代码在 usize 上回绕后可以绕过边界检查
        let mut i = info(1, 1, 4, format::XRGB8888);
        i.offset = usize::MAX - 3;
        assert!(
            read_pixels(&data, &i).is_err(),
            "巨大 offset 必须被拒绝（而不是回绕成小数）"
        );
    }
}
