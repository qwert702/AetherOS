//! aether-init 的服务控制 IPC：接收 ServiceControl 请求，操作服务管理器。

use crate::manager::{Manager, SvcState};
use aether_ipc::{decode, encode, Request, Response, ServiceAction, ServiceStatus, SysReport};
use std::io::{BufRead, BufReader, Read};
#[cfg(not(target_os = "linux"))]
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// 单条请求行长度上限：超长行直接拒绝，不吃内存。
const MAX_LINE_BYTES: u64 = 64 * 1024;

/// 毒锁恢复：持锁线程 panic 不应让 PID 1 的 IPC 通道一起失效。
fn lock_ok(manager: &Mutex<Manager>) -> std::sync::MutexGuard<'_, Manager> {
    manager.lock().unwrap_or_else(|e| e.into_inner())
}

/// 服务控制 socket 路径（Linux）。PID 1 边界，仅 root 可连。
#[cfg(target_os = "linux")]
pub const SOCKET_PATH: &str = "/run/aether-init.sock";

/// 启动 IPC 服务线程（每连接一线程）。
/// Linux 走 Unix socket（0600，root-only）——"PID 1 服务管理"是高权限边界，
/// 回环 TCP 任何本机用户都能连上停掉关键服务；非 Linux（开发自检）保留 TCP。
#[cfg(target_os = "linux")]
pub fn spawn(manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(SOCKET_PATH);
    let listener = std::os::unix::net::UnixListener::bind(SOCKET_PATH)?;
    let _ = std::fs::set_permissions(SOCKET_PATH, std::fs::Permissions::from_mode(0o600));
    eprintln!("[aether-init] 服务控制通道: {SOCKET_PATH} (0600)");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let manager = manager.clone();
            std::thread::spawn(move || {
                let _ = handle_conn(stream, manager);
            });
        }
    });
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn spawn(manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", aether_ipc::INIT_PORT))?;
    eprintln!("[aether-init] 服务控制通道: 127.0.0.1:{}", aether_ipc::INIT_PORT);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let manager = manager.clone();
            std::thread::spawn(move || {
                let _ = handle_conn(stream, manager);
            });
        }
    });
    Ok(())
}

/// 可复制句柄的流（TcpStream / UnixStream 的公共抽象）。
trait DuplexStream: std::io::Read + std::io::Write {
    fn try_clone_stream(&self) -> std::io::Result<Self>
    where
        Self: Sized;
}
impl DuplexStream for std::net::TcpStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}
#[cfg(target_os = "linux")]
impl DuplexStream for std::os::unix::net::UnixStream {
    fn try_clone_stream(&self) -> std::io::Result<Self> {
        self.try_clone()
    }
}

fn handle_conn<S: DuplexStream>(stream: S, manager: Arc<Mutex<Manager>>) -> anyhow::Result<()> {
    let mut writer = stream.try_clone_stream()?;
    // take() 限制单次 read_line 的读取量：超长行不会撑爆内存
    let mut reader = BufReader::new(stream).take(MAX_LINE_BYTES);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
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