//! Wayland 协议定义：接口、方法、请求签名（纯数据）。
//!
//! 签名是**线协议的参数布局**（类型字符见 `wire` 模块）：
//! `i`=int `u`=uint `f`=fixed `s`=string `o`=object `n`=new_id `a`=array `h`=fd
//!
//! **这些定义必须与官方 wayland.xml 一致** —— 它们是线协议的契约：
//! 服务端按这里的签名解析客户端消息，客户端按它构造消息。写错一个字符，
//! 两端就会互相读出垃圾，而且往往只是"看起来能跑、值是错的"。
//! 所以每个接口的 `requests` 数组都配了 opcode 索引测试 ——
//! opcode 是协议定义的常量，不能按"加了一个方法"顺手推。

/// 一个接口的请求表。数组索引即 opcode。
pub struct Interface {
    pub name: &'static str,
    pub version: u32,
    /// (请求名, 签名)。**索引即 opcode**。
    pub requests: &'static [(&'static str, &'static str)],
}

impl Interface {
    /// opcode → 签名。
    pub fn signature_of(&self, opcode: u16) -> Option<&'static str> {
        self.requests.get(opcode as usize).map(|(_, sig)| *sig)
    }

    pub fn request_of(&self, opcode: u16) -> Option<&'static str> {
        self.requests.get(opcode as usize).map(|(name, _)| *name)
    }
}

// ---- 核心协议（wayland.xml）----

pub const WL_DISPLAY: Interface = Interface {
    name: "wl_display",
    version: 1,
    requests: &[("sync", "n"), ("get_registry", "n")],
};

pub const WL_REGISTRY: Interface = Interface {
    name: "wl_registry",
    version: 1,
    requests: &[("bind", "usun")],
};

/// `wl_callback` 只有事件（done），没有请求。
pub const WL_CALLBACK: Interface = Interface {
    name: "wl_callback",
    version: 1,
    requests: &[],
};

pub const WL_COMPOSITOR: Interface = Interface {
    name: "wl_compositor",
    version: 4,
    requests: &[("create_surface", "n"), ("create_region", "n")],
};

pub const WL_SURFACE: Interface = Interface {
    name: "wl_surface",
    version: 4,
    requests: &[
        ("destroy", ""),
        ("attach", "oii"),
        ("damage", "iiii"),
        ("frame", "n"),
        ("set_opaque_region", "o"),
        ("set_input_region", "o"),
        ("commit", ""),
        ("set_buffer_transform", "i"),
        ("set_buffer_scale", "i"),
        ("damage_buffer", "iiii"),
    ],
};

pub const WL_SHM: Interface = Interface {
    name: "wl_shm",
    version: 1,
    // new_id **也在签名里**（"nhi"）—— spike 初版写成 "hi"，create_pool 时
    // 参数个数对不上直接触发 encode 的 debug_assert。这就是协议定义要逐条
    // 对照 wayland.xml 的原因
    requests: &[("create_pool", "nhi")],
};

pub const WL_SHM_POOL: Interface = Interface {
    name: "wl_shm_pool",
    version: 1,
    requests: &[("create_buffer", "niiiiu"), ("destroy", ""), ("resize", "i")],
};

pub const WL_BUFFER: Interface = Interface {
    name: "wl_buffer",
    version: 1,
    requests: &[("destroy", "")],
};

pub const WL_REGION: Interface = Interface {
    name: "wl_region",
    version: 1,
    requests: &[("destroy", ""), ("add", "iiii"), ("subtract", "iiii")],
};

// ---- xdg-shell（稳定版，v1）----

pub const XDG_WM_BASE: Interface = Interface {
    name: "xdg_wm_base",
    version: 1,
    requests: &[
        ("destroy", ""),
        ("create_positioner", "n"),
        ("get_xdg_surface", "no"),
    ],
};

pub const XDG_SURFACE: Interface = Interface {
    name: "xdg_surface",
    version: 1,
    requests: &[
        ("destroy", ""),
        ("get_toplevel", "n"),
        ("get_popup", "nnoo"),
        ("set_window_geometry", "iiii"),
        ("ack_configure", "u"),
    ],
};

pub const XDG_TOPLEVEL: Interface = Interface {
    name: "xdg_toplevel",
    version: 1,
    requests: &[
        ("destroy", ""),
        ("set_parent", "o"),
        ("set_title", "s"),
        ("set_app_id", "s"),
        ("show_window_menu", "oii"),
        ("move", "ou"),
        ("resize", "ouu"),
        ("set_max_size", "ii"),
        ("set_min_size", "ii"),
        ("set_maximized", ""),
        ("unset_maximized", ""),
        ("set_fullscreen", "o"),
        ("unset_fullscreen", ""),
        ("set_minimized", ""),
    ],
};

/// 服务端要 advertise 给客户端的全局对象：`(接口定义, 全局名, 版本)`。
///
/// **没有 `wl_seat` / `wl_output`** —— spike 不做输入与多显示器（见 mod 头注释）。
/// 客户端（如 wlr-randr）若因此报缺接口，那是范围划定，不是 bug。
pub const GLOBALS: &[(&Interface, &'static str, u32)] = &[
    (&WL_COMPOSITOR, "wl_compositor", 4),
    (&WL_SHM, "wl_shm", 1),
    (&XDG_WM_BASE, "xdg_wm_base", 1),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// opcode 是协议常量。**索引即 opcode** 这个约定本身要测 ——
    /// 如果有人在数组中间插了一个方法，后面所有的 opcode 全部偏移，
    /// 客户端会调用到完全不同的方法上（而且没有报错）。
    #[test]
    fn opcodes_are_stable() {
        assert_eq!(WL_DISPLAY.request_of(0), Some("sync"));
        assert_eq!(WL_DISPLAY.request_of(1), Some("get_registry"));
        assert_eq!(WL_DISPLAY.request_of(2), None, "wl_display 只有 2 个请求");

        assert_eq!(WL_SURFACE.request_of(6), Some("commit"), "commit 必须是 opcode 6");
        assert_eq!(WL_SURFACE.request_of(1), Some("attach"));
        assert_eq!(WL_SHM.request_of(0), Some("create_pool"));
        assert_eq!(XDG_SURFACE.request_of(1), Some("get_toplevel"));
        assert_eq!(XDG_TOPLEVEL.request_of(2), Some("set_title"));
    }

    #[test]
    fn signatures_match_official_protocol() {
        // 与 wayland.xml 逐条对照。这里钉住最容易记错的几个：
        assert_eq!(WL_DISPLAY.signature_of(0), Some("n"), "sync 的参数是 new_id");
        assert_eq!(WL_REGISTRY.signature_of(0), Some("usun"), "bind: name/interface/version/new_id");
        // new_id 在签名里占一个位置 —— 初版漏了它（写成 "hi"/"iiiiu"）
        assert_eq!(WL_SHM.signature_of(0), Some("nhi"), "create_pool: new_id + fd + size");
        assert_eq!(
            WL_SHM_POOL.signature_of(0),
            Some("niiiiu"),
            "create_buffer: new_id + offset/w/h/stride/format"
        );
        assert_eq!(XDG_WM_BASE.signature_of(2), Some("no"), "get_xdg_surface: new_id + surface");
        assert_eq!(XDG_SURFACE.signature_of(4), Some("u"), "ack_configure: serial");
        assert_eq!(XDG_TOPLEVEL.signature_of(2), Some("s"), "set_title: string");
    }

    /// 签名里不能有未知类型字符 —— 写错的话 `decode` 会直接拒绝。
    #[test]
    fn all_signatures_use_known_type_chars() {
        let all = [
            &WL_DISPLAY, &WL_REGISTRY, &WL_CALLBACK, &WL_COMPOSITOR, &WL_SURFACE,
            &WL_SHM, &WL_SHM_POOL, &WL_BUFFER, &WL_REGION,
            &XDG_WM_BASE, &XDG_SURFACE, &XDG_TOPLEVEL,
        ];
        for iface in all {
            for (name, sig) in iface.requests {
                for c in sig.chars() {
                    assert!(
                        "iufsonah".contains(c),
                        "{}.{} 的签名 {sig:?} 含未知类型 {c:?}",
                        iface.name,
                        name
                    );
                }
            }
        }
    }

    /// spike 只 advertise 这三个全局 —— 输入与显示器是范围外。
    #[test]
    fn globals_are_the_expected_three() {
        let names: Vec<&str> = GLOBALS.iter().map(|(_, n, _)| *n).collect();
        assert_eq!(names, ["wl_compositor", "wl_shm", "xdg_wm_base"]);
        // 没做输入：谁要 wl_seat 就会拿到"全局不存在"，而不是挂死
        assert!(!names.contains(&"wl_seat"));
    }

    /// advertise 的版本不能超过接口定义的版本（客户端会按 advertise 的版本 bind）。
    #[test]
    fn global_versions_do_not_exceed_interface_versions() {
        for (iface, _, ver) in GLOBALS {
            assert!(ver <= &iface.version, "{} advertise 了高于定义的版本", iface.name);
        }
    }
}
