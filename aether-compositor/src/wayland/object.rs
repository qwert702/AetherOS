//! Wayland 对象表：`id → (接口, 版本, 用户数据)`。
//!
//! 这是 Wayland 的核心数据结构。线协议里**只有整数 id**，所有语义都靠这张表：
//! 收到 `object_id=5, opcode=3` 时，服务端必须知道 id 5 是什么接口、什么版本，
//! 才能确定 opcode 3 的签名并正确解析参数。
//!
//! **id 空间是双向的**：客户端与服务端各持一份表，各自分配自己那一半的 id
//! （见 `CLIENT_ID_START` / `SERVER_ID_START`）。这是 Wayland 不需要
//! "谁先发谁后发"握手就能建立对象的原因。

use std::collections::HashMap;

/// 客户端分配的 id 从 1 开始（0 保留给 `wl_display`）。
pub const CLIENT_ID_START: u32 = 1;
/// 服务端分配的 id 从 0xff000000 起（协议规定的 `WL_SERVER_ID_START`）。
pub const SERVER_ID_START: u32 = 0xff00_0000;

/// `wl_display` 的固定 id。
pub const DISPLAY_ID: u32 = 1;

/// 表里的一个对象。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Object {
    pub interface: &'static str,
    pub version: u32,
    /// 调用方自用的关联数据（服务端用来存 surface 索引、buffer 描述等）。
    /// 用 `u64` 而不是泛型，是为了让这张表能直接嵌进连接状态而不引入类型参数。
    pub data: u64,
}

/// 对象表。
#[derive(Debug, Clone)]
pub struct ObjectTable {
    map: HashMap<u32, Object>,
    /// 下一个可分配的服务端 id
    next_server_id: u32,
    /// 下一个可分配的客户端 id（用于校验客户端是否按序分配 —— 不强制，但便于诊断）
    next_client_id: u32,
}

impl Default for ObjectTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectTable {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_server_id: SERVER_ID_START,
            next_client_id: CLIENT_ID_START,
        }
    }

    /// 插入一个对象。
    ///
    /// 重复 id 会**覆盖**（协议上不该发生；覆盖而不是 panic，是因为恶意客户端
    /// 可以用它来打崩合成器 —— 而合成器 `restart:false`，崩一次桌面就没了）。
    pub fn insert(&mut self, id: u32, interface: &'static str, version: u32, data: u64) {
        self.map.insert(id, Object { interface, version, data });
    }

    pub fn get(&self, id: u32) -> Option<Object> {
        self.map.get(&id).copied()
    }

    pub fn interface_of(&self, id: u32) -> Option<&'static str> {
        self.map.get(&id).map(|o| o.interface)
    }

    pub fn contains(&self, id: u32) -> bool {
        self.map.contains_key(&id)
    }

    pub fn remove(&mut self, id: u32) -> Option<Object> {
        self.map.remove(&id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 分配一个新的**服务端** id（合成器创建的对象，如 `wl_callback`）。
    pub fn alloc_server_id(&mut self) -> u32 {
        let id = self.next_server_id;
        self.next_server_id = self.next_server_id.wrapping_add(1);
        id
    }

    /// 记录客户端用掉的 id，返回它是否**按序**（连续）分配。
    ///
    /// 不按序不是错误（协议允许客户端跳跃），但连续是绝大多数客户端的实际行为 ——
    /// 偏离时值得记一笔日志，便于排查"客户端 id 用光了"这类问题。
    pub fn note_client_id(&mut self, id: u32) -> bool {
        let sequential = id == self.next_client_id;
        self.next_client_id = id.wrapping_add(1);
        sequential
    }

    /// 所有 id 是否都在合法的两半空间里（用于自检）。
    pub fn ids_are_well_formed(&self) -> bool {
        self.map.keys().all(|id| {
            *id == DISPLAY_ID || *id >= CLIENT_ID_START && *id < SERVER_ID_START || *id >= SERVER_ID_START
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut t = ObjectTable::new();
        t.insert(1, "wl_display", 1, 0);
        assert_eq!(t.interface_of(1), Some("wl_display"));
        assert!(t.contains(1));
        assert_eq!(t.len(), 1);

        assert_eq!(t.remove(1).map(|o| o.interface), Some("wl_display"));
        assert!(!t.contains(1));
        assert!(t.is_empty());
    }

    #[test]
    fn unknown_id_returns_none() {
        let t = ObjectTable::new();
        assert!(t.get(42).is_none());
        assert!(t.interface_of(42).is_none());
    }

    /// 恶意客户端用重复 id 不该打崩合成器 —— 覆盖而不是 panic。
    #[test]
    fn duplicate_id_overwrites_instead_of_panicking() {
        let mut t = ObjectTable::new();
        t.insert(5, "wl_surface", 1, 111);
        t.insert(5, "wl_buffer", 1, 222);
        assert_eq!(t.len(), 1);
        assert_eq!(t.interface_of(5), Some("wl_buffer"));
        assert_eq!(t.get(5).unwrap().data, 222);
    }

    #[test]
    fn server_ids_come_from_the_high_half() {
        let mut t = ObjectTable::new();
        let a = t.alloc_server_id();
        let b = t.alloc_server_id();
        assert_eq!(a, SERVER_ID_START);
        assert_eq!(b, SERVER_ID_START + 1);
        assert!(a >= SERVER_ID_START, "服务端 id 必须落在高半区");
    }

    #[test]
    fn client_ids_start_at_one() {
        let mut t = ObjectTable::new();
        assert!(t.note_client_id(CLIENT_ID_START), "第一个客户端 id 应判为按序");
        assert!(t.note_client_id(CLIENT_ID_START + 1));
        // 跳跃不算错，但会被判为"非按序"
        assert!(!t.note_client_id(100));
        assert!(t.note_client_id(101), "跳过一次后应重新连续");
    }

    /// 两个 id 空间不能重叠 —— 重叠意味着客户端和服务端会互相覆盖对象。
    #[test]
    fn id_spaces_do_not_overlap() {
        assert!(CLIENT_ID_START < SERVER_ID_START);
        assert!(DISPLAY_ID < SERVER_ID_START, "wl_display 的 id 属于客户端半区");
    }

    #[test]
    fn ids_are_well_formed_for_normal_use() {
        let mut t = ObjectTable::new();
        t.insert(DISPLAY_ID, "wl_display", 1, 0);
        t.insert(2, "wl_registry", 1, 0);
        let sid = t.alloc_server_id();
        t.insert(sid, "wl_callback", 1, 0);
        assert!(t.ids_are_well_formed());
    }
}
