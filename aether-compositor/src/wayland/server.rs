//! Wayland 服务端：Unix socket 监听与连接生命周期（**Linux-only**）。
//!
//! 这一层**只做字节搬运**：socket 收字节 → `Session::handle` → 事件写回。
//! 协议逻辑全在 `session` 里 —— 那样才能在开发机上测（W1–W4 的全部测试
//! 都不碰 socket）。所以这个文件出问题时，范围是明确的。
//!
//! ## 线程模型
//!
//! 每个连接一个 `Session`（Wayland 的对象 id 只在连接内有意义），
//! 连接线程处理完一批消息就把自己的会话**克隆一份**同步进 `sessions`；
//! 渲染主循环每帧读一遍（`sync_wayland`）。克隆在 spike 阶段的开销可接受 ——
//! 换来的是渲染线程完全不用加锁等 I/O。
//!
//! ## fd 的处理
//!
//! `wl_shm.create_pool` 带一个 fd，经 `SCM_RIGHTS` 传（**不在消息体里**）。
//! 本层用 `recvmsg` 收 ancillary data 是下一步（W5 剩余部分）；在此之前
//! `Session` 里的 pool 停在 `Pending`，协议流程照常走，只是读不到像素。
//!
//! 先不做 fd 的原因：它需要 `libc::recvmsg` + `CMSG` 手工解析，且**必须在真机上验证**
//! （开发机没有 Unix socket 的 fd 传递场景）。把协议与线程模型先做扎实，
//! 再叠这一层，出问题时能分清是哪一层的错。

#[cfg(target_os = "linux")]
mod imp {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    // `imp` 是 server 的**子模块**，所以这里必须写全路径 ——
    // `super::object` 只到 server，够不到 wayland 的兄弟模块
    use crate::wayland::object::DISPLAY_ID;
    use crate::wayland::protocol::Interface;
    use crate::wayland::session::{iface_by_name, Session};
    use crate::wayland::wire::{Arg, Message};

    /// 监听中的 Wayland 服务端。
    pub struct WaylandServer {
        /// 所有连接的会话快照（渲染主循环每帧读）
        sessions: Arc<Mutex<Vec<Session>>>,
        socket_path: PathBuf,
    }

    impl WaylandServer {
        /// 在指定路径上监听（通常是 `$XDG_RUNTIME_DIR/wayland-0`）。
        pub fn bind(path: &Path) -> std::io::Result<Self> {
            // 上次没清干净的 socket 文件会让 bind 直接失败（EADDRINUSE），
            // 而"服务起不来"的表现是"客户端连不上"，很难联想到残留文件
            let _ = std::fs::remove_file(path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let listener = UnixListener::bind(path)?;
            let sessions = Arc::new(Mutex::new(Vec::new()));
            let shared = Arc::clone(&sessions);
            std::thread::spawn(move || accept_loop(listener, shared));
            Ok(Self { sessions, socket_path: path.to_path_buf() })
        }

        /// 当前所有连接的会话（已断开连接的槽位是空会话）。
        pub fn sessions(&self) -> Vec<Session> {
            self.sessions.lock().map(|g| g.clone()).unwrap_or_default()
        }

        /// 有客户端在连吗（没有任何 surface 时可以让渲染省一次同步）。
        pub fn has_clients(&self) -> bool {
            self.sessions
                .lock()
                .map(|g| g.iter().any(|s| !s.mapped.is_empty()))
                .unwrap_or(false)
        }
    }

    impl Drop for WaylandServer {
        fn drop(&mut self) {
            // socket 文件不删会挡住下次启动（见 bind 里的注释）
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    fn accept_loop(listener: UnixListener, sessions: Arc<Mutex<Vec<Session>>>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 占一个槽位；连接线程之后一直写这个索引
            let idx = match sessions.lock() {
                Ok(mut g) => {
                    g.push(Session::new());
                    g.len() - 1
                }
                Err(_) => continue,
            };
            let shared = Arc::clone(&sessions);
            std::thread::spawn(move || handle_conn(stream, shared, idx));
        }
    }

    fn handle_conn(mut stream: UnixStream, sessions: Arc<Mutex<Vec<Session>>>, idx: usize) {
        let mut session = Session::new();
        let mut buf = [0u8; 16384];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) => break, // 客户端正常关闭
                Ok(n) => n,
                Err(_) => break,
            };
            let events = match session.handle(&buf[..n]) {
                Ok(ev) => ev,
                Err(e) => {
                    // 协议错误：按协议回 wl_display.error 再断开。
                    // 不发这一条客户端只会看到"连接莫名断了"，无从排查
                    let err = Message {
                        object_id: DISPLAY_ID,
                        opcode: 0,
                        args: vec![
                            Arg::Object(e.object_id),
                            Arg::Uint(e.code),
                            Arg::Str(e.message.clone()),
                        ],
                    };
                    let _ = stream.write_all(&encode_event(&session, &err));
                    break;
                }
            };
            // 把会话状态同步出去（渲染主循环每帧读它）
            if let Ok(mut g) = sessions.lock() {
                if idx < g.len() {
                    g[idx] = session.clone();
                }
            }
            for ev in &events {
                if stream.write_all(&encode_event(&session, ev)).is_err() {
                    break;
                }
            }
        }
        // 断开：清空该槽 —— `sync_wayland` 会把对应窗口移除
        if let Ok(mut g) = sessions.lock() {
            if idx < g.len() {
                g[idx] = Session::new();
            }
        }
    }

    /// 按对象接口 + opcode 查**事件**签名并编码。
    ///
    /// 查不到签名时用空签名 —— 那意味着对象表里没有这个 id（不该发生），
    /// 编出一条空消息也比 panic 好：合成器 `restart:false`，崩一次桌面就没了。
    fn encode_event(session: &Session, msg: &Message) -> Vec<u8> {
        let sig = session
            .objects
            .interface_of(msg.object_id)
            .and_then(iface_by_name)
            .and_then(|i: &'static Interface| i.event_signature_of(msg.opcode))
            .unwrap_or("");
        msg.encode(sig)
    }
}

// 接线在 W5 剩余部分（渲染主循环启动服务端）之前，这个 re-export 暂时没人用
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use imp::WaylandServer;
