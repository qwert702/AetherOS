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
use std::time::Duration;

pub struct Pty {
    master: File,
    child: Child,
    /// 子进程是否已被回收（见 `Drop` 里的 PID 复用说明）
    reaped: bool,
}

/// 默认 shell 的回退值。用绝对路径避免 PATH 差异（PID 1 环境里 PATH 由 /init 设置）。
const SHELL_FALLBACK: &str = "/bin/sh";

/// 支持括号粘贴的 shell（首选）。
const SHELL_PREFERRED: &str = "/bin/bash";

/// 选一个 shell：**优先 bash**，没有就退回 `/bin/sh`。
///
/// 为什么（2026-10-02 审计 M-13）：终端粘贴的安全路径依赖"程序主动声明支持
/// 括号粘贴（DECSET 2004）"—— 那是 readline 的特性，**busybox 的 ash 不实现**。
/// 镜像里只有 busybox 时，粘贴多行文本就会走"换行即回车"的老路（逐行执行），
/// 而这条路径正是 M-13 想消除的场景。镜像已加 `BR2_PACKAGE_BASH=y`；
/// 这里再做一次存在性判断，好让"没有 bash 的旧镜像"仍然能用。
fn default_shell() -> &'static str {
    if std::path::Path::new(SHELL_PREFERRED).exists() {
        SHELL_PREFERRED
    } else {
        SHELL_FALLBACK
    }
}

/// 交给终端会话的 PATH。
///
/// 与 `platform/overlay/init` 导出的那条一致 —— `/usr/local/bin` 放的是"已装应用"的
/// 包装脚本，少了它用户装了应用却在终端里敲不到（这是该项目已验证过的行为）。
const SHELL_PATH: &str = "/usr/local/sbin:/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin";

/// 终端会话的 HOME。镜像里唯一的账号是 root（`/etc/passwd` 只有 root）。
const SHELL_HOME: &str = "/root";

/// `write_all` 遇到 `EAGAIN` 时的最大等待时长。
///
/// 有界是必须的：主循环每帧都可能往 PTY 写，不能因为一个卡住的 shell 拖住渲染。
const MAX_WRITE_WAIT: Duration = Duration::from_millis(50);
const WRITE_RETRY_INTERVAL: Duration = Duration::from_millis(1);

impl Pty {
    /// 开一对 PTY 并 exec shell。`cols`/`rows` 决定 shell 看到的窗口大小。
    pub fn spawn(cols: u16, rows: u16, cwd: Option<&str>) -> io::Result<Pty> {
        // 1) 开主端（不把它当控制终端，控制终端由子进程的 slave 承担）
        //
        // O_CLOEXEC（2026-10-02 审计 L-15）：不设的话这个 fd 会被子进程继承，
        // 于是"终端里跑的任何程序"都持有一个 PTY 主端句柄 —— 关掉它并不会让
        // 会话真正结束，而且多一层可被利用的句柄泄漏。
        let master = unsafe {
            libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC)
        };
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
        let mut cmd = Command::new(default_shell());
        // **env_clear + 显式白名单**，不要继承合成器的环境。
        //
        // 为什么（2026-10-02 审计 M-16）：合成器以 root 运行，而 UI 通道密钥的约定是
        // "`AETHER_UI_KEY` 环境变量优先"（合成器自己就这么读，见 `main.rs::ui_key`）。
        // 环境若被继承，终端里任意一条命令都能 `env` 看到密钥，进而自行注册 UI 通道
        // 并批准 L2/L3 操作 —— 那等于把确认门槛交给任意程序。
        // 同理也不该把其它无关变量（如调试开关）带进终端。
        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env_clear()
            .env("PATH", SHELL_PATH)
            .env("HOME", SHELL_HOME)
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
        //
        // fcntl 的返回值必须检查（审计 L-15）：F_GETFL 失败返回 -1，
        // 拿 -1 去 `| O_NONBLOCK` 会把标志位写成一堆垃圾；F_SETFL 失败则意味着
        // 主端仍是阻塞的 —— 而整个渲染循环建立在"它不阻塞"的前提上。
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut pty = Pty { master, child, reaped: false };
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

    /// 把字节写进 PTY 主端。
    ///
    /// 主端是 `O_NONBLOCK`：shell 来不及消费时 `write` 返回 `EAGAIN`。直接放弃会
    /// **静默丢掉用户键入的字节**（第四轮审查 P3-3）—— 大段粘贴尤其容易命中，
    /// 表现是"粘贴少了一截"，极难排查。所以对 `EAGAIN` 做**有界重试**。
    pub fn write_all(&mut self, bytes: &[u8]) {
        let mut off = 0;
        let mut waited = Duration::ZERO;
        while off < bytes.len() {
            let n = unsafe {
                libc::write(
                    self.master.as_raw_fd(),
                    bytes[off..].as_ptr() as *const libc::c_void,
                    bytes.len() - off,
                )
            };
            if n > 0 {
                off += n as usize;
                continue;
            }
            // n <= 0：EAGAIN（缓冲满，可重试）或真错误（EIO 等，放弃）。
            // EWOULDBLOCK 与 EAGAIN 在 Linux 上是同一个值，不必分开判。
            let would_block = io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN);
            if !would_block || waited >= MAX_WRITE_WAIT {
                break;
            }
            std::thread::sleep(WRITE_RETRY_INTERVAL);
            waited += WRITE_RETRY_INTERVAL;
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
            Ok(Some(st)) => {
                // 记下"已经回收"（审计 L-15）：回收后再按 pid 发信号有**PID 复用**风险 ——
                // 那个 pid 可能已经属于别人的进程。
                self.reaped = true;
                Some(st.code().unwrap_or(-1))
            }
            _ => None,
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // 关掉主端会让 shell 收到 SIGHUP；再显式 kill 一次兜底，避免留下孤儿进程。
        //
        // 但如果子进程**已经被 wait 回收**，就不要再 kill（审计 L-15）：
        // `Child::id()` 返回的是当初的 pid，进程没了之后这个 pid 可能已被系统复用，
        // 一个 SIGHUP 会打到无关进程上。先 try_wait 确认还活着再动手。
        if !self.reaped && self.child.try_wait().ok().flatten().is_none() {
            let pid = self.child.id() as libc::pid_t;
            unsafe { libc::kill(pid, libc::SIGHUP) };
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
