//! aether-init 的服务控制 IPC：接收 ServiceControl 请求，操作服务管理器。

use crate::manager::{Manager, SvcState};
use aether_ipc::{decode, encode, Request, Response, ServiceAction, ServiceStatus, SysReport};
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// 启动 IPC 服务线程（每连接一线程）。
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

fn handle_conn(
    stream: std::net::TcpStream,
    manager: Arc<Mutex<Manager>>,
) -> anyhow::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = std::io::BufReader::new(stream);
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
    let mut m = manager.lock().expect("aether-init: manager 毒锁");
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
    let m = manager.lock().expect("aether-init: manager 毒锁");
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
            })
            .collect(),
    })
}

// SvcState 被 sys_report 的格式化引用，保持导出
#[allow(dead_code)]
fn _state_used(s: &SvcState) -> String {
    format!("{s:?}")
}