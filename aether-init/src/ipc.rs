//! aether-init 的服务控制 IPC：接收 ServiceControl 请求，操作服务管理器。

use crate::manager::{Manager, SvcState};
use aether_ipc::{decode, encode, Request, Response, ServiceAction, ServiceStatus, SysReport};
use std::io::{BufRead, BufReader, Read};
#[cfg(not(target_os = "linux"))]
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 单条请求行长度上限：超长行直接拒绝，不吃内存。
const MAX_LINE_BYTES: u64 = 64 * 1024;

/// 毒锁恢复：持锁线程 panic 不应让 PID 1 的 IPC 通道一起失效。
fn lock_ok(manager: &Mutex<Manager>) -> std::sync::MutexGuard<'_, Manager> {
    manager.lock().unwrap_or_else(|e| e.into_inner())
}

/// 服务控制 socket 路径（Linux）。PID 1 边界，仅 root 可连。
#[cfg(target_os = "linux")]
pub const SOCKET_PATH: &str = "/run/aether-init.sock";

/// 同时服务的连接数上限（PID 1 的线程池必须封顶：服务控制通道被打满 = 系统不可控）。
const MAX_CONNECTIONS: usize = 16;

/// 单连接空闲读超时。
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// 连接数上限 / 读超时 / 行长上限都在这里补齐（2026-10-02 审计 L-3）。
///
/// 为什么不能只靠 0600：0600 挡的是"非 root 连接"，但**任何以 root 运行的程序**
/// 都能开一堆连接把 PID 1 的线程打满 —— 而 PID 1 被拖住意味着整个系统的服务管理
/// 停摆。上限 + 超时是这一层的正确边界，权限位是另一层。
fn accept_loop<S: DuplexStream + Send + 'static>(
    mut listener: impl FnMut() -> Option<S>,
    manager: Arc<Mutex<Manager>>,
) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    loop {
        let Some(stream) = listener() else {
            // None = 监听器结束或一次瞬时错误（EMFILE 等）。这里**不要空转** ——
            // 一个 continue 会把一个核跑满，而 PID 1 不该出现这种情况。
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        };
        if active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
            eprintln!("[aether-init] 连接数已达上限（{MAX_CONNECTIONS}），拒绝新连接");
            // 拒绝也要节流：否则对端狂连会把控制台与日志刷爆（代码审查指出）
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        // 读超时是"连接数上限"的前提：设不上就说明这条连接可能永久占住一个槽位。
        // 因此**失败即关闭**（原来 `.ok()` 静默吞掉，恰好把防线本身吞了）。
        if let Err(e) = stream.set_read_timeout(Some(IDLE_TIMEOUT)) {
            eprintln!("[aether-init] 无法设置读超时（{e}），直接关闭该连接");
            continue;
        }
        let manager = manager.clone();
        let active = active.clone();
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::thread::spawn(move || {
            struct Guard(Arc<std::sync::atomic::AtomicUsize>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            let _g = Guard(active);
            let _ = handle_conn(stream, manager);
        });
    }
}

/// 读取 Unix socket 对端的 uid（`SO_PEERCRED`）。
///
/// 为什么要有（2026-10-02 审计 L-3）：socket 是 0600，**理论上**只有 root 能连；
/// 但"权限位正确"与"真的只接受预期身份"是两件事 —— 一旦权限被改宽、
/// 或者哪天换了挂载点/打包方式，这里仍会拒绝陌生 uid。这就是纵深防御的意义。
/// 读不到对端身份时返回 None，调用方按"拒绝"处理（fail-closed）。
#[cfg(target_os = "linux")]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(cred.uid)
    } else {
        None
    }
}

/// 启动 IPC 服务线程（每连接一线程）。
/// Linux 走 Unix socket（0600，root-only）——"PID 1 服务管理"是高权限边界，
/// 回环 TCP 任何本机用户都能连上停掉关键服务；非 Linux（开发自检）保留 TCP。
#[cfg(target_os = "linux")]
pub fn spawn(manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = std::os::unix::net::UnixListener::bind(SOCKET_PATH)?;
    let _ = std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o600));
    eprintln!("[aether-init] 服务控制通道: {SOCKET_PATH} (0600，上限 {MAX_CONNECTIONS} 连接)");
    std::thread::spawn(move || {
        let mut incoming = listener.incoming();
        accept_loop(
            move || match incoming.next() {
                Some(Ok(s)) => match peer_uid(&s) {
                    Some(uid) if uid == 0 || uid == unsafe { libc::getuid() } => Some(s),
                    Some(uid) => {
                        eprintln!("[aether-init] 拒绝来自 uid={uid} 的服务控制连接");
                        None
                    }
                    None => {
                        eprintln!("[aether-init] 读不到对端身份，拒绝服务控制连接");
                        None
                    }
                },
                _ => None,
            },
            manager,
        );
    });
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn spawn(manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", aether_ipc::INIT_PORT))?;
    eprintln!("[aether-init] 服务控制通道: 127.0.0.1:{}", aether_ipc::INIT_PORT);
    std::thread::spawn(move || {
        let mut incoming = listener.incoming();
        accept_loop(
            move || match incoming.next() {
                Some(Ok(s)) => Some(s),
                _ => None,
            },
            manager,
        );
    });
    Ok(())
}

/// 可复制句柄的流（TcpStream / UnixStream 的公共抽象）。
trait DuplexStream: std::io::Read + std::io::Write + Send {
    fn try_clone_stream(&self) -> std::io::Result<Self>
    where
        Self: Sized;
    /// 设置空闲读超时（两种流都有这个方法，但不是同一个类型）。
    fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()>;
}
impl DuplexStream for std::net::TcpStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        std::net::TcpStream::set_read_timeout(self, d)
    }
}
#[cfg(target_os = "linux")]
impl DuplexStream for std::os::unix::net::UnixStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
    fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        std::os::unix::net::UnixStream::set_read_timeout(self, d)
    }
}

/// 读一行请求，**按行**限长（2026-10-02 审计 I-10 的完整性修复）。
///
/// 原来用 `BufReader::take(MAX_LINE_BYTES)`：`take` 是**连接级累计**上限 ——
/// 正常的长连接累计读满 64KB 就被静默关闭（与"对端正常关闭"不可区分），
/// 而单条超长行不报错、被切成多段逐段 decode 失败。改成按行判：
/// 超长行显式报错断开，正常连接不再有累计上限。
fn read_line_capped<R: BufRead + ?Sized>(reader: &mut R, max: u64) -> std::io::Result<String> {
    let mut line = String::new();
    let n = (&mut *reader).take(max).read_line(&mut line)?;
    if n as u64 >= max && !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("请求行超过 {max} 字节上限"),
        ));
    }
    Ok(line)
}

fn handle_conn<S: DuplexStream>(stream: S, manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    let mut writer = stream.try_clone_stream()?;
    let mut reader = BufReader::new(stream);
    let mut line: String;
    loop {
        line = read_line_capped(&mut reader, MAX_LINE_BYTES)?;
        if line.is_empty() {
            return Ok(());
        }
        let resp = match decode::<Request>(&line) {
            Ok(Request::Ping) => Response::Pong,
            Ok(Request::ServiceControl { unit, action }) => service_control(&manager, &unit, action),
            Ok(Request::SysInfo { .. }) => sys_report(&manager),
            Ok(_) => Response::Error { code: 1, message: "aether-init 仅处理 service_control/ping/sys_info".into() },
            Err(e) => Response::Error { code: 1, message: e.to_string() },
        };
        writer.write_all(encode(&resp).as_bytes())?;
        writer.flush()?;
    }
}

fn service_control(manager: &Arc<Mutex<Manager>>, unit: &str, action: ServiceAction) -> Response {
    let mut m = lock_ok(manager);
    if !m.has(unit) {
        return Response::ServiceAck { unit: unit.into(), ok: false, message: "服务不存在".into() };
    }
    let result = match action {
        ServiceAction::Start | ServiceAction::Restart => {
            if action == ServiceAction::Restart {
                let _ = m.stop(unit);
            }
            m.start(unit).map(|s| format!("{:?} pid={:?}", s.state, s.pid))
        }
        ServiceAction::Stop => m.stop(unit).map(|s| format!("{:?}", s.state)),
        ServiceAction::Status => {
            let s = m.status_all().into_iter().find(|s| s.name == unit);
            match s {
                Some(s) => Ok(format!("{:?} restarts={}", s.state, s.restarts)),
                None => bail_unit(unit),
            }
        }
    };
    match result {
        Ok(message) => Response::ServiceAck { unit: unit.into(), ok: true, message },
        Err(e) => Response::ServiceAck { unit: unit.into(), ok: false, message: e.to_string() },
    }
}

fn bail_unit(unit: &str) -> anyhow::Result<String> {
    anyhow::bail!("服务 {unit} 不存在")
}

fn sys_report(manager: &Arc<Mutex<Manager>>) -> Response {
    let m = lock_ok(manager);
    Response::SysInfo(SysReport {
        cpu_percent: None,
        mem_used_mb: None,
        mem_total_mb: None,
        uptime_secs: None,
        services: m
            .status_all()
            .into_iter()
            .map(|s| ServiceStatus {
                unit: s.name,
                state: format!("{:?}", s.state),
                pid: s.pid,
                restart: s.restart,
                essential: s.essential,
            })
            .collect(),
    })
}

// SvcState 被 sys_report 的格式化引用，保持导出
#[allow(dead_code)]
fn _state_used(s: &SvcState) -> String {
    format!("{s:?}")
}