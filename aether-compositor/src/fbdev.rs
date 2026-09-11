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
    /// 诊断信息只打印一次
    reported: bool,
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
        // modeset，让 CRTC/平面接管显示。
        var.activate = FB_ACTIVATE_NOW | FB_ACTIVATE_FORCE;
        let rc_put = unsafe { libc::ioctl(fd, FBIOPUT_VSCREENINFO as _, &mut var) };
        unsafe {
            libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var);
        }

        let width = if var.xres > 0 { var.xres as usize } else { 800 };
        let height = if var.yres > 0 { var.yres as usize } else { 600 };
        let bpp = if var.bits_per_pixel > 0 { var.bits_per_pixel } else { 32 };
        let bytes_pp = (bpp / 8).max(1) as usize;
        let line_length = if fix.line_length > 0 {
            fix.line_length as usize
        } else {
            width * bytes_pp
        };
        eprintln!(
            "aether-compositor: fbdev {}x{} @{}bpp stride={} ioctl(get var={},fix={} put={}) 通路=write",
            width, height, bpp, line_length, rc_v, rc_f, rc_put
        );

        Ok(Self {
            file,
            width,
            height,
            bpp,
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

        let mut bytes = Vec::with_capacity(fw * fh * 4);
        for row in 0..fh {
            for &p in &buf[row * w..row * w + fw] {
                let r = (p >> 16) & 0xff;
                let g = (p >> 8) & 0xff;
                let b = p & 0xff;
                match self.bpp {
                    16 => {
                        let v = (((r >> 3) as u16) << 11)
                            | (((g >> 2) as u16) << 5)
                            | ((b >> 3) as u16);
                        bytes.extend_from_slice(&v.to_le_bytes()[..]);
                    }
                    24 => {
                        bytes.push(b as u8);
                        bytes.push(g as u8);
                        bytes.push(r as u8);
                    }
                    _ => {
                        bytes.push(b as u8);
                        bytes.push(g as u8);
                        bytes.push(r as u8);
                        bytes.push(0xff);
                    }
                }
            }
        }

        use std::io::{Seek, SeekFrom, Write};
        let _ = self.file.seek(SeekFrom::Start(0));
        match self.file.write(&bytes) {
            Ok(n) if n == bytes.len() => {}
            Ok(n) => {
                if !self.reported {
                    self.reported = true;
                    eprintln!(
                        "aether-compositor: fb 短写 {n}/{} 字节（驱动限制了单次写入量）",
                        bytes.len()
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
}
