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
//! 本层用 `recvmsg` 收 ancillary data，解析出的 fd 进 `Session` 的 fd 队列，
//! 由 `handle_with_fds` 按 `h` 参数顺序回填；`shm` 模块在 Linux 下 mmap 成
//! pool 数据。收包解析见 `fd` 模块，映射见 `shm::map_pool_fd`。
//!
//! 平台边界：本文件整体在 `cfg(target_os = "linux")` 下；Windows 开发机
//! 不编译此实现，fd 映射走 `shm` 的 stub（明确报错，不 panic）。
//!
//! ## 槽位的生命周期（2026-11-21 审计）
//!
//! 早期版本断开连接只把槽位**覆盖**成空会话、从不回收，`Vec` 长度只增不减：
//! 桌面正常开关 16 次客户端之后，`len == MAX_WAYLAND_CONNS`，之后所有新连接
//! 全被拒 —— 表现是"新窗口再也开不出来"，直到合成器重启。现在槽位是
//! `Option<Session>`，断开置 `None`，新连接优先复用。

#[cfg(target_os = "linux")]
mod imp {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// 同时连接的客户端上限（2026-10-02 审计 L-16）：此前每来一个连接就 push 一个
    /// `Session` 并起一条线程，**没有上限** —— 本机任何进程都能把合成器（root）的
    /// 线程与内存吃光。取 16：一个桌面上同时说话的 Wayland 客户端不会有这么多，
    /// 而这个数字**与 aetherd 的 32 不是同一口径**（aetherd 面向本机所有工具连接，
    /// 合成器只面向桌面客户端，后者更小是刻意的）。
    const MAX_WAYLAND_CONNS: usize = 16;

    /// 跨读待解析缓冲的上限（超过即认定对端在灌垃圾，断开）。
    const MAX_PENDING: usize = 1024 * 1024;

    // `imp` 是 server 的**子模块**，所以这里必须写全路径 ——
    // `super::object` 只到 server，够不到 wayland 的兄弟模块
    use crate::wayland::object::DISPLAY_ID;
    use crate::wayland::protocol::Interface;
    use crate::wayland::session::{iface_by_name, Session};
    use crate::wayland::wire::{Arg, Message};

    /// 监听中的 Wayland 服务端。
    pub struct WaylandServer {
        /// 所有连接的会话快照（渲染主循环每帧读）；`None` = 已断开的空槽
        sessions: Arc<Mutex<Vec<Option<Session>>>>,
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

        /// 当前活着的连接的会话（已断开连接的槽位不在结果里）。
        pub fn sessions(&self) -> Vec<Session> {
            self.sessions
                .lock()
                .map(|g| g.iter().flatten().cloned().collect())
                .unwrap_or_default()
        }

        /// 有客户端在连吗（没有任何 surface 时可以让渲染省一次同步）。
        pub fn has_clients(&self) -> bool {
            self.sessions
                .lock()
                .map(|g| g.iter().flatten().any(|s| !s.mapped.is_empty()))
                .unwrap_or(false)
        }
    }

    impl Drop for WaylandServer {
        fn drop(&mut self) {
            // socket 文件不删会挡住下次启动（见 bind 里的注释）
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    /// 分配一个连接槽：优先复用已断开的空槽，其次追加；满则 `None`。
    ///
    /// 抽成独立函数是为了能在开发机上单测"断开后槽位能被复用"——
    /// 这正是早期版本丢掉的那条不变量。
    fn alloc_slot(slots: &mut Vec<Option<Session>>) -> Option<usize> {
        if let Some(i) = slots.iter().position(|s| s.is_none()) {
            slots[i] = Some(Session::new());
            return Some(i);
        }
        if slots.len() < MAX_WAYLAND_CONNS {
            slots.push(Some(Session::new()));
            return Some(slots.len() - 1);
        }
        None
    }

    fn accept_loop(listener: UnixListener, sessions: Arc<Mutex<Vec<Option<Session>>>>) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 占一个槽位；连接线程之后一直写这个索引
            let idx = match sessions.lock() {
                Ok(mut g) => match alloc_slot(&mut g) {
                    Some(i) => i,
                    None => {
                        drop(g);
                        eprintln!("[wayland] 连接数已达上限（{MAX_WAYLAND_CONNS}），拒绝新连接");
                        // 拒绝也要节流：否则对端狂连会把控制台与日志刷爆。
                        // 注意必须在**放锁之后**睡 —— 持锁睡会卡住渲染线程的
                        // `sessions()` / `has_clients()`（每帧都调）。
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                },
                Err(_) => continue,
            };
            let shared = Arc::clone(&sessions);
            std::thread::spawn(move || handle_conn(stream, shared, idx));
        }
    }

    /// 缓冲区里"完整消息"的总长度（跨读重组）。
    ///
    /// 实现放在 `wire`（纯逻辑，可在开发机上单测）；这里只做转发，
    /// 让 server 这一层继续保持"只做字节搬运"。
    fn complete_prefix_len(buf: &[u8]) -> usize {
        crate::wayland::wire::complete_prefix_len(buf)
    }

    fn handle_conn(mut stream: UnixStream, sessions: Arc<Mutex<Vec<Option<Session>>>>, idx: usize) {
        let mut session = Session::new();
        let mut buf = [0u8; 16384];
        // 跨读残留（不足一条完整消息的尾巴），见 complete_prefix_len 的说明
        let mut pending: Vec<u8> = Vec::new();
        // 本连接收到、尚未被消息消费的 fd（FIFO）。fd 随 `SCM_RIGHTS` 到，
        // 由 `Session::handle_with_fds` 按 `h` 参数顺序取走；连接断开时
        // 队列里残余的 fd 必须关闭 —— `SCM_RIGHTS` 传过来的是独立描述符
        // 副本，不关会泄漏在合成器进程里。
        let mut fd_queue: Vec<i32> = Vec::new();
        loop {
            // 必须用 `recvmsg`（而不是 `read`）：pool fd 在 ancillary data 里，
            // 普通读会把 fd 丢掉，pool 永远停在 Pending。
            let received = match crate::wayland::fd::recvmsg_fds(&stream, &mut buf) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[wayland] recvmsg 失败: {e}");
                    break;
                }
            };
            if received.bytes == 0 {
                break; // 客户端正常关闭
            }
            fd_queue.extend(received.fds);
            pending.extend_from_slice(&buf[..received.bytes]);
            if pending.len() > MAX_PENDING {
                eprintln!("[wayland] 待解析缓冲超过 {MAX_PENDING} 字节，断开连接");
                break;
            }
            let complete = complete_prefix_len(&pending);
            if complete == 0 {
                continue; // 还没凑够一条完整消息，继续读
            }
            let events = match session.handle_with_fds(&pending[..complete], &mut fd_queue) {
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
            pending.drain(..complete);
            // 把会话状态同步出去（渲染主循环每帧读它）
            if let Ok(mut g) = sessions.lock() {
                if idx < g.len() {
                    g[idx] = Some(session.clone());
                }
            }
            for ev in &events {
                if stream.write_all(&encode_event(&session, ev)).is_err() {
                    break;
                }
            }
        }
        // 断开：关掉队列里残余的 fd，再释放槽位（置 `None` 而不是覆盖成空会话，
        // 否则槽位永不回收、连接数上限会在开关 16 次客户端之后把新客户端全挡在外面）。
        // `sync_wayland` 会把对应窗口移除。
        for fd in fd_queue {
            crate::wayland::shm::close_fd(fd);
        }
        if let Ok(mut g) = sessions.lock() {
            if idx < g.len() {
                g[idx] = None;
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn slots_are_reused_after_disconnect() {
            let mut slots: Vec<Option<Session>> = Vec::new();
            for _ in 0..MAX_WAYLAND_CONNS {
                assert!(alloc_slot(&mut slots).is_some());
            }
            assert_eq!(slots.len(), MAX_WAYLAND_CONNS);
            // 满了：拒绝，而不是无限增长
            assert_eq!(alloc_slot(&mut slots), None);

            // 第 3 个客户端断开
            slots[3] = None;
            // 新客户端复用这个槽，而不是被上限拒绝
            assert_eq!(alloc_slot(&mut slots), Some(3));
            assert_eq!(slots.len(), MAX_WAYLAND_CONNS);
        }

        #[test]
        fn allocate_prefers_lowest_free_slot() {
            let mut slots: Vec<Option<Session>> = vec![None; 4];
            assert_eq!(alloc_slot(&mut slots), Some(0));
            slots[1] = None;
            slots[2] = Some(Session::new());
            // 0 已占，1 空 → 取 1
            assert_eq!(alloc_slot(&mut slots), Some(1));
        }
    }
}

// 接线在 W5 剩余部分（渲染主循环启动服务端）之前，这个 re-export 暂时没人用
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use imp::WaylandServer;
