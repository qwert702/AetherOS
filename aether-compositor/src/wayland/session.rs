//! 一个客户端连接的会话状态与请求分发。
//!
//! **刻意与传输分离**：`Session::handle` 只吃字节、吐事件，不碰 socket ——
//! 所以能在开发机上用"手工构造的字节流"完整测试协议流程。socket 集成
//! （`$XDG_RUNTIME_DIR/wayland-0`）是真机阶段的事，只做字节搬运。
//!
//! 每个连接一个 `Session`：对象表是 per-connection 的（Wayland 的 id 只在
//! 一个连接内有意义），surface 状态也是。

use std::collections::HashMap;

use super::object::{ObjectTable, DISPLAY_ID};
use super::protocol::*;
use super::shm::{read_pixels, BufferInfo};
use super::wire::{self, Arg, Message};

/// pool 的数据来源。
///
/// 真机上 `create_pool` 带一个 fd，服务端 `mmap` 它得到字节 —— 那一步是
/// 平台相关的，真机阶段接。在此之前 pool 处于 `Pending`，协议流程照常走，
/// 只是读不到像素（commit 时像素为空，不报错 —— 客户端可能真的还没写完）。
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PoolData {
    Ready(Vec<u8>),
    #[default]
    Pending,
}

/// surface 的状态。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceState {
    /// 被赋予的角色（`xdg_surface.get_toplevel` 之后）
    pub role: Option<Role>,
    pub has_buffer: bool,
    /// 最近一次 attach 的 buffer id（commit 时据此读像素）
    pub attached_buffer: Option<u32>,
    /// 对应的 `xdg_surface` 对象 id（发 configure 事件用）
    pub xdg_surface: Option<u32>,
    /// 最近一次 commit 读出的像素（RGBA8888）。pool 未 Ready 时为 None
    pub pixels: Option<Vec<u8>>,
}

/// 顶层窗口的角色数据。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Role {
    pub title: String,
    pub app_id: String,
    /// 拥有该角色的 `xdg_toplevel` 对象 id
    pub toplevel: Option<u32>,
    /// 客户端 `ack_configure` 过的 serial（未 ack 前 map 是协议错误）
    pub acked_serial: Option<u32>,
}

/// 协议错误 → `wl_display.error` 事件 / 断开连接。
#[derive(Clone, Debug, PartialEq)]
pub struct ProtocolError {
    pub object_id: u32,
    pub code: u32,
    pub message: String,
}

/// 一个客户端连接。
///
/// `Clone` 是给 socket 层用的：连接线程持有自己的会话，每处理完一批消息
/// 就克隆一份同步给渲染主循环（spike 阶段的开销可接受）。
#[derive(Clone)]
pub struct Session {
    pub objects: ObjectTable,
    pub surfaces: HashMap<u32, SurfaceState>,
    /// pool id → 数据（fd 在真机上 mmap 后填 `Ready`）
    pub pools: HashMap<u32, PoolData>,
    /// buffer id → 参数（像素在 pool 里）
    pub buffers: HashMap<u32, BufferInfo>,
    /// global name → 接口名（`wl_registry.bind` 靠它校验）
    pub globals: HashMap<u32, &'static str>,
    next_serial: u32,
    /// 已 map（有 buffer + 有角色 + 已 ack）的 surface，按 map 顺序
    pub mapped: Vec<u32>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// 新连接：对象表里只有 `wl_display`。
    ///
    /// global name **跨客户端一致**（`global_names_are_stable_across_registries`
    /// 钉住这一点）—— name 是"服务端的全局对象编号"，不是"这次 advertise 的序号"。
    pub fn new() -> Self {
        let mut objects = ObjectTable::new();
        objects.insert(DISPLAY_ID, "wl_display", 1, 0);
        let mut globals = HashMap::new();
        for (i, (_, name, _)) in GLOBALS.iter().enumerate() {
            globals.insert((i + 1) as u32, *name);
        }
        Self {
            objects,
            surfaces: HashMap::new(),
            pools: HashMap::new(),
            buffers: HashMap::new(),
            globals,
            next_serial: 1,
            mapped: Vec::new(),
        }
    }

    fn next_serial(&mut self) -> u32 {
        let s = self.next_serial;
        self.next_serial += 1;
        s
    }

    /// 处理一批字节（可能含多条消息），返回要发回客户端的事件。
    pub fn handle(&mut self, buf: &[u8]) -> Result<Vec<Message>, ProtocolError> {
        let mut out = Vec::new();
        let mut rest = buf;
        while !rest.is_empty() {
            let (obj_id, opcode, _) = wire::peek_header(rest).map_err(|e| ProtocolError {
                object_id: obj_id_from(rest),
                code: 0,
                message: e.to_string(),
            })?;
            let iface_name =
                self.objects
                    .interface_of(obj_id)
                    .ok_or_else(|| ProtocolError {
                        object_id: obj_id,
                        code: 0,
                        message: format!("未知对象 id {obj_id}"),
                    })?;
            let Some(iface) = iface_by_name(iface_name) else {
                return Err(ProtocolError {
                    object_id: obj_id,
                    code: 0,
                    message: format!("服务端未实现接口 {iface_name}"),
                });
            };
            let Some(sig) = iface.signature_of(opcode) else {
                return Err(ProtocolError {
                    object_id: obj_id,
                    code: 1,
                    message: format!("{}.{} 非法 opcode {opcode}", iface.name, iface.name),
                });
            };
            let (msg, used) = wire::decode(rest, sig).map_err(|e| ProtocolError {
                object_id: obj_id,
                code: 0,
                message: e.to_string(),
            })?;
            rest = &rest[used..];

            let mut events = self.handle_one(&msg)?;
            out.append(&mut events);
        }
        Ok(out)
    }

    fn handle_one(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        let Some(iface_name) = self.objects.interface_of(msg.object_id) else {
            return Err(ProtocolError {
                object_id: msg.object_id,
                code: 0,
                message: "未知对象".into(),
            });
        };
        match iface_name {
            "wl_display" => self.handle_display(msg),
            "wl_registry" => self.handle_registry(msg),
            "wl_compositor" => self.handle_compositor(msg),
            "wl_surface" => self.handle_surface(msg),
            "wl_shm" => self.handle_shm(msg),
            "wl_shm_pool" => self.handle_shm_pool(msg),
            "xdg_wm_base" => self.handle_xdg_wm_base(msg),
            "xdg_surface" => self.handle_xdg_surface(msg),
            "xdg_toplevel" => self.handle_xdg_toplevel(msg),
            // wl_callback / wl_buffer / wl_region：无需要响应的请求
            _ => Ok(vec![]),
        }
    }

    fn handle_display(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            0 => {
                // sync(new_id) → 对该 callback 发 done，然后声明它可回收
                let cb = arg_newid(msg, 0)?;
                self.objects.insert(cb, "wl_callback", 1, 0);
                let serial = self.next_serial();
                Ok(vec![
                    event(cb, 0, vec![Arg::Uint(serial)]),
                    event(DISPLAY_ID, 1, vec![Arg::Uint(cb)]),
                ])
            }
            1 => {
                // get_registry(new_id) → 逐个 advertise 全局
                let reg = arg_newid(msg, 0)?;
                self.objects.insert(reg, "wl_registry", 1, 0);
                let mut out = Vec::new();
                for (iface, _, ver) in GLOBALS {
                    out.push(event(
                        reg,
                        0,
                        vec![
                            Arg::Uint(self.global_name_of(iface.name)),
                            Arg::Str(iface.name.into()),
                            Arg::Uint(*ver),
                        ],
                    ));
                }
                Ok(out)
            }
            _ => Err(bad_opcode(msg, "wl_display")),
        }
    }

    fn handle_registry(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        debug_assert!(msg.opcode == 0, "wl_registry 只有 bind");
        let name = arg_uint(msg, 0)?;
        let iface_str = arg_str(msg, 1)?;
        let ver = arg_uint(msg, 2)?;
        let new_id = arg_newid(msg, 3)?;

        let iface = iface_by_name(&iface_str).ok_or_else(|| ProtocolError {
            object_id: msg.object_id,
            code: 0,
            message: format!("bind 了未实现的全局 {iface_str}"),
        })?;
        if self.globals.get(&name).copied() != Some(iface.name) {
            return Err(ProtocolError {
                object_id: msg.object_id,
                code: 0,
                message: format!("global name {name} 与接口 {iface_str} 不匹配"),
            });
        }
        // 客户端要求的版本不能超过我们 advertise 的版本 —— 夹到交集，
        // 否则客户端会按它自己以为的版本发请求，服务端解析不了
        self.objects
            .insert(new_id, iface.name, ver.min(iface.version), 0);
        Ok(vec![])
    }

    fn handle_compositor(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            0 => {
                let id = arg_newid(msg, 0)?;
                self.objects.insert(id, "wl_surface", 4, 0);
                self.surfaces.insert(id, SurfaceState::default());
                Ok(vec![])
            }
            1 => {
                let id = arg_newid(msg, 0)?;
                self.objects.insert(id, "wl_region", 1, 0);
                Ok(vec![])
            }
            _ => Err(bad_opcode(msg, "wl_compositor")),
        }
    }

    fn handle_surface(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        let surface = msg.object_id;
        match msg.opcode {
            0 | 2 | 7 | 8 | 9 => Ok(vec![]),
            1 => {
                // attach：buffer 为 0 表示"解除"，非 0 记下 id（commit 读像素用）
                let buf = match msg.args.first() {
                    Some(Arg::Object(b)) => *b,
                    _ => 0,
                };
                if let Some(s) = self.surfaces.get_mut(&surface) {
                    s.has_buffer = buf != 0;
                    s.attached_buffer = if buf != 0 { Some(buf) } else { None };
                }
                Ok(vec![])
            }
            3 => {
                // frame(new_id)：spike 阶段立即回 done（真实实现要等 vblank）
                let cb = arg_newid(msg, 0)?;
                self.objects.insert(cb, "wl_callback", 1, 0);
                Ok(vec![event(cb, 0, vec![Arg::Uint(self.next_serial())])])
            }
            6 => self.commit(surface),
            _ => Err(bad_opcode(msg, "wl_surface")),
        }
    }

    /// commit 是 surface 生命周期的关键节点。
    ///
    /// Wayland 是**显式状态机**：attach 只是暂存，commit 才生效。map 的条件是
    /// 「有 buffer + 有角色 + 角色的 configure 已被 ack」—— 少一个都不能上屏，
    /// 否则客户端还没准备好就被合成器当成"已可见"。
    fn commit(&mut self, surface: u32) -> Result<Vec<Message>, ProtocolError> {
        let Some(st) = self.surfaces.get(&surface).cloned() else {
            return Err(ProtocolError {
                object_id: surface,
                code: 0,
                message: "commit 了不存在的 surface".into(),
            });
        };
        if !st.has_buffer {
            // 没内容就不 map（unmap 的情形），不是错误
            self.mapped.retain(|m| *m != surface);
            return Ok(vec![]);
        }
        // 读像素：buffer → pool → 格式转换。
        //
        // pool 未 Ready（真机 mmap 未接 / 客户端还没写完）或读失败都**不阻断协议
        // 流程** —— 像素为 None 只表示"这次 commit 没有可显示的内容"。
        // 把读像素失败当成协议错误会误伤合法的时序（客户端先 commit 后写像素
        // 是常见模式，帧回调之后才画）。
        let pixels = st
            .attached_buffer
            .and_then(|bid| self.buffers.get(&bid).cloned())
            .and_then(|info| match self.pools.get(&info.pool_id) {
                Some(PoolData::Ready(data)) => read_pixels(data, &info).ok(),
                _ => None,
            });
        if let Some(s) = self.surfaces.get_mut(&surface) {
            s.pixels = pixels;
        }
        let Some(role) = &st.role else {
            return Ok(vec![]); // 没角色的 surface（光标之类）map 与否 spike 不关心
        };
        if role.acked_serial.is_none() {
            return Err(ProtocolError {
                object_id: surface,
                code: 0,
                message: "xdg_surface 未 ack configure 就 commit（协议要求先等配置）".into(),
            });
        }
        if !self.mapped.contains(&surface) {
            self.mapped.push(surface);
        }
        let mut out = Vec::new();
        let serial = self.next_serial();
        if let Some(xdg) = st.xdg_surface {
            out.push(event(xdg, 0, vec![Arg::Uint(serial)]));
        }
        if let Some(tl) = role.toplevel {
            // toplevel.configure(0, 0, [])：0 表示"由客户端决定尺寸"，
            // 这是合成器对新窗口的礼貌做法
            out.push(event(
                tl,
                0,
                vec![Arg::Int(0), Arg::Int(0), Arg::Array(vec![])],
            ));
        }
        Ok(out)
    }

    fn handle_shm(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        debug_assert!(msg.opcode == 0);
        let id = arg_newid(msg, 0)?;
        // fd 在签名 "nhi" 里占**索引 1**（wire 解码时保留槽位、值来自 SCM_RIGHTS），
        // 所以 size 在索引 2 —— 按索引 1 取会拿到 Fd
        let size = arg_int(msg, 2)?;
        if size < 0 {
            return Err(ProtocolError {
                object_id: msg.object_id,
                code: 0,
                message: format!("create_pool 的 size 为负（{size}）"),
            });
        }
        // fd（Arg::Fd）在 spike 阶段不 mmap：数据来源是平台相关的。
        // pool 先记为 Pending；真机 mmap 后（或测试里）用 `attach_pool_data` 填。
        self.objects.insert(id, "wl_shm_pool", 1, 0);
        self.pools.insert(id, PoolData::Pending);
        Ok(vec![])
    }

    /// 给 pool 挂数据（真机：mmap 出的字节；测试：直接注入）。
    pub fn attach_pool_data(&mut self, pool_id: u32, data: Vec<u8>) {
        self.pools.insert(pool_id, PoolData::Ready(data));
    }

    fn handle_shm_pool(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            0 => {
                let id = arg_newid(msg, 0)?;
                let offset = arg_int(msg, 1)? as usize;
                let width = arg_int(msg, 2)?;
                let height = arg_int(msg, 3)?;
                let stride = arg_int(msg, 4)?;
                let format = arg_uint(msg, 5)?;
                self.objects.insert(id, "wl_buffer", 1, 0);
                self.buffers.insert(
                    id,
                    BufferInfo {
                        pool_id: msg.object_id,
                        offset,
                        width,
                        height,
                        stride,
                        format,
                    },
                );
                Ok(vec![])
            }
            1 => {
                // destroy：buffer 参数一并清掉，别留悬空引用
                self.buffers.remove(&msg.object_id);
                Ok(vec![])
            }
            2 => Ok(vec![]), // resize：真机上要重新 mmap，spike 忽略
            _ => Err(bad_opcode(msg, "wl_shm_pool")),
        }
    }

    fn handle_xdg_wm_base(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            2 => {
                let id = arg_newid(msg, 0)?;
                let surface = arg_object(msg, 1)?;
                if !self.surfaces.contains_key(&surface) {
                    return Err(ProtocolError {
                        object_id: msg.object_id,
                        code: 0,
                        message: "get_xdg_surface 的参数不是 wl_surface".into(),
                    });
                }
                self.objects.insert(id, "xdg_surface", 1, 0);
                if let Some(s) = self.surfaces.get_mut(&surface) {
                    s.xdg_surface = Some(id);
                }
                Ok(vec![])
            }
            0 | 1 => Ok(vec![]),
            _ => Err(bad_opcode(msg, "xdg_wm_base")),
        }
    }

    fn handle_xdg_surface(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            1 => {
                let id = arg_newid(msg, 0)?;
                self.objects.insert(id, "xdg_toplevel", 1, 0);
                // 找到拥有这个 xdg_surface 的 wl_surface，给它赋予角色
                for st in self.surfaces.values_mut() {
                    if st.xdg_surface == Some(msg.object_id) {
                        st.role = Some(Role {
                            toplevel: Some(id),
                            ..Role::default()
                        });
                        break;
                    }
                }
                Ok(vec![])
            }
            4 => {
                // ack_configure(serial)
                let serial = arg_uint(msg, 0)?;
                for st in self.surfaces.values_mut() {
                    if st.xdg_surface == Some(msg.object_id) {
                        if let Some(role) = &mut st.role {
                            role.acked_serial = Some(serial);
                        }
                        break;
                    }
                }
                Ok(vec![])
            }
            0 | 2 | 3 => Ok(vec![]),
            _ => Err(bad_opcode(msg, "xdg_surface")),
        }
    }

    fn handle_xdg_toplevel(&mut self, msg: &Message) -> Result<Vec<Message>, ProtocolError> {
        match msg.opcode {
            2 => {
                let title = arg_str(msg, 0)?;
                for st in self.surfaces.values_mut() {
                    if let Some(role) = &mut st.role {
                        role.title = title.clone();
                    }
                }
                Ok(vec![])
            }
            3 => {
                let app_id = arg_str(msg, 0)?;
                for st in self.surfaces.values_mut() {
                    if let Some(role) = &mut st.role {
                        role.app_id = app_id.clone();
                    }
                }
                Ok(vec![])
            }
            0 | 1 | 4..=13 => Ok(vec![]),
            _ => Err(bad_opcode(msg, "xdg_toplevel")),
        }
    }

    fn global_name_of(&self, iface: &str) -> u32 {
        self.globals
            .iter()
            .find(|(_, n)| **n == iface)
            .map(|(k, _)| *k)
            .unwrap_or(0)
    }
}

// ---- 事件构造与参数读取 ----

fn event(object_id: u32, opcode: u16, args: Vec<Arg>) -> Message {
    Message { object_id, opcode, args }
}

fn bad_opcode(msg: &Message, iface: &str) -> ProtocolError {
    ProtocolError {
        object_id: msg.object_id,
        code: 0,
        message: format!("{iface} 非法 opcode {}", msg.opcode),
    }
}

macro_rules! arg_getter {
    ($name:ident, $variant:ident, $ty:ty, $desc:expr) => {
        fn $name(msg: &Message, i: usize) -> Result<$ty, ProtocolError> {
            match msg.args.get(i) {
                Some(Arg::$variant(v)) => Ok(*v),
                other => Err(ProtocolError {
                    object_id: msg.object_id,
                    code: 0,
                    message: format!("参数 {i} 应为 {}，实得 {:?}", $desc, other),
                }),
            }
        }
    };
}

arg_getter!(arg_uint, Uint, u32, "uint");
arg_getter!(arg_int, Int, i32, "int");
arg_getter!(arg_object, Object, u32, "object");
arg_getter!(arg_newid, NewId, u32, "new_id");

fn arg_str(msg: &Message, i: usize) -> Result<String, ProtocolError> {
    match msg.args.get(i) {
        Some(Arg::Str(v)) => Ok(v.clone()),
        other => Err(ProtocolError {
            object_id: msg.object_id,
            code: 0,
            message: format!("参数 {i} 应为 string，实得 {:?}", other),
        }),
    }
}

/// 按接口名查定义。socket 层编码事件时也要用它（查事件签名）。
pub(crate) fn iface_by_name(name: &str) -> Option<&'static Interface> {
    Some(match name {
        "wl_display" => &WL_DISPLAY,
        "wl_registry" => &WL_REGISTRY,
        "wl_callback" => &WL_CALLBACK,
        "wl_compositor" => &WL_COMPOSITOR,
        "wl_surface" => &WL_SURFACE,
        "wl_shm" => &WL_SHM,
        "wl_shm_pool" => &WL_SHM_POOL,
        "wl_buffer" => &WL_BUFFER,
        "wl_region" => &WL_REGION,
        "xdg_wm_base" => &XDG_WM_BASE,
        "xdg_surface" => &XDG_SURFACE,
        "xdg_toplevel" => &XDG_TOPLEVEL,
        _ => return None,
    })
}

fn obj_id_from(buf: &[u8]) -> u32 {
    if buf.len() >= 4 {
        u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已知对象 id 的约定（与 full_map_flow 测试配套）：
    /// 1=wl_display 2=registry 3=compositor 4=shm 5=xdg_wm_base
    /// 6=surface 7=buffer 8=xdg_surface 9=xdg_toplevel
    const REGISTRY: u32 = 2;
    const COMPOSITOR: u32 = 3;
    const SHM: u32 = 4;
    const WM_BASE: u32 = 5;
    const SURFACE: u32 = 6;
    const BUFFER: u32 = 7;
    const XDG_SURFACE: u32 = 8;
    const TOPLEVEL: u32 = 9;

    fn setup_registry() -> Session {
        let mut s = Session::new();
        s.handle(&Message { object_id: DISPLAY_ID, opcode: 1, args: vec![Arg::NewId(REGISTRY)] }
            .encode("n"))
        .unwrap();
        s
    }

    #[test]
    fn sync_roundtrips_with_delete_id() {
        let mut s = Session::new();
        let out = s
            .handle(&Message { object_id: DISPLAY_ID, opcode: 0, args: vec![Arg::NewId(2)] }
                .encode("n"))
            .expect("sync 不该报错");
        assert_eq!(out.len(), 2, "应回 done + delete_id");
        assert_eq!(out[0].opcode, 0, "第一个事件是 wl_callback.done");
        assert_eq!(out[1].opcode, 1, "第二个事件是 wl_display.delete_id");
        assert_eq!(out[1].args[0], Arg::Uint(2), "delete_id 带回那个 callback 的 id");
    }

    #[test]
    fn registry_advertises_all_globals() {
        // 这个 session 只用来确认 `setup_registry` 能把会话建起来（它的回包在
        // 上一个测试里断言），下面才重新走一遍拿事件
        let s = setup_registry();
        let mut s2 = Session::new();
        let out = s2
            .handle(&Message { object_id: DISPLAY_ID, opcode: 1, args: vec![Arg::NewId(REGISTRY)] }
                .encode("n"))
            .unwrap();
        assert_eq!(out.len(), GLOBALS.len(), "每个全局一条 global 事件");
        let names: Vec<&str> = out
            .iter()
            .filter_map(|m| match m.args.get(1) {
                Some(Arg::Str(n)) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["wl_compositor", "wl_shm", "xdg_wm_base"]);
        let _ = s;
    }

    #[test]
    fn bind_records_object_and_clamps_version() {
        let mut s = setup_registry();
        // 客户端要版本 99 —— 必须被夹到 advertise 的版本
        s.handle(&Message {
            object_id: REGISTRY,
            opcode: 0,
            args: vec![
                Arg::Uint(1),
                Arg::Str("wl_compositor".into()),
                Arg::Uint(99),
                Arg::NewId(COMPOSITOR),
            ],
        }
        .encode("usun"))
        .expect("bind 不该报错");
        assert_eq!(s.objects.interface_of(COMPOSITOR), Some("wl_compositor"));
        assert_eq!(
            s.objects.get(COMPOSITOR).unwrap().version,
            4,
            "客户端不能单方面升版本"
        );
    }

    #[test]
    fn bind_rejects_unknown_global() {
        let mut s = setup_registry();
        // spike 不做输入：wl_seat 不在 GLOBALS 里
        let err = s
            .handle(&Message {
                object_id: REGISTRY,
                opcode: 0,
                args: vec![Arg::Uint(1), Arg::Str("wl_seat".into()), Arg::Uint(1), Arg::NewId(3)],
            }
            .encode("usun"))
            .expect_err("未实现的全局应报错");
        assert!(err.message.contains("未实现"), "实得: {}", err.message);
    }

    #[test]
    fn bind_rejects_mismatched_name() {
        let mut s = setup_registry();
        // name=1 是 wl_compositor，却报 wl_shm —— 一致性校验
        let err = s
            .handle(&Message {
                object_id: REGISTRY,
                opcode: 0,
                args: vec![Arg::Uint(1), Arg::Str("wl_shm".into()), Arg::Uint(1), Arg::NewId(3)],
            }
            .encode("usun"))
            .expect_err("name 与接口不匹配应报错");
        assert!(err.message.contains("不匹配"), "实得: {}", err.message);
    }

    #[test]
    fn unknown_object_is_protocol_error() {
        let mut s = Session::new();
        let err = s
            .handle(&Message { object_id: 999, opcode: 0, args: vec![] }.encode(""))
            .expect_err("未知对象应报错");
        assert!(err.message.contains("未知对象"), "实得: {}", err.message);
    }

    /// ★ spike 的核心场景：纯逻辑下走完「registry → bind ×3 → surface →
    /// buffer → xdg → ack → map」全流程，不碰 socket。
    #[test]
    fn full_map_flow_without_socket() {
        let mut s = setup_registry();
        let bind = |s: &mut Session, name: u32, iface: &str, new_id: u32| {
            s.handle(&Message {
                object_id: REGISTRY,
                opcode: 0,
                args: vec![Arg::Uint(name), Arg::Str(iface.into()), Arg::Uint(1), Arg::NewId(new_id)],
            }
            .encode("usun"))
            .unwrap();
        };
        bind(&mut s, 1, "wl_compositor", COMPOSITOR);
        bind(&mut s, 2, "wl_shm", SHM);
        bind(&mut s, 3, "xdg_wm_base", WM_BASE);

        // create_surface
        s.handle(&Message { object_id: COMPOSITOR, opcode: 0, args: vec![Arg::NewId(SURFACE)] }
            .encode("n"))
        .unwrap();
        // create_pool → create_buffer（签名含 new_id："nhi" / "niiiiu"）
        s.handle(&Message { object_id: SHM, opcode: 0, args: vec![Arg::NewId(BUFFER), Arg::Fd(-1), Arg::Int(4096)] }
            .encode("nhi"))
        .unwrap();
        s.handle(&Message {
            object_id: BUFFER,
            opcode: 0,
            args: vec![Arg::NewId(BUFFER), Arg::Int(0), Arg::Int(64), Arg::Int(64), Arg::Int(256), Arg::Uint(0)],
        }
        .encode("niiiiu"))
        .unwrap();
        // attach + get_xdg_surface + get_toplevel + set_title
        s.handle(&Message {
            object_id: SURFACE,
            opcode: 1,
            args: vec![Arg::Object(BUFFER), Arg::Int(0), Arg::Int(0)],
        }
        .encode("oii"))
        .unwrap();
        s.handle(&Message {
            object_id: WM_BASE,
            opcode: 2,
            args: vec![Arg::NewId(XDG_SURFACE), Arg::Object(SURFACE)],
        }
        .encode("no"))
        .unwrap();
        s.handle(&Message { object_id: XDG_SURFACE, opcode: 1, args: vec![Arg::NewId(TOPLEVEL)] }
            .encode("n"))
        .unwrap();
        s.handle(&Message { object_id: TOPLEVEL, opcode: 2, args: vec![Arg::Str("测试窗口".into())] }
            .encode("s"))
        .unwrap();

        // ★ 未 ack 就 commit：必须报协议错误，而不是直接 map
        let err = s
            .handle(&Message { object_id: SURFACE, opcode: 6, args: vec![] }.encode(""))
            .expect_err("未 ack 就 commit 应报错");
        assert!(err.message.contains("ack"), "实得: {}", err.message);

        // ack 后再 commit → map + 发 configure 事件
        // ack 本身不产生需要断言的回包（`.unwrap()` 已经保证它没报错）
        s
            .handle(&Message { object_id: XDG_SURFACE, opcode: 4, args: vec![Arg::Uint(1)] }.encode("u"))
            .unwrap();
        let out = s
            .handle(&Message { object_id: SURFACE, opcode: 6, args: vec![] }.encode(""))
            .unwrap();
        assert!(
            s.mapped.contains(&SURFACE),
            "commit 后 surface 应已 map，事件: {out:?}"
        );
        // 标题被记下了（中文也要完好）
        assert_eq!(s.surfaces[&SURFACE].role.as_ref().unwrap().title, "测试窗口");
        // 事件里有 toplevel.configure（发给 xdg_toplevel 对象）
        assert!(
            out.iter().any(|m| m.object_id == TOPLEVEL && m.opcode == 0),
            "应有 toplevel.configure: {out:?}"
        );
    }

    /// ★ W3 核心场景：pool 数据就位后，commit 能读出像素（客户端画红+蓝，
    /// 合成器读回 RGBA —— 字节序错了这里立刻现形）。
    #[test]
    fn commit_reads_pixels_from_pool() {
        let mut s = setup_registry();
        s.handle(&Message {
            object_id: REGISTRY,
            opcode: 0,
            args: vec![Arg::Uint(2), Arg::Str("wl_shm".into()), Arg::Uint(1), Arg::NewId(SHM)],
        }
        .encode("usun"))
        .unwrap();
        // create_pool：fd 占索引 1（值 -1 占位），size 在索引 2（签名 "nhi"）
        const POOL: u32 = 10;
        s.handle(&Message {
            object_id: SHM,
            opcode: 0,
            args: vec![Arg::NewId(POOL), Arg::Fd(-1), Arg::Int(64)],
        }
        .encode("nhi"))
        .unwrap();
        // 客户端往 pool 里画两个 XRGB 像素：纯红、纯蓝
        // 小端字节序：0x00FF0000 → [00,00,FF,00]，0x000000FF → [FF,00,00,00]
        s.attach_pool_data(POOL, vec![0, 0, 0xff, 0, 0xff, 0, 0, 0]);
        // create_buffer：2×1，XRGB8888（签名 "niiiiu"）
        s.handle(&Message {
            object_id: POOL,
            opcode: 0,
            args: vec![
                Arg::NewId(BUFFER),
                Arg::Int(0),
                Arg::Int(2),
                Arg::Int(1),
                Arg::Int(8),
                Arg::Uint(1), // XRGB8888
            ],
        }
        .encode("niiiiu"))
        .unwrap();
        // surface + attach + commit
        s.objects.insert(COMPOSITOR, "wl_compositor", 4, 0);
        s.handle(&Message { object_id: COMPOSITOR, opcode: 0, args: vec![Arg::NewId(SURFACE)] }
            .encode("n"))
        .unwrap();
        s.handle(&Message {
            object_id: SURFACE,
            opcode: 1,
            args: vec![Arg::Object(BUFFER), Arg::Int(0), Arg::Int(0)],
        }
        .encode("oii"))
        .unwrap();
        s.handle(&Message { object_id: SURFACE, opcode: 6, args: vec![] }.encode("")).unwrap();

        let pixels = s.surfaces[&SURFACE].pixels.as_deref().expect("像素应已读出");
        assert_eq!(pixels, &[0xff, 0, 0, 255, 0, 0, 0xff, 255], "XRGB → RGBA");
    }

    #[test]
    fn commit_without_buffer_unmaps() {
        let mut s = setup_registry();
        s.objects.insert(COMPOSITOR, "wl_compositor", 4, 0);
        s.handle(&Message { object_id: COMPOSITOR, opcode: 0, args: vec![Arg::NewId(SURFACE)] }
            .encode("n"))
        .unwrap();
        s.surfaces.get_mut(&SURFACE).unwrap().role = Some(Role {
            toplevel: Some(TOPLEVEL),
            acked_serial: Some(1),
            ..Default::default()
        });
        s.mapped.push(SURFACE);
        // attach(0) = 解除 buffer
        s.handle(&Message {
            object_id: SURFACE,
            opcode: 1,
            args: vec![Arg::Object(0), Arg::Int(0), Arg::Int(0)],
        }
        .encode("oii"))
        .unwrap();
        s.handle(&Message { object_id: SURFACE, opcode: 6, args: vec![] }.encode("")).unwrap();
        assert!(!s.mapped.contains(&SURFACE), "解除 buffer 后应 unmap");
    }

    #[test]
    fn get_xdg_surface_rejects_non_surface() {
        let mut s = Session::new();
        s.objects.insert(WM_BASE, "xdg_wm_base", 1, 0);
        let err = s
            .handle(&Message {
                object_id: WM_BASE,
                opcode: 2,
                args: vec![Arg::NewId(4), Arg::Object(99)],
            }
            .encode("no"))
            .expect_err("非 surface 应报错");
        assert!(err.message.contains("wl_surface"), "实得: {}", err.message);
    }

    /// global name 是**服务端的全局编号**，必须跨客户端一致 ——
    /// 不然第二个客户端 bind 的 name 在语义上对不上。
    #[test]
    fn global_names_are_stable_across_sessions() {
        let name_of = |s: &Session, iface: &str| {
            s.globals.iter().find(|(_, n)| **n == iface).map(|(k, _)| *k)
        };
        let a = Session::new();
        let b = Session::new();
        assert_eq!(name_of(&a, "wl_shm"), name_of(&b, "wl_shm"));
        assert_eq!(name_of(&a, "xdg_wm_base"), name_of(&b, "xdg_wm_base"));
    }

    #[test]
    fn partial_message_is_not_consumed_silently() {
        // 截断的消息必须报错，而不是"静默忽略" —— 静默丢消息会让客户端
        // 卡在等一个永远不会来的回复上
        let mut s = Session::new();
        let full = Message { object_id: DISPLAY_ID, opcode: 0, args: vec![Arg::NewId(2)] }
            .encode("n");
        assert!(s.handle(&full[..full.len() - 2]).is_err());
    }
}
