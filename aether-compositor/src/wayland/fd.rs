//! Wayland fd 传递（`SCM_RIGHTS`）接收层。
//!
//! ## 协议背景
//!
//! `wl_shm.create_pool` 的 fd 参数（签名 `h`）**不进消息体**：它随 `sendmsg`
//! 的 ancillary data（`SCM_RIGHTS`）一起传给对端。服务端因此不能用普通
//! `read` 收包 —— 必须用 `recvmsg` 读取 `msghdr.msg_control`，再按
//! `cmsghdr` 链表把 `SCM_RIGHTS` 里的 `i32` fd 逐个取出来。
//!
//! ## 平台分层
//!
//! - **Linux**：`recvmsg` + `cmsghdr` 解析，真实收 fd（[`imp::recvmsg_fds`]）。
//! - **其他平台（Windows 开发机）**：没有 Unix 域 socket 的 fd 传递语义，
//!   入口不参与编译；需要判断平台能力时用 [`fds_supported`]，走到映射路径
//!   的调用方会拿到明确错误，**不 panic**。
//!
//! ## fd 生命周期
//!
//! 收到的 fd 归属本进程：`SCM_RIGHTS` 传过来的是一份独立的描述符副本，
//! 用完必须 `close`。本模块只负责"收进来"；关闭责任在 `shm` / `session`
//! 映射失败或连接断开的路径上（见 `shm::close_fd`）。

use std::fmt;

/// fd 传递相关错误。
#[derive(Debug, PartialEq, Eq)]
pub enum FdError {
    /// 当前平台不支持 Unix socket 的 `SCM_RIGHTS` fd 传递（Windows stub 路径）。
    UnsupportedPlatform,
    /// `recvmsg` 系统调用失败，携带原始 errno。
    RecvFailed(i32),
    /// ancillary data 里 fd 数量超过 [`MAX_FDS`] 上限。
    TooManyFds(usize),
}

impl fmt::Display for FdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FdError::UnsupportedPlatform => {
                write!(f, "当前平台不支持 SCM_RIGHTS fd 传递（仅 Linux 支持）")
            }
            FdError::RecvFailed(errno) => write!(f, "recvmsg 失败，errno={errno}"),
            FdError::TooManyFds(n) => write!(f, "一次随附 {n} 个 fd，超过上限 {MAX_FDS}"),
        }
    }
}

impl std::error::Error for FdError {}

/// 单次收包最多携带的 fd 数。
///
/// Wayland 一条消息通常只传 1 个 pool fd；16 是留足余量的上限，
/// 防止恶意客户端在一条 ancillary 里灌爆描述符表。
pub const MAX_FDS: usize = 16;

/// 本次 `recvmsg` 收到的字节与随附 fd。
#[cfg(target_os = "linux")]
pub struct Received {
    /// 消息体字节数（不含 ancillary data）。
    pub bytes: usize,
    /// 从 `SCM_RIGHTS` 解析出的 fd，按线上到达顺序排列。
    pub fds: Vec<i32>,
}

/// 当前编译目标是否支持真实 fd 传递。
pub fn fds_supported() -> bool {
    cfg!(target_os = "linux")
}

/// 按 usize 宽度做 CMSG 对齐（`CMSG_ALIGN`）。
const fn cmsg_align(len: usize) -> usize {
    (len + std::mem::size_of::<usize>() - 1) & !(std::mem::size_of::<usize>() - 1)
}

/// 单个 cmsghdr 对齐后的头部长度（`CMSG_HDRLEN`）。
///
/// 引用 `libc::cmsghdr`，故整体在 Linux 分支下编译；非 Linux 平台不进入此路径。
#[cfg(target_os = "linux")]
fn cmsg_header_len() -> usize {
    cmsg_align(std::mem::size_of::<libc::cmsghdr>())
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{cmsg_align, cmsg_header_len, FdError, Received, MAX_FDS};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use libc::{
        cmsghdr, iovec, msghdr, recvmsg, MSG_CMSG_CLOEXEC, SCM_RIGHTS, SOL_SOCKET,
    };

    /// 从 Unix stream 收一条报文：消息体进 `buf`，ancillary 里的 fd 解析出来。
    ///
    /// 返回的 [`Received::bytes`] 为 0 表示对端正常关闭（EOF）。
    pub fn recvmsg_fds(stream: &UnixStream, buf: &mut [u8]) -> Result<Received, FdError> {
        let mut iov = iovec {
            iov_base: buf.as_mut_ptr() as *mut _,
            iov_len: buf.len(),
        };
        // 控制缓冲按 MAX_FDS 个 i32 的 CMSG_SPACE 预留，避免一条报文里
        // fd 太多把内核截断（MSG_CTRUNC）后我们解析到残缺数据。
        let cmsg_cap = cmsg_header_len() + cmsg_align(std::mem::size_of::<i32>() * MAX_FDS);
        let mut cmsg_buf = vec![0u8; cmsg_cap];

        let mut msg: msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut _;
        msg.msg_controllen = cmsg_buf.len() as _;

        let n = unsafe { recvmsg(stream.as_raw_fd(), &mut msg, MSG_CMSG_CLOEXEC) };
        if n < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(FdError::RecvFailed(errno));
        }

        let fds = parse_cmsgs(msg.msg_control, msg.msg_controllen as usize)?;
        Ok(Received { bytes: n as usize, fds })
    }

    /// 解析一块 ancillary 缓冲里的 `SCM_RIGHTS` fd。
    ///
    /// 抽成独立函数（不依赖 `msghdr` 的 iovec 部分）是为了能在单测里
    /// 手工拼一块 cmsg 缓冲直接喂进来，不必真的起 socketpair。
    pub(crate) fn parse_cmsgs(
        control: *mut std::ffi::c_void,
        controllen: usize,
    ) -> Result<Vec<i32>, FdError> {
        let mut fds = Vec::new();
        if control.is_null() || controllen == 0 {
            return Ok(fds);
        }

        let hdr_len = cmsg_header_len();
        let base = control as *const u8;
        let mut off = 0usize;
        unsafe {
            while off + std::mem::size_of::<cmsghdr>() <= controllen {
                let hdr = &*(base.add(off) as *const cmsghdr);
                // cmsg_len 在不同架构上是 size_t / c_ulong，`as usize` 保留跨架构可移植性。
                #[allow(clippy::unnecessary_cast)]
                let cmsg_len = hdr.cmsg_len as usize;
                // 头部残缺或越界：停止解析，而不是回绕后读出垃圾。
                if cmsg_len < hdr_len || off + cmsg_len > controllen {
                    break;
                }
                if hdr.cmsg_level == SOL_SOCKET && hdr.cmsg_type == SCM_RIGHTS {
                    let data = base.add(off + hdr_len) as *const i32;
                    let count = (cmsg_len - hdr_len) / std::mem::size_of::<i32>();
                    if fds.len() + count > MAX_FDS {
                        return Err(FdError::TooManyFds(fds.len() + count));
                    }
                    for i in 0..count {
                        fds.push(std::ptr::read(data.add(i)));
                    }
                }
                off += cmsg_align(cmsg_len);
            }
        }
        Ok(fds)
    }
}

#[cfg(target_os = "linux")]
pub use imp::recvmsg_fds;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_error_is_explicit_on_non_linux() {
        // 非 Linux 平台：错误类型本身要能说清"平台不支持"，而不是 panic。
        if !fds_supported() {
            let err = FdError::UnsupportedPlatform;
            assert!(err.to_string().contains("不支持"));
        }
    }

    /// 纯解析测试：手工拼一块 `SCM_RIGHTS` cmsg 缓冲，校验 fd 按序取出。
    ///
    /// cmsghdr / CMSG 布局是 Linux ABI，故本测试只在 Linux 下编译运行。
    #[cfg(target_os = "linux")]
    #[test]
    fn parse_synthetic_scm_rights_buffer() {
        use libc::{cmsghdr, SCM_RIGHTS, SOL_SOCKET};
        const FDS: [i32; 3] = [10, 11, 12];
        let hdr_len = cmsg_header_len();
        let cmsg_len = hdr_len + std::mem::size_of::<i32>() * FDS.len();
        let cmsg_space = cmsg_len;
        let mut buf = vec![0u8; cmsg_space];
        unsafe {
            let hdr = buf.as_mut_ptr() as *mut cmsghdr;
            (*hdr).cmsg_len = cmsg_len as _;
            (*hdr).cmsg_level = SOL_SOCKET;
            (*hdr).cmsg_type = SCM_RIGHTS;
            let data = buf.as_ptr().add(hdr_len) as *mut i32;
            for (i, fd) in FDS.iter().enumerate() {
                std::ptr::write(data.add(i), *fd);
            }
        }
        let fds = imp::parse_cmsgs(buf.as_mut_ptr() as *mut _, cmsg_len).unwrap();
        assert_eq!(fds, FDS);
    }

    /// 端到端（Linux-only）：socketpair 上 `sendmsg` 带一个文件 fd，
    /// `recvmsg_fds` 必须同时取到字节与 fd。
    ///
    /// Windows 无法运行该场景；Linux 下本测试在开发机/CI 执行，
    /// 实机结果以交付文档的"已测/未测"清单为准。
    #[cfg(target_os = "linux")]
    #[test]
    fn socketpair_transfers_bytes_and_one_fd() {
        use std::fs::File;
        use std::io::Read as _;
        use std::io::Write as _;
        use std::os::unix::io::{AsRawFd, FromRawFd};
        use std::os::unix::net::UnixStream;
        use libc::{cmsghdr, iovec, msghdr, sendmsg, SCM_RIGHTS, SOL_SOCKET};

        let (client, server) = UnixStream::pair().unwrap();

        // 造一个临时文件作为被传递的 fd 载体（真实客户端传的是 memfd / shm）。
        let tmp = std::env::temp_dir().join("aether-fd-probe");
        {
            let mut f = File::create(&tmp).unwrap();
            f.write_all(b"probe").unwrap();
        }
        let file = File::open(&tmp).unwrap();
        let raw = file.as_raw_fd();

        let payload = b"hello";
        let mut iov = iovec {
            iov_base: payload.as_ptr() as *mut _,
            iov_len: payload.len(),
        };
        let hdr_len = cmsg_header_len();
        let cmsg_space = hdr_len + std::mem::size_of::<i32>();
        let mut cmsg_buf = vec![0u8; cmsg_space];
        let mut msg: msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut _;
        msg.msg_controllen = cmsg_space as _;
        unsafe {
            let hdr = cmsg_buf.as_mut_ptr() as *mut cmsghdr;
            (*hdr).cmsg_len = (hdr_len + std::mem::size_of::<i32>()) as _;
            (*hdr).cmsg_level = SOL_SOCKET;
            (*hdr).cmsg_type = SCM_RIGHTS;
            let data = cmsg_buf.as_ptr().add(hdr_len) as *mut i32;
            std::ptr::write(data, raw);
        }
        let rc = unsafe { sendmsg(client.as_raw_fd(), &msg, 0) };
        assert!(rc >= 0, "sendmsg 失败: {}", std::io::Error::last_os_error());
        drop(file);

        let mut buf = [0u8; 64];
        let recv = recvmsg_fds(&server, &mut buf).unwrap();
        assert_eq!(recv.bytes, payload.len());
        assert_eq!(&buf[..recv.bytes], payload);
        assert_eq!(recv.fds.len(), 1, "应收到 1 个随附 fd");

        // 收到的 fd 是独立副本：能打开读到内容即说明传递成功。
        let mut received = unsafe { File::from_raw_fd(recv.fds[0]) };
        let mut txt = String::new();
        received.read_to_string(&mut txt).unwrap();
        assert_eq!(txt, "probe");

        let _ = std::fs::remove_file(tmp);
    }
}
