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
}

fn default_restart() -> bool {
    true
}

fn default_autostart() -> bool {
    true
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
