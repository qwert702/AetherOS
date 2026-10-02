//! 服务单元定义：AetherOS 的"什么服务、怎么启动、依赖谁"。
//!
//! 安全模型：AetherOS 是封闭系统，可运行的服务集合 = 编译期内建于
//! init 的白名单，每个服务的启动命令都是字面量构造（无任何字符串
//! 拼接进命令行），服务文件 JSON 只能引用白名单内的服务并配置策略。
//! 第三方应用后续通过签名清单机制接入（见 docs/roadmap.md）。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// 白名单服务：全部已知的 AetherOS 系统服务。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownService {
    /// 网络配置（Buildroot busybox udhcpc）
    Network,
    /// AI 中枢
    AetherD,
    /// 桌面合成器
    Compositor,
    /// AI 运维
    Ops,
    /// 终端登录（tty1）
    Getty,
    /// 内置自退出探针：用于监督逻辑自检（退出码 0）
    Noop,
}

impl KnownService {
    pub fn from_name(name: &str) -> Result<Self> {
        Ok(match name {
            "network" => Self::Network,
            "aetherd" => Self::AetherD,
            "compositor" => Self::Compositor,
            "ops" => Self::Ops,
            "getty" => Self::Getty,
            "noop" => Self::Noop,
            other => bail!("未知服务「{other}」：服务必须内建于 aether-init 白名单"),
        })
    }

    #[allow(dead_code)] // 供审计日志与后续 ops 使用
    pub fn name(&self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::AetherD => "aetherd",
            Self::Compositor => "compositor",
            Self::Ops => "ops",
            Self::Getty => "getty",
            Self::Noop => "noop",
        }
    }

    /// 构造启动命令：全部字面量，参数为编译期常量（无注入面）。
    pub fn spawn_command(&self) -> Command {
        match self {
            Self::Network => {
                #[cfg(target_os = "linux")]
                {
                    // 前台常驻（-f 强制前台）：不加 -f/-b 时 busybox udhcpc 会 fork 后台后
                    // 令父进程返回 0，被 aether-init 监督循环误判为崩溃而无限退避重启。
                    let mut c = Command::new("/sbin/udhcpc");
                    c.arg("-f").arg("-i").arg("eth0");
                    c
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Command::new("noop-placeholder-network")
                }
            }
            Self::AetherD => {
                let mut c = Command::new("/usr/bin/aetherd");
                c.arg("serve");
                c
            }
            Self::Compositor => Command::new("/usr/bin/aether-compositor"),
            Self::Ops => {
                let mut c = Command::new("/usr/bin/aether-ops");
                c.arg("run");
                c
            }
            Self::Getty => {
                #[cfg(target_os = "linux")]
                {
                    let mut c = Command::new("/sbin/getty");
                    c.arg("115200").arg("tty1");
                    c
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Command::new("noop-placeholder-getty")
                }
            }
            Self::Noop => {
                #[cfg(target_os = "linux")]
                {
                    Command::new("/bin/true")
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let mut c = Command::new("cmd");
                    c.args(["/C", "exit", "0"]);
                    c
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// 必须是白名单服务名
    pub name: String,
    /// 依赖的服务名（启动顺序约束）
    #[serde(default)]
    pub deps: Vec<String>,
    /// 系统关键服务
    #[serde(default)]
    pub essential: bool,
    /// 退出后是否自动重启（带退避）
    #[serde(default = "default_restart")]
    pub restart: bool,
    /// 开机自启
    #[serde(default = "default_autostart")]
    pub autostart: bool,
    /// 以哪个用户运行（用户名或数字 uid）。
    ///
    /// **为什么要有这个字段**（2026-10-02 审计 M-5）：此前所有服务都以 root 运行，
    /// 而"哪个服务真的需要 root"从代码里看不出来 —— 一旦某个服务被攻破，
    /// 就是整机沦陷。现在**必须显式声明**：写 `"user": "root"` 表示"确认它需要 root"，
    /// 省略则默认 root 并会在启动日志里提示（不改变现有行为，但让依赖变得可见）。
    #[serde(default = "default_user")]
    pub user: String,
    /// 附加组（可选）。留空表示只用该用户的主组。
    #[serde(default)]
    pub groups: Vec<String>,
}

fn default_restart() -> bool {
    true
}

fn default_autostart() -> bool {
    true
}

/// 默认用户：root（与既有行为一致）。
fn default_user() -> String {
    "root".to_string()
}

/// 从 `/etc/passwd` 文本里解析用户名 → uid（纯函数，便于单测）。
///
/// 数字直接当 uid 用（允许 `"user": "1000"`）；找不到返回 None。
/// 格式：`name:x:uid:gid:gecos:home:shell`
///
/// 非 Linux 平台不调用它（降权只在真机上做），故允许 dead_code。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_uid(passwd: &str, user: &str) -> Option<u32> {
    if let Ok(n) = user.parse::<u32>() {
        return Some(n);
    }
    passwd.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3 && f[0] == user {
            f[2].parse::<u32>().ok()
        } else {
            None
        }
    })
}

/// 从 `/etc/group` 文本里解析组名 → gid（纯函数）。数字直接当 gid 用。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_gid(groups: &str, group: &str) -> Option<u32> {
    if let Ok(n) = group.parse::<u32>() {
        return Some(n);
    }
    groups.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3 && f[0] == group {
            f[2].parse::<u32>().ok()
        } else {
            None
        }
    })
}

impl ServiceSpec {
    pub fn validate(&self) -> Result<KnownService> {
        KnownService::from_name(&self.name)
    }
}

/// 从目录加载全部服务定义（*.json）。
/// 单个文件损坏只跳过并记录告警（文件级故障不该让整机失去全部服务）；
/// 目录本身不可读时返回 Err，由调用方决定是否致命。
pub fn load_dir(dir: &Path) -> Result<(Vec<ServiceSpec>, Vec<String>)> {
    let mut specs = Vec::new();
    let mut warnings = Vec::new();
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("读取服务目录 {dir:?} 失败"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .with_context(|| format!("读取服务文件 {path:?} 失败"))
            .and_then(|raw| {
                serde_json::from_str::<ServiceSpec>(&raw)
                    .with_context(|| format!("解析 {path:?} 失败"))
            });
        match parsed {
            Ok(spec) => match spec.validate() {
                Ok(_) => specs.push(spec),
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            },
            Err(e) => warnings.push(format!("{e:#}")),
        }
    }
    Ok((specs, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **随镜像发布**的服务里，`essential: true` 必须同时 `restart: true`。    ///
    /// 为什么值得一条断言：`restart: false` 的语义在 2026-09-19（`a0f1268` 的 P1-3
    /// "重启策略以服务定义为唯一事实来源"）被改成了**"没人接管"** —— 在那之前
    /// ops 会给 `restart:false` 的服务兜底。而 `compositor.json` 是 09-12 按旧语义
    /// （"崩溃由 ops 自愈，监督器不接管"）设的，没人回头核对，于是它同时失去了
    /// init 监督与 ops 冷拉起：**一次 panic，桌面永久死掉、只能人工重启整机**。
    ///
    /// 这类"跨提交的语义漂移"靠读代码很难发现（两处都自洽），但一行断言就能钉死。
    #[test]
    fn shipped_essential_services_must_be_restartable() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../platform/overlay/etc/aether/services");
        let (specs, warnings) = load_dir(&dir).expect("服务定义应可加载");
        assert!(warnings.is_empty(), "服务定义告警（会被跳过）: {warnings:?}");
        assert!(!specs.is_empty(), "没加载到服务定义，路径对吗: {}", dir.display());
        for s in &specs {
            if s.essential {
                assert!(
                    s.restart,
                    "{} 是 essential 但 restart=false —— 退出后没人拉起，只能人工重启整机",
                    s.name
                );
            }
        }
    }

    #[test]
    fn parse_and_validate() {
        let s = r#"{
            "name": "aetherd",
            "deps": ["network"],
            "essential": true
        }"#;
        let spec: ServiceSpec = serde_json::from_str(s).unwrap();
        assert_eq!(spec.validate().unwrap(), KnownService::AetherD);
        assert!(spec.restart);
        assert!(spec.autostart);
        // 省略 user 时默认 root（与既有行为一致），不是空串 —— 空串会让
        // spawn_with_logs 的降权判断与"root"分支都走不到，语义含糊。
        assert_eq!(spec.user, "root", "省略 user 必须默认 root");
    }

    /// M-5 回归：uid/gid 解析是纯函数，必须能吃下真实的 /etc/passwd 形态。
    #[test]
    fn resolve_user_and_group_from_text() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\n\
                      aether:x:1000:1000:Aether:/home/aether:/bin/sh\n\
                      broken:x:notanumber:0::/:/bin/sh\n";
        assert_eq!(resolve_uid(passwd, "root"), Some(0));
        assert_eq!(resolve_uid(passwd, "aether"), Some(1000));
        assert_eq!(resolve_uid(passwd, "1001"), Some(1001), "数字应直接当 uid");
        assert_eq!(resolve_uid(passwd, "nobody"), None);
        assert_eq!(resolve_uid(passwd, "broken"), None, "uid 非数字应返回 None");

        let groups = "root:x:0:\naether:x:1000:\nwheel:x:10:aether\n";
        assert_eq!(resolve_gid(groups, "aether"), Some(1000));
        assert_eq!(resolve_gid(groups, "wheel"), Some(10));
        assert_eq!(resolve_gid(groups, "9"), Some(9));
        assert_eq!(resolve_gid(groups, "nope"), None);
    }

    /// M-5：**随镜像发布**的服务必须显式声明运行用户。
    ///
    /// 不声明也能跑（默认 root），但"哪个服务真的需要 root"就从代码里消失了 ——
    /// 而这条信息正是最小权限改造的起点。这里钉住"每个服务都写了 user"。
    #[test]
    fn shipped_services_declare_their_user() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../platform/overlay/etc/aether/services");
        for p in std::fs::read_dir(&dir).expect("服务目录应可读").flatten() {
            let text = std::fs::read_to_string(p.path()).unwrap();
            let spec: ServiceSpec = serde_json::from_str(&text).unwrap();
            assert!(
                !spec.user.is_empty(),
                "{} 没有声明 user —— 请显式写 \"user\": \"root\" 或实际用户",
                p.path().display()
            );
        }
    }

    #[test]
    fn unknown_service_rejected() {
        let spec: ServiceSpec =
            serde_json::from_str(r#"{"name":"curl http://evil","argv":[]}"#).unwrap();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn spawn_commands_are_literal() {
        // 白名单服务的启动命令必须可构造（路径/参数为编译期常量）：
        // 只要不 panic 即通过
        for name in ["network", "aetherd", "compositor", "ops", "getty", "noop"] {
            let _ = KnownService::from_name(name).unwrap().spawn_command();
        }
    }
}
