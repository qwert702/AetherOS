//! Linux framebuffer 后端：/dev/fb0（mmap 为主，write 兜底）。
//!
//! AetherOS 的 /dev/fb0 由 DRM 驱动提供（bochs-drm / virtio-gpu / vmwgfx / vboxvideo）
//! 或 VESA 帧缓冲。对 DRM 的 fbdev 模拟，mmap 是标准且可靠的通路；vesafb 对 mmap
//! 支持不佳时回退到 write() 逐帧写出。
//!
//! 启动时打印一次硬件信息与所选通路，便于无 GUI 环境下诊断（串口可见）。

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

const FBIOGET_VSCREENINFO: libc::c_int = 0x4600;
const FBIOPUT_VSCREENINFO: libc::c_int = 0x4601;
const FBIOGET_FSCREENINFO: libc::c_int = 0x4602;
const FB_ACTIVATE_NOW: u32 = 0;
const FB_ACTIVATE_FORCE: u32 = 0x80;

/// linux/fb.h 的 fb_var_screeninfo（x86_64 布局）
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct VarScreeninfo {
    pub xres: u32,
    pub yres: u32,
    pub xres_virtual: u32,
    pub yres_virtual: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub bits_per_pixel: u32,
    pub grayscale: u32,
    red: [u32; 3],
    green: [u32; 3],
    blue: [u32; 3],
    transp: [u32; 3],
    pub nonstd: u32,
    pub activate: u32,
    pub height: u32,
    pub width: u32,
    pub accel_flags: u32,
    pub pixclock: u32,
    pub left_margin: u32,
    pub right_margin: u32,
    pub upper_margin: u32,
    pub lower_margin: u32,
    pub hsync_len: u32,
    pub vsync_len: u32,
    pub sync: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub colorspace: u32,
    reserved: [u32; 4],
}

/// linux/fb.h 的 fb_fix_screeninfo（x86_64 布局）
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FixScreeninfo {
    pub id: [u8; 16],
    pub smem_start: usize,
    pub smem_len: u32,
    pub type_: u32,
    pub type_aux: u32,
    pub visual: u32,
    pub xpanstep: u16,
    pub ywrapstep: u16,
    pub line_length: u32,
    pub mmio_start: usize,
    pub mmio_len: u32,
    pub accel: u32,
    pub capabilities: u16,
    reserved: [u16; 2],
}

pub struct Fbdev {
    file: std::fs::File,
    pub width: usize,
    pub height: usize,
    pub bpp: u32,
    /// 像素字节序：由 fb 声明的通道偏移推出（**不写死 BGRX**，见 `PixelOrder`）
    order: PixelOrder,
    /// 行距（字节）：驱动可能要求 line_length != width * bpp/8，逐行写入时必须遵守
    stride: usize,
    /// 复用的帧缓冲（避免每帧 ~4MB 分配，10fps 下 ≈ 39MB/s 的分配压力）
    frame: Vec<u8>,
    /// 上一帧的像素（脏行检测用；鼠标移动时只有少数行需要重写）
    prev: Vec<u32>,
    /// 诊断信息只打印一次
    reported: bool,
}

/// 像素字节序。
///
/// 2026-09-30：真机浅色界面出现**整体暖偏**（红通道偏高、绿偏低），而纯白文字像素是中性 ——
/// 这类"偏色但图像结构正常"的症状，典型原因就是**字节序假设与实际 fb 不符**。
/// 此前 `blit` 对 32bpp 一律按 BGRX 写，现在按 `FBIOGET_VSCREENINFO` 里
/// red/green/blue 的 offset 决定顺序，并把结果打进日志（现场可查）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelOrder {
    /// 小端 32bpp，内存字节序 B,G,R,X（red.offset = 16，最常见）
    Bgrx,
    /// 小端 32bpp，内存字节序 R,G,B,X（red.offset = 0）
    Rgbx,
    /// 24bpp，内存字节序 B,G,R
    Bgr24,
    /// 24bpp，内存字节序 R,G,B
    Rgb24,
    /// 16bpp RGB565（red.offset = 11）
    Rgb565,
    /// 16bpp BGR565（red.offset = 0）
    Bgr565,
}

impl PixelOrder {
    /// 由 bpp 与通道偏移推断；**不认识的布局返回 None**（调用方按 bpp 回落并告警一次，
    /// 而不是默默按错误的顺序画出一屏偏色）。
    pub fn from_offsets(bpp: u32, r_off: u32, g_off: u32, b_off: u32) -> Option<Self> {
        Some(match (bpp, r_off, g_off, b_off) {
            (32, 16, 8, 0) => Self::Bgrx,
            (32, 0, 8, 16) => Self::Rgbx,
            (24, 16, 8, 0) => Self::Bgr24,
            (24, 0, 8, 16) => Self::Rgb24,
            (16, 11, 5, 0) => Self::Rgb565,
            (16, 0, 5, 11) => Self::Bgr565,
            _ => return None,
        })
    }

    /// 按 bpp 的保守回落（32bpp 用 BGRX —— 绝大多数 Linux fb 的默认）。
    pub fn fallback_for(bpp: u32) -> Self {
        match bpp {
            16 => Self::Rgb565,
            24 => Self::Bgr24,
            _ => Self::Bgrx,
        }
    }
}

impl Fbdev {
    /// 打开 /dev/fb0，读取硬件信息并尝试 mmap。
    pub fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")
            .context("打开 /dev/fb0 失败（内核需提供 framebuffer 驱动）")?;
        let fd = file.as_raw_fd();

        let mut var = VarScreeninfo::default();
        let mut fix = FixScreeninfo::default();
        let rc_v = unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var) };
        let rc_f = unsafe { libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix) };

        // 关键：内核的 DRM fbdev 模拟只创建 fb0，不会自动点亮 CRTC
        // （实测 crtc.enable=0、plane.fb=0 → 屏幕停留在引导文本模式）。
        // FBIOPUT_VSCREENINFO 会走到 drm_fb_helper_set_par，从而执行一次真正的
        // modeset，让 CRTC/平面接管显示。GET 失败时 var 全 0，此时回写等于
        // 请求 0x0 显示模式——只在 GET 成功时才 PUT。
        let rc_put = if rc_v == 0 {
            var.activate = FB_ACTIVATE_NOW | FB_ACTIVATE_FORCE;
            let rc_put = unsafe { libc::ioctl(fd, FBIOPUT_VSCREENINFO as _, &mut var) };
            unsafe {
                libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var);
            }
            rc_put
        } else {
            -1
        };

        let width = if var.xres > 0 { var.xres as usize } else { 800 };
        let height = if var.yres > 0 { var.yres as usize } else { 600 };
        let bpp = if var.bits_per_pixel > 0 { var.bits_per_pixel } else { 32 };
        let bytes_pp = (bpp / 8).max(1) as usize;
        let stride = if fix.line_length > 0 {
            fix.line_length as usize
        } else {
            width * bytes_pp
        };
        // 通道布局：`fb_bitfield` 在本文件里简化为 [offset, length, msb_right]
        let (r_off, g_off, b_off) = (var.red[0], var.green[0], var.blue[0]);
        let order = match PixelOrder::from_offsets(bpp, r_off, g_off, b_off) {
            Some(o) => o,
            None => {
                let f = PixelOrder::fallback_for(bpp);
                eprintln!(
                    "aether-compositor: 未识别的 fb 通道布局（bpp={bpp} R={r_off} G={g_off} B={b_off}）—— 按 {f:?} 回落，颜色可能不对"
                );
                f
            }
        };
        eprintln!(
            "aether-compositor: fbdev {}x{} @{}bpp stride={} 字节序={:?} 通道 R({},{}) G({},{}) B({},{}) ioctl(get var={},fix={} put={}) 通路=write",
            width, height, bpp, stride, order,
            var.red[0], var.red[1],
            var.green[0], var.green[1],
            var.blue[0], var.blue[1],
            rc_v, rc_f, rc_put
        );

        let frame = vec![0u8; stride * height.max(1)];
        Ok(Self {
            file,
            width,
            height,
            bpp,
            order,
            stride,
            frame,
            prev: Vec::new(),
            reported: false,
        })
    }

    /// 把 ARGB(u32, 0x00RRGGBB) 帧写入帧缓冲。
    ///
    /// 走 write() 系统调用（fb_sys_write 路径）。实测 mmap 写入首帧后
    /// 会触发 DRM deferred-IO 刷新把 guest 卡死，故不采用 mmap。
    pub fn blit(&mut self, buf: &[u32], w: usize, h: usize) {
        let fw = w.min(self.width);
        let fh = h.min(self.height);
        let row_bytes = fw * ((self.bpp / 8).max(1) as usize);
        let stride = self.stride.max(row_bytes);

        // 逐行按硬件 stride 排布进复用缓冲：行距大于行宽时，行尾 padding 保持黑色
        for row in 0..fh {
            let mut dst = row * stride;
            for &p in &buf[row * w..row * w + fw] {
                let r = (p >> 16) & 0xff;
                let g = (p >> 8) & 0xff;
                let b = p & 0xff;
                match self.order {
                    PixelOrder::Bgrx | PixelOrder::Rgbx => {
                        // 32bpp：第 4 字节写 0xff（部分驱动把 X 位当 alpha 用，留 0 会变全黑）
                        let (b0, b1, b2) = if self.order == PixelOrder::Bgrx {
                            (b, g, r)
                        } else {
                            (r, g, b)
                        };
                        self.frame[dst] = b0 as u8;
                        self.frame[dst + 1] = b1 as u8;
                        self.frame[dst + 2] = b2 as u8;
                        self.frame[dst + 3] = 0xff;
                        dst += 4;
                    }
                    PixelOrder::Bgr24 | PixelOrder::Rgb24 => {
                        let (b0, b1, b2) = if self.order == PixelOrder::Bgr24 {
                            (b, g, r)
                        } else {
                            (r, g, b)
                        };
                        self.frame[dst] = b0 as u8;
                        self.frame[dst + 1] = b1 as u8;
                        self.frame[dst + 2] = b2 as u8;
                        dst += 3;
                    }
                    PixelOrder::Rgb565 | PixelOrder::Bgr565 => {
                        let v = if self.order == PixelOrder::Rgb565 {
                            (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16)
                        } else {
                            (((b >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((r >> 3) as u16)
                        };
                        self.frame[dst..dst + 2].copy_from_slice(&v.to_le_bytes());
                        dst += 2;
                    }
                }
            }
        }

        use std::io::{Seek, SeekFrom, Write};
        let _ = self.file.seek(SeekFrom::Start(0));
        match self.file.write(&self.frame[..stride * fh]) {
            Ok(n) if n == stride * fh => {}
            Ok(n) => {
                if !self.reported {
                    self.reported = true;
                    eprintln!(
                        "aether-compositor: fb 短写 {n}/{} 字节（驱动限制了单次写入量）",
                        stride * fh
                    );
                }
            }
            Err(e) => {
                if !self.reported {
                    self.reported = true;
                    eprintln!("aether-compositor: fb write 失败: {e}");
                }
            }
        }
    }

    /// 只把**与上一帧不同的行**写到帧缓冲。
    ///
    /// 动机：鼠标移动时整屏 99% 的像素没变，但 `blit()` 每帧仍要组装并
    /// write 整屏（1280x800x4 = 4MB）。在 VM 里这笔系统调用足以把帧率压下来。
    /// 脏行检测本身只是一次内存比较（切片 `!=` 走 memcmp，约 1ms），
    /// 换来的是"鼠标移动只写几行"。
    ///
    /// 尺寸变化或首帧退回全量；整屏无变化时一次写都不做。
    pub fn blit_dirty(&mut self, buf: &[u32], w: usize, h: usize) {
        let fw = w.min(self.width);
        let fh = h.min(self.height);
        let row_bytes = fw * ((self.bpp / 8).max(1) as usize);
        let stride = self.stride.max(row_bytes);

        if self.prev.len() != fw * fh {
            self.prev = vec![0; fw * fh];
            self.blit(buf, w, h);
            for y in 0..fh {
                self.prev[y * fw..(y + 1) * fw].copy_from_slice(&buf[y * w..y * w + fw]);
            }
            return;
        }

        // 1. 找脏行范围（一次扫描，无分配）
        let Some((y0, y1)) = crate::draw::dirty_rows(&self.prev, buf, w, fw, fh) else {
            return; // 整屏没变
        };

        // 2. 只组装脏行
        for row in y0..y1 {
            let mut dst = row * stride;
            for &p in &buf[row * w..row * w + fw] {
                let r = (p >> 16) & 0xff;
                let g = (p >> 8) & 0xff;
                let b = p & 0xff;
                match self.order {
                    PixelOrder::Bgrx | PixelOrder::Rgbx => {
                        // 32bpp：第 4 字节写 0xff（部分驱动把 X 位当 alpha 用，留 0 会变全黑）
                        let (b0, b1, b2) = if self.order == PixelOrder::Bgrx {
                            (b, g, r)
                        } else {
                            (r, g, b)
                        };
                        self.frame[dst] = b0 as u8;
                        self.frame[dst + 1] = b1 as u8;
                        self.frame[dst + 2] = b2 as u8;
                        self.frame[dst + 3] = 0xff;
                        dst += 4;
                    }
                    PixelOrder::Bgr24 | PixelOrder::Rgb24 => {
                        let (b0, b1, b2) = if self.order == PixelOrder::Bgr24 {
                            (b, g, r)
                        } else {
                            (r, g, b)
                        };
                        self.frame[dst] = b0 as u8;
                        self.frame[dst + 1] = b1 as u8;
                        self.frame[dst + 2] = b2 as u8;
                        dst += 3;
                    }
                    PixelOrder::Rgb565 | PixelOrder::Bgr565 => {
                        let v = if self.order == PixelOrder::Rgb565 {
                            (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16)
                        } else {
                            (((b >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((r >> 3) as u16)
                        };
                        self.frame[dst..dst + 2].copy_from_slice(&v.to_le_bytes());
                        dst += 2;
                    }
                }
            }
        }

        // 3. 只写脏行（一次 seek + 一次 write）
        use std::io::{Seek, SeekFrom, Write};
        let _ = self.file.seek(SeekFrom::Start((y0 * stride) as u64));
        let end = (y1 * stride).min(self.frame.len());
        let _ = self.file.write(&self.frame[y0 * stride..end]);

        // 4. 更新基准
        for y in y0..y1 {
            self.prev[y * fw..(y + 1) * fw].copy_from_slice(&buf[y * w..y * w + fw]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PixelOrder;

    /// 通道偏移 → 字节序。**这是"真机偏色"那类问题的第一道闸**：
    /// 判错就会按错误顺序写像素，症状正是"图像结构正常、但整体偏色"。
    #[test]
    fn pixel_order_from_channel_offsets() {
        // 32bpp 的两种常见布局
        assert_eq!(PixelOrder::from_offsets(32, 16, 8, 0), Some(PixelOrder::Bgrx));
        assert_eq!(PixelOrder::from_offsets(32, 0, 8, 16), Some(PixelOrder::Rgbx));
        // 24bpp
        assert_eq!(PixelOrder::from_offsets(24, 16, 8, 0), Some(PixelOrder::Bgr24));
        assert_eq!(PixelOrder::from_offsets(24, 0, 8, 16), Some(PixelOrder::Rgb24));
        // 16bpp
        assert_eq!(PixelOrder::from_offsets(16, 11, 5, 0), Some(PixelOrder::Rgb565));
        assert_eq!(PixelOrder::from_offsets(16, 0, 5, 11), Some(PixelOrder::Bgr565));
        // 不认识的布局必须返回 None（调用方告警并保守回落），而不是悄悄画错一屏
        assert_eq!(PixelOrder::from_offsets(32, 8, 16, 0), None);
        assert_eq!(PixelOrder::from_offsets(8, 0, 0, 0), None);
        // 回落只看 bpp
        assert_eq!(PixelOrder::fallback_for(16), PixelOrder::Rgb565);
        assert_eq!(PixelOrder::fallback_for(24), PixelOrder::Bgr24);
        assert_eq!(PixelOrder::fallback_for(32), PixelOrder::Bgrx);
        assert_eq!(PixelOrder::fallback_for(8), PixelOrder::Bgrx);
    }
}

// `dirty_rows`（脏行检测）已移到 `draw.rs`：那个模块跨平台，其单元测试
// 在开发机上就能跑；fbdev 只在 Linux 编译，测试跑不到。
