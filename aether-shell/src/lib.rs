//! aether-shell — Shell 归属决策的边界声明 crate。
//!
//! 依据 ADR《架构决策（ADR）-aether-shell归属-2026-11-13.md》：
//! **暂不拆分**，本 crate 仅作为保留命名空间（reserved namespace）存在；
//! 顶栏、Dock、工作区、AI 指令条等 Shell 运行时职责实际由 `aether-compositor` 承担，
//! M10 平台化阶段再评估是否拆出。
//!
//! 本模块不承载任何 Shell 运行时逻辑，只把上述边界事实集中在库层的
//! [`ShellOwnership`] / [`ownership`] 里：命令行 `--status` 的打印文本由它渲染，
//! 单元测试也直接断言这些事实，避免归属说法散落在各处字符串里。
//!
//! 代码仅依赖 `std`；Windows / Linux 均可编译运行。

/// Shell 职责归属与本 crate 定位的边界事实（与 2026-11-13 ADR 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOwnership {
    /// 实际承担 Shell 职责（顶栏 / Dock / 工作区 / AI 指令条）的 crate 名。
    pub shell_host_crate: &'static str,
    /// 本 crate 的定位：保留命名空间，不实现 Shell 运行时职责。
    pub namespace_role: &'static str,
    /// 再评估拆分时机的里程碑。
    pub reassess_milestone: &'static str,
    /// 触发再评估（先写新 ADR 再决定拆分 / 删除）的条件。
    pub reassess_triggers: &'static [&'static str],
    /// 对应 ADR 文档路径（相对仓库根目录）。
    pub adr_path: &'static str,
    /// ADR 决策日期。
    pub adr_date: &'static str,
}

/// 实际承担 Shell 职责的 crate。
pub const SHELL_HOST_CRATE: &str = "aether-compositor";

/// 本 crate 定位。
pub const NAMESPACE_ROLE: &str =
    "保留命名空间（reserved namespace）；暂不拆分，本 crate 不实现 Shell 运行时职责";

/// 再评估里程碑。
pub const REASSESS_MILESTONE: &str = "M10 平台化阶段";

/// 再评估触发条件（与 ADR「后果」一节对齐）。
pub const REASSESS_TRIGGERS: &[&str] = &[
    "出现第三方 Shell 需求",
    "出现多会话 / 多用户会话需求",
    "出现独立 Wayland 会话需求",
];

/// ADR 文档路径（相对仓库根目录）。
pub const ADR_PATH: &str = "docs/架构决策（ADR）-aether-shell归属-2026-11-13.md";

/// ADR 决策日期。
pub const ADR_DATE: &str = "2026-11-13";

/// 返回当前生效的 Shell 归属边界事实。
pub fn ownership() -> ShellOwnership {
    ShellOwnership {
        shell_host_crate: SHELL_HOST_CRATE,
        namespace_role: NAMESPACE_ROLE,
        reassess_milestone: REASSESS_MILESTONE,
        reassess_triggers: REASSESS_TRIGGERS,
        adr_path: ADR_PATH,
        adr_date: ADR_DATE,
    }
}

/// 子命令解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliCommand {
    /// `--status` / `--describe`：打印归属边界事实。
    Status,
    /// `--help` / `-h`（或无参数）：打印用法。
    Help,
    /// 未识别的参数：退出码非 0。
    Unknown,
}

/// 解析命令行参数（不含 argv[0] 程序名）。
pub fn parse_args<I>(args: I) -> CliCommand
where
    I: IntoIterator<Item = String>,
{
    let mut saw_status = false;
    let mut unknown = false;
    for arg in args {
        match arg.as_str() {
            "--status" | "--describe" => saw_status = true,
            "--help" | "-h" => {}
            _ => unknown = true,
        }
    }
    if unknown {
        CliCommand::Unknown
    } else if saw_status {
        CliCommand::Status
    } else {
        // 无参数 / 仅 --help：给出用法提示，退出码 0。
        CliCommand::Help
    }
}

/// 命令对应的进程退出码。
pub fn exit_code(cmd: CliCommand) -> i32 {
    match cmd {
        CliCommand::Status | CliCommand::Help => 0,
        CliCommand::Unknown => 2,
    }
}

/// 渲染 `--status` 的多行归属说明文本。
pub fn describe_text() -> String {
    ownership().describe()
}

impl ShellOwnership {
    /// 渲染为多行人类可读文本；字段标签固定，供 `--status` 输出与测试断言。
    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        lines.push("aether-shell — Shell 归属边界".to_string());
        lines.push(format!("Shell 职责归属: {}", self.shell_host_crate));
        lines.push(format!("本 crate 定位: {}", self.namespace_role));
        lines.push(format!("再评估时机: {}", self.reassess_milestone));
        lines.push("再评估触发条件:".to_string());
        for trigger in self.reassess_triggers {
            lines.push(format!("  - {trigger}"));
        }
        lines.push(format!(
            "ADR: {}（决策日期 {}）",
            self.adr_path, self.adr_date
        ));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 归属声明与决策一致() {
        let o = ownership();
        assert_eq!(o.shell_host_crate, "aether-compositor");
        assert!(o.namespace_role.contains("保留命名空间"));
        assert!(o.namespace_role.contains("暂不拆分"));
        assert_eq!(o.reassess_milestone, "M10 平台化阶段");
        assert!(o.adr_path.contains("aether-shell归属"));
        assert_eq!(o.adr_date, "2026-11-13");
        // ADR「后果」一节列出的三类触发条件必须全部在册。
        let triggers = o.reassess_triggers.join(" | ");
        assert!(triggers.contains("第三方 Shell"));
        assert!(triggers.contains("多会话"));
        assert!(triggers.contains("Wayland 会话"));
    }

    #[test]
    fn status文本包含全部必需字段() {
        let text = describe_text();
        for required in [
            "Shell 职责归属",
            "aether-compositor",
            "本 crate 定位",
            "保留命名空间",
            "再评估时机",
            "M10",
            "再评估触发条件",
            "第三方 Shell",
            "多会话",
            "Wayland 会话",
            "ADR",
            "aether-shell归属",
        ] {
            assert!(text.contains(required), "status 输出缺少字段: {required}");
        }
    }

    #[test]
    fn 已知参数解析为状态或帮助() {
        assert_eq!(
            parse_args(["--status".to_string()]),
            CliCommand::Status
        );
        assert_eq!(
            parse_args(["--describe".to_string()]),
            CliCommand::Status
        );
        assert_eq!(parse_args(["--help".to_string()]), CliCommand::Help);
        assert_eq!(parse_args(["-h".to_string()]), CliCommand::Help);
        assert_eq!(parse_args(Vec::<String>::new()), CliCommand::Help);
    }

    #[test]
    fn 未知参数返回非零退出码() {
        let cmd = parse_args(["--bogus".to_string()]);
        assert_eq!(cmd, CliCommand::Unknown);
        assert_ne!(exit_code(cmd), 0);
        // 合法命令退出码必须为 0。
        assert_eq!(exit_code(CliCommand::Status), 0);
        assert_eq!(exit_code(CliCommand::Help), 0);
    }

    #[test]
    fn 未知参数混在合法参数中仍判未知() {
        let cmd = parse_args(["--status".to_string(), "--bogus".to_string()]);
        assert_eq!(cmd, CliCommand::Unknown);
        assert_ne!(exit_code(cmd), 0);
    }
}
