//! aether-init — AetherOS 自研 init（M3 实现）。
//!
//! 作为 PID 1 运行于 initramfs 之后：挂载伪文件系统、按依赖顺序拉起
//! 服务、回收孤儿进程、响应 aether-ipc 的 ServiceControl 请求。
//! 设计原则：小而美，不照抄 systemd。静态链接（musl），无任何
//! 动态库依赖，保证 init 永远能跑起来。
//!
//! 注意：本 crate 最终在 Linux 上以 `#![no_main]` 级别的最小依赖构建，
//! 当前为 M0 骨架占位，可在任意平台编译以便日常 CI。

fn main() {
    println!("aether-init: AetherOS PID 1（M3 里实现，当前为骨架占位）");
}
