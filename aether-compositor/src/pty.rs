//! Linux PTY 后端：真实 shell 会话。
//!
//! 这是"从演示到能用"的那一步 —— 之前终端里的内容是 `draw.rs` 里一份写死的行列表。
//!
//! 只在本文件里碰系统调用，上游（`term.rs`/`draw.rs`）拿到的是跨平台的 `Screen`，
//! 所以 VT 解析与渲染仍能在开发机上测（见 `vt.rs` 的模块注释）。
//!
//! ⚠️ 本文件**无法在 Windows 开发机上运行**，只能做 Linux 目标的编译检查。
//! 因此这里刻意保持"薄"：逻辑越少，看不见的部分越少。

use std::fs::File;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    master: File,
    child: Child,
}

/// 默认 shell。用绝对路径避免 PATH 差异（PID 1 环境里 PATH 由 /init 设置）。
const SHELL: &str = "/bin/sh";

impl Pty {
    /// 开一对 PTY 并 exec shell。`cols`/`rows` 决定 shell 看到的窗口大小。
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&str>) -> io::Result<Pty> {
        // 1) 开主端（不把它当控制终端，控制终端由子进程的 slave 承担）
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = unsafe { File::from_raw_fd(master) };

        // 2) grantpt/unlockpt：没有授权与解锁，slave 打不开
        if unsafe { libc::grantpt(master.as_raw_fd()) } != 0
            || unsafe { libc::unlockpt(master.as_raw_fd()) } != 0
        {
            return Err(io::Error::last_os_error());
        }

        // 3) 取 slave 名字（ptsname_r 是可重入版本，避免静态缓冲的竞态）
        let mut buf = [0i8; 256];
        if unsafe { libc::ptsname_r(master.as_raw_fd(), buf.as_mut_ptr(), buf.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        let name = name.to_str().map_err(|_| io::Error::other("pts 名字不是 UTF-8"))?;

        let slave = std::fs::OpenOptions::new().read(true).write(true).open(name)?;

        // 4) 子进程：stdin/out/err 都指向 slave；pre_exec 里脱离会话并认领控制终端
        //
        // 用 Command + pre_exec 而不是手写 fork：手写 fork 在**多线程**进程里
        // （合成器有输入/AI 线程）后只能调 async-signal-safe 函数，容易埋雷。
        let mut cmd = Command::new(SHELL);
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color")
            .env("PS1", "$ ")
            .env("LANG", "C.UTF-8");
        if let Some(dir) = cwd {
            if std::path::Path::new(dir).is_dir() {
                cmd.current_dir(dir);
            }
        }
        unsafe {
            cmd.pre_exec(|| {
                // setsid：成为新会话首进程，否则拿不到控制终端（也就收不到 Ctrl+C）
                libc::setsid();
                // 认领控制终端：0 号 fd 此刻就是 slave
                libc::ioctl(0, libc::TIOCSCTTY, 0);
                Ok(())
            });
        }
        let child = cmd.spawn()?;

        // 5) 主端设非阻塞：主循环每帧只"取走现有数据"，绝不阻塞渲染
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };

        let mut pty = Pty { master, child };
        pty.resize(cols, rows);
        Ok(pty)
    }

    /// 取走当前可读的数据（非阻塞），最多 `max` 字节。
    ///
    /// 有上限是必要的：`cat` 一个大文件时内核缓冲里可能积压兆字节，
    /// 一次读完会让单帧渲染被 I/O 拖死。剩下的留在缓冲里，下一帧继续。
    pub fn read_available(&mut self, out: &mut Vec<u8>, max: usize) -> usize {
        let mut buf = [0u8; 4096];
        let mut total = 0;
        while total < max {
            let want = buf.len().min(max - total);
            let n = unsafe {
                libc::read(self.master.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, want)
            };
            if n <= 0 {
                break; // 0 = EOF；-1 = EAGAIN/EIO
            }
            out.extend_from_slice(&buf[..n as usize]);
            total += n as usize;
        }
        total
    }

    pub fn write_all(&mut self, bytes: &[u8]) {
        let mut off = 0;
        while off < bytes.len() {
            let n = unsafe {
                libc::write(
                    self.master.as_raw_fd(),
                    bytes[off..].as_ptr() as *const libc::c_void,
                    bytes.len() - off,
                )
            };
            if n <= 0 {
                break;
            }
            off += n as usize;
        }
    }

    /// 同步窗口大小给 shell（否则 `vim`/`top` 按默认 80x24 排版）。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let ws = libc::winsize { ws_col: cols, ws_row: rows, ws_xpixel: 0, ws_ypixel: 0 };
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    /// 子进程是否已退出；退出则返回状态码（只在第一次返回 Some）。
    pub fn try_wait(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(st)) => Some(st.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // 关掉主端会让 shell 收到 SIGHUP；再显式 kill 一次兜底，避免留下孤儿进程
        let pid = self.child.id() as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGHUP) };
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
