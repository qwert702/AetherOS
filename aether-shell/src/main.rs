//! aether-shell — 保留命名空间 crate（见 ADR 2026-11-13）。
//!
//! 本二进制不启动任何 Shell 运行时；只提供 `--status` / `--describe` 查询
//! 当前 Shell 职责归属与边界事实。事实来源在库层 `aether_shell`，便于单测断言。

use aether_shell::{describe_text, exit_code, parse_args, CliCommand};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = parse_args(args);
    match cmd {
        CliCommand::Status => println!("{}", describe_text()),
        CliCommand::Help => {
            println!(
                "aether-shell: 保留命名空间 crate（Shell 职责由 aether-compositor 承担，见 ADR）"
            );
            println!();
            println!("用法：aether-shell [选项]");
            println!("  --status, --describe  打印 Shell 归属与本 crate 边界事实");
            println!("  --help, -h            显示本帮助");
        }
        CliCommand::Unknown => {
            eprintln!("aether-shell: 未识别的参数（见 --help）");
        }
    }
    std::process::exit(exit_code(cmd));
}
