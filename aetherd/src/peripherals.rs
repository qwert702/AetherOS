//! 外设的**最小切片**（§3.7）：只读检测 + 如实报告。
//!
//! ## 范围（按项目对同类问题的既有做法）
//!
//! §3.8 多显示器的处理方式是「**如实报告**：检测输出 + 界面说清只绘制主输出」。
//! 本模块对蓝牙与打印用同一套：
//!
//! * **蓝牙**：从 `/sys/class/bluetooth` 数出适配器（名字 + 地址）。
//!   **配对、连接、音频 profile 都不在本版范围** —— 那需要 BlueZ（D-Bus）一整套；
//! * **打印**：看有没有 CUPS（socket 或配置），有则把 `printers.conf` 里**已配置**的队列名读出来。
//!   **提交作业、查询状态不在本版范围** —— 那需要 CUPS 客户端。
//!
//! 一句话：**能如实看到的就显示，做不到的明说做不到** —— 不画假开关。
//!
//! ## 为什么目录/文件路径是参数
//!
//! 与 `removable::scan(sys_block)` 同一手法：把路径当参数传进来，纯函数就能用
//! 临时目录做测试，不必依赖跑测试的机器上真有什么硬件。

use std::path::Path;

/// 蓝牙适配器的 sysfs 目录。
pub const BT_CLASS: &str = "/sys/class/bluetooth";
/// CUPS 的 unix socket（CUPS 在跑才有）。
pub const CUPS_SOCKET: &str = "/run/cups/cups.sock";
/// CUPS 的队列配置（配置过才有）。
pub const CUPS_PRINTERS: &str = "/etc/cups/printers.conf";

/// 一个蓝牙适配器（只有 sysfs 能给的事实）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// 适配器名，如 `hci0`。
    pub name: String,
    /// 设备地址（`/sys/class/bluetooth/<name>/address`；读不到就是 `None`）。
    pub address: Option<String>,
}

/// 打印子系统的状态。**如实三分**：没有 / 有但没队列 / 有队列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Printing {
    /// socket 与配置都不在 —— 这台机器没有打印子系统。
    NoSubsystem,
    /// CUPS 在，但一个队列都没配。
    NoQueues,
    /// 配了这些队列。
    Queues(Vec<String>),
}

/// 列出蓝牙适配器（**纯函数**：目录由调用方给，便于测试）。
///
/// 只认 `hci*` 这类目录名 —— sysfs 的 `power/`、`subsystem` 之类不是适配器。
pub fn adapters(dir: &Path) -> Vec<Adapter> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        // 目录不存在 = 没有蓝牙子系统。这不是错误，是"没有"。
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("hci") {
            continue;
        }
        let address = std::fs::read_to_string(e.path().join("address"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        out.push(Adapter { name, address });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 从 `printers.conf` 文本里取出**队列名**（纯函数）。
///
/// 格式是 CUPS 的简单配置：每个队列一段，以 `<Printer 名字>` 开头、`</Printer>` 结束。
/// 只取名字 —— 队列状态（空闲/卡纸）要问 CUPS，本版不接。
pub fn printer_names(conf: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in conf.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("<Printer ") {
            let name = rest.trim_end_matches('>').trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 判断打印子系统状态（**纯函数**：路径由调用方给）。
pub fn printing(sock: &Path, conf: &Path) -> Printing {
    let has_cups = sock.exists() || conf.exists();
    if !has_cups {
        return Printing::NoSubsystem;
    }
    let names = std::fs::read_to_string(conf)
        .map(|t| printer_names(&t))
        .unwrap_or_default();
    if names.is_empty() {
        Printing::NoQueues
    } else {
        Printing::Queues(names)
    }
}

/// 检测真实的系统路径，汇总成给界面用的列表。
pub fn list() -> (Vec<Adapter>, Printing) {
    (
        adapters(Path::new(BT_CLASS)),
        printing(Path::new(CUPS_SOCKET), Path::new(CUPS_PRINTERS)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用临时目录造一个"有两块适配器"的 sysfs 形状。
    fn fake_bt() -> tempdir::Dir {
        let d = tempdir::Dir::new("bt");
        std::fs::create_dir_all(d.path().join("hci0")).unwrap();
        std::fs::write(d.path().join("hci0/address"), "00:11:22:33:44:55\n").unwrap();
        std::fs::create_dir_all(d.path().join("hci1")).unwrap();
        // hci1 故意**没有** address —— 模拟 sysfs 读不到的情况
        // 还有几个不该被当成适配器的条目
        std::fs::create_dir_all(d.path().join("power")).unwrap();
        std::fs::write(d.path().join("uevent"), "MODALIAS=...\n").unwrap();
        d
    }

    #[test]
    fn bluetooth_lists_only_hci_entries_and_tolerates_missing_address() {
        let d = fake_bt();
        let a = adapters(d.path());
        assert_eq!(a.len(), 2, "只应认 hci* 目录：{a:?}");
        assert_eq!(a[0].name, "hci0");
        assert_eq!(a[0].address.as_deref(), Some("00:11:22:33:44:55"));
        assert_eq!(a[1].name, "hci1");
        assert_eq!(a[1].address, None, "读不到地址时应当是 None，而不是空串");
    }

    #[test]
    fn missing_directory_means_no_bluetooth_not_an_error() {
        let d = tempdir::Dir::new("empty");
        assert!(adapters(d.path()).is_empty());
        // 连目录都不存在也一样：这是"没有"，不是"出错"
        assert!(adapters(&d.path().join("nope")).is_empty());
    }

    #[test]
    fn printers_conf_yields_queue_names_only() {
        let conf = "\
# 这是注释
<Printer Office>
PrinterId 1
DeviceURI ipp://x
State Idle
</Printer>
<DefaultPrinter Home>
State Idle
</DefaultPrinter>
";
        // 只认 <Printer …>；`<DefaultPrinter …>` 也是"配置了队列"，
        // 但它不以 "<Printer " 开头 —— 这里**如实只报 <Printer>**，
        // 因为 CUPS 的 printers.conf 里 DefaultPrinter 是独立的默认标记，不是另一台打印机。
        assert_eq!(printer_names(conf), vec!["Office"]);
        assert!(printer_names("").is_empty());
    }

    #[test]
    fn printing_state_is_three_way_and_honest() {
        let d = tempdir::Dir::new("cups");
        let sock = d.path().join("cups.sock");
        let conf = d.path().join("printers.conf");

        // ① 都不在 → 没有子系统
        assert_eq!(printing(&sock, &conf), Printing::NoSubsystem);

        // ② 只有 socket（CUPS 在跑，但没配队列）
        std::fs::write(&sock, "").unwrap();
        assert_eq!(printing(&sock, &conf), Printing::NoQueues);

        // ③ 配上队列
        std::fs::write(&conf, "<Printer Office>\nState Idle\n</Printer>\n").unwrap();
        assert_eq!(
            printing(&sock, &conf),
            Printing::Queues(vec!["Office".to_string()])
        );
    }
}

/// 测试用的临时目录（RAII：析构时删掉）。
///
/// 为什么自己写一个：仓库没有 tempfile 依赖（`--offline --locked`，不该为一个测试加依赖）。
/// 名字带进程号与计数器，避免并行测试互相撞。
#[cfg(test)]
pub mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    pub struct Dir(PathBuf);

    impl Dir {
        pub fn new(tag: &str) -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "aether-test-{}-{}-{}",
                std::process::id(),
                tag,
                n
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("建临时目录");
            Dir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
