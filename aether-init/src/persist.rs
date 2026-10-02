//! 持久化分区：把安装器建好的 ext4 分区（卷标 AETHER，通常是 /dev/<盘>2）
//! 在启动时挂到 /var，使日志、诊断报告、审计跨重启保留。
//!
//! 设计：
//! - 候选设备是编译期常量列表（与 aether-install 的分区约定一致）；
//!   逐个尝试挂载，成功即止——光盘启动（无该分区）时全部失败，静默退回内存态。
//! - 挂载在服务启动之前完成，因此 logtee 写 /var/log/aether 时已经是真磁盘。
//! - 每次成功挂载都往 /var/boot.log 追加一条启动记录（持久化的自证）。
//! - 与平台相关仅限 Linux；其余平台为 no-op（可编译、可单测纯逻辑）。

// 挂载路径在非 Linux 平台不被调用，但代码保留为可读文档与单测载体
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// 持久化分区候选：virtio 优先，其次 SATA/IDE（VMware/VBox 常用），最后 nvme。
pub const PERSIST_CANDIDATES: [&str; 4] =
    ["/dev/vda2", "/dev/sda2", "/dev/hda2", "/dev/nvme0n1p2"];

/// 我们自己的持久化分区的 ext4 卷标。
///
/// **必须与 `aether-install::PERSIST_LABEL` 一致**（安装器用 `mkfs.ext4 -L` 写入）。
/// 两处各有一份常量，是因为 aether-init 不该依赖 aether-install（那是可执行 crate），
/// 而 aether-install 连 aether-ipc 都不依赖（只为这一个常量拉进 serde 不划算）。
/// 因此**两侧各钉一次字面量**：本文件有 `persist_label_and_mount_opts_follow_contract`，
/// aether-install 有 `persist_label_matches_init_convention`。
/// 只改一侧 ⇒ 那一侧的测试变红 —— 这是"改一处会红"能达到的最好效果
/// （真正的跨 crate 钉法需要共享常量所在的新 crate，见审计报告附录 C 的残余项）。
pub const PERSIST_LABEL: &str = "AETHER";

/// 读设备头部这么多字节就够（ext4 超级块在 1024，卷标在 1024+0x78）。
const EXT_HEAD_LEN: usize = 2048;

/// /var 下需要存在的目录（挂载覆盖后可能为空，运行时补齐）。
pub const VAR_DIRS: [&str; 3] = ["/var/log/aether", "/var/diag", "/var/tmp"];

/// 挂载选项。
///
/// **为什么是 nodev,nosuid 而不是 nodev,nosuid,noexec**（对抗审查修正）：
/// 这个分区上不只是日志/诊断/回收站 —— `aetherd` 的**已装应用**就住在 `/var/apps`
/// （`aetherd::apps::DEFAULT_DIR`），包装脚本 `exec /var/apps/<id>/<entry>` 并设
/// `LD_LIBRARY_PATH`。`noexec` 会同时挡掉 execve 与 `dlopen` 的 PROT_EXEC 映射，
/// 于是"装个 htop 跑起来"这个已实测宣传的功能，在带持久分区安装后就失效了。
/// `nodev`（设备节点）与 `nosuid`（setuid/setgid 位）才是这一层真正需要的两个约束：
/// 它们挡掉"用持久分区提权"，而不影响正常程序执行。
///
/// `errors=remount-ro`（2026-10-02 审计收尾）：文件系统出错时**转只读**而不是继续写。
/// 日志/审计盘上一旦发生结构性错误，继续写只会扩大损坏面；转只读后系统仍能跑
/// （日志退化为不可写，`aether-init` 会提示），用户有机会把数据抢救出来。
///
/// 残余风险（已知、有理由）：该分区可写且可执行 ⇒ 能往它写的人可以留持久化代码。
/// 但"能写"已经要求 root（或经 AI 的 L2 确认写白名单），所以在当前单用户模型下不构成升级路径。
pub const PERSIST_MOUNT_OPTS: &str = "nodev,nosuid,errors=remount-ro";

/// fsck 退出码 ≥ 此值表示"有未纠正的错误"（e2fsck 语义：4 = 未修复，8 = 操作错误）。
const FSCK_UNCORRECTED_CODE: i32 = 4;

/// 安装标记（相对分区根）：由 `aether-install` 在建立持久分区时写入。
///
/// 这是"这个分区确实是本机装出来的"的第二个判据（第一个是卷标）。见
/// [`has_install_marker`] 的说明 —— 它挡的是误挂，不是有物理访问权的攻击者。
pub const INSTALL_MARKER: &str = "log/install-id";

/// 身份探测用的临时挂载点（`/run` 是 tmpfs，随时可写；不复用 `/var`，
/// 因为探测阶段 `/var` 还得保持原样）。
#[cfg(target_os = "linux")]
const PROBE_MNT: &str = "/run/aether-persist-probe";

/// 探测用外部命令（mount/umount）的超时。
///
/// PID 1 **不能**被一次卡住的挂载拖停：设备异常时 `mount` 可能长时间不返回，
/// 而服务监督与救援控制台都在同一个进程里。15 秒对正常设备绰绰有余。
#[cfg(target_os = "linux")]
const MOUNT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 只读探测：分区里有没有安装标记。
///
/// 为什么必须是**只读**挂载（2026-10-02 审计 H-3 残余）：ext4 在 rw 挂载时会重放
/// 日志、更新时间戳 —— 对"误挂别人的分区"这个场景，**写盘本身就是损失**。
/// 所以顺序是：卷标 → 只读探测标记 → fsck → rw 挂载；把 fsck 放在探测之后，
/// 也是因为 `fsck -p` 会修改文件系统。
///
/// 诚实边界：能写盘的人可以伪造卷标与标记。它挡的是**误挂**
/// （双系统机器上把宿主机的根分区当 /var），不是有物理访问权的攻击者 ——
/// 后者要靠安全启动/TPM，不在本系统当前能力范围内。
/// 带超时地跑一条外部命令，返回是否成功。
///
/// **为什么必须有超时**（代码审查发现）：这段代码跑在 PID 1 里。设备挂起或
/// `/bin/mount` 卡住时，一次无超时的 `status()` 会把整个服务监督与救援控制台
/// 一起拖停 —— 那是"为了挂个日志盘把系统搭进去"。超时后杀掉子进程并按失败处理。
///
/// 用 `spawn` + 轮询 `try_wait`（100ms 粒度）而不是 `status()`：标准库没有
/// "带超时的 wait"，而轮询在这里的代价可以忽略（只在启动时跑一两次）。
#[cfg(target_os = "linux")]
fn run_with_timeout(cmd: &str, args: &[&str], timeout: std::time::Duration) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    eprintln!("[aether-init] {cmd} 超过 {timeout:?} 未返回，已终止");
                    let _ = child.kill(); // 杀不掉也只能放弃（下面 wait 会立刻返回）
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("[aether-init] 等待 {cmd} 失败：{e}");
                return false;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn has_install_marker(dev: &str) -> bool {
    if std::fs::create_dir_all(PROBE_MNT).is_err() {
        return false;
    }
    // 先尝试卸载：上一次探测如果异常中断（进程被杀），挂载点会残留，
    // 那样 mount 会直接失败 → 永远认不出持久分区（可用性回归）。
    // 卸载失败**不阻断**（可能本来就没挂），只当清理。
    let _ = run_with_timeout("/bin/umount", &[PROBE_MNT], MOUNT_TIMEOUT);
    let mounted = run_with_timeout(
        "/bin/mount",
        &["-t", "ext4", "-o", "ro,nodev,nosuid,noexec", dev, PROBE_MNT],
        MOUNT_TIMEOUT,
    );
    if !mounted {
        return false;
    }
    let marker = std::path::Path::new(PROBE_MNT).join(INSTALL_MARKER);
    // 必须是**普通文件**，且不能是符号链接（对抗审查发现）：分区里放一个
    // `log/install-id -> boot.log` 就能骗过 `is_file()`（它跟随链接）。
    let found = std::fs::symlink_metadata(&marker)
        .map(|md| md.file_type().is_file())
        .unwrap_or(false);
    // 卸载必须成功：否则 rw 挂载会失败（EBUSY），而我们又报"不是我们的分区"，
    // 用户看到的是"持久化没了"却查不出原因。卸载失败时把它当成"探测失败"（保守）。
    let unmounted = run_with_timeout("/bin/umount", &[PROBE_MNT], MOUNT_TIMEOUT);
    if !unmounted {
        println!("[aether-init] 警告：探测挂载点 {PROBE_MNT} 卸载失败 —— 本次不挂载持久分区");
        return false;
    }
    found
}

/// 从设备头部字节里解析 ext4 卷标（**纯函数**，可在开发机上单测）。
///
/// ext4 超级块固定在偏移 1024：`s_magic` 在 +0x38（`0xEF53`），`s_volume_name`
/// 在 +0x78（16 字节，NUL 填充）。只读 2KiB 就能判定，**不需要 blkid**
/// （镜像里是 busybox，参数支持不齐），也不需要挂载。
///
/// 返回 `None` = 不是 ext4；`Some("")` = 是 ext4 但没卷标。
pub fn ext4_label(head: &[u8]) -> Option<String> {
    const SB: usize = 1024;
    const MAGIC_OFF: usize = SB + 0x38;
    const LABEL_OFF: usize = SB + 0x78;
    const MAGIC: u16 = 0xEF53;
    const LABEL_LEN: usize = 16;

    let magic = u16::from_le_bytes(head.get(MAGIC_OFF..MAGIC_OFF + 2)?.try_into().ok()?);
    if magic != MAGIC {
        return None;
    }
    let raw = head.get(LABEL_OFF..LABEL_OFF + LABEL_LEN)?;
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    Some(String::from_utf8_lossy(&raw[..end]).trim().to_string())
}

/// 读块设备头部（只读打开；失败 → None）。
#[cfg(target_os = "linux")]
fn read_device_head(dev: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(dev).ok()?;
    let mut buf = vec![0u8; EXT_HEAD_LEN];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// 该设备是不是"我们的"持久化分区（ext4 且卷标为 [`PERSIST_LABEL`]）。
///
/// 2026-10-02 审计 H-3：原实现只判"设备存在 + mount 成功"。而候选表里的
/// `/dev/nvme0n1p2` 恰好是多数 Linux 发行版的 root 分区 —— 在双系统机器上引导
/// 本镜像，会把**别人的根分区**以 rw 挂成 /var，然后往里写日志（污染另一个系统）。
#[cfg(target_os = "linux")]
fn is_our_persist_partition(dev: &str) -> Result<(), String> {
    match read_device_head(dev) {
        Some(head) => match ext4_label(&head) {
            Some(label) if label == PERSIST_LABEL => Ok(()),
            Some(other) => Err(format!("卷标是「{other}」，不是 {PERSIST_LABEL}")),
            None => Err("没有 ext4 超级块".to_string()),
        },
        None => Err("读不到设备头部（权限或设备异常）".to_string()),
    }
}

/// 挂载前的文件系统检查。返回 `true` = 可以挂载。
///
/// 判据用 fsck 的退出码（权威）：0/1 = 干净或已自动修复；≥4 = 有未纠正的错误 →
/// **不挂载**（把可能损坏的文件系统以 rw 挂上去，比暂时不挂更糟）。
/// 工具缺失时返回 true 并明确提示：不因缺工具而让系统失去持久化能力。
#[cfg(target_os = "linux")]
fn fsck_before_mount(dev: &str) -> bool {
    const FSCK_TOOLS: [&str; 2] = ["/sbin/fsck.ext4", "/usr/sbin/fsck.ext4"];
    for tool in FSCK_TOOLS {
        if !std::path::Path::new(tool).exists() {
            continue;
        }
        match std::process::Command::new(tool).arg("-p").arg(dev).status() {
            Ok(st) => {
                let code = st.code().unwrap_or(FSCK_UNCORRECTED_CODE);
                if code < FSCK_UNCORRECTED_CODE {
                    println!("[aether-init] fsck {dev}: 通过（退出码 {code}）");
                    return true;
                }
                println!(
                    "[aether-init] 拒绝挂载 {dev}：fsck 报未纠正的错误（退出码 {code}），\
                     请手工修复后再启动"
                );
                return false;
            }
            Err(e) => {
                println!("[aether-init] 警告：fsck {dev} 执行失败（{e}）—— 跳过检查并继续");
                return true;
            }
        }
    }
    println!("[aether-init] 提示：镜像内没有 fsck.ext4，跳过文件系统检查");
    true
}

/// 启动记录行：`<unix秒> 启动（第 N 次）`。安装行不计入启动次数。
pub fn boot_record_line(ts: u64, boot_index: usize) -> String {
    format!("{ts} 启动（第 {boot_index} 次）\n")
}

/// 统计已有启动记录条数（只数含"启动"的行，安装行等其它内容不计）。
pub fn count_boot_records(text: &str) -> usize {
    text.lines().filter(|l| l.contains("启动")).count()
}

/// 把持久化分区挂到 /var。返回挂载成功的设备名。
///
/// 四道门，顺序有讲究（2026-10-02 审计 H-3）：
/// 1. **身份**：ext4 且卷标为 `AETHER` —— 否则跳过（候选表里的 nvme 分区可能是别人的 root）；
/// 2. **归属**：**只读**探测安装标记 `log/install-id` —— 只读是因为 rw 挂载会写盘，
///    而这一步的整个意义就是"不碰不属于我们的东西"；
/// 3. **健康**：`fsck -p`（会改文件系统，所以必须排在"确认归属"之后），
///    有未纠正的错误就不挂；
/// 4. **收敛**：挂载选项 `nodev,nosuid`（只放日志/诊断/回收站/已装应用，
///    不需要设备节点与 setuid；但**不能加 noexec** —— 已装应用要能执行）。
#[cfg(target_os = "linux")]
pub fn mount_persist() -> Option<String> {
    for dev in PERSIST_CANDIDATES {
        if !std::path::Path::new(dev).exists() {
            continue;
        }
        if let Err(why) = is_our_persist_partition(dev) {
            println!("[aether-init] 跳过 {dev}：{why}（不是本系统的持久化分区）");
            continue;
        }
        if !has_install_marker(dev) {
            println!(
                "[aether-init] 跳过 {dev}：卷标是 {PERSIST_LABEL}，但没有安装标记 \
                 {INSTALL_MARKER} —— 不是本机装出来的分区，拒绝挂载（也不做 fsck：那是写操作）"
            );
            continue;
        }
        if !fsck_before_mount(dev) {
            continue;
        }
        let ok = std::process::Command::new("/bin/mount")
            .args(["-t", "ext4", "-o", PERSIST_MOUNT_OPTS, dev, "/var"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            continue;
        }
        for d in VAR_DIRS {
            let _ = std::fs::create_dir_all(d);
        }
        println!("[aether-init] 持久化分区已挂载: {dev} -> /var（{PERSIST_MOUNT_OPTS}）");
        append_boot_record();
        return Some(dev.to_string());
    }
    println!("[aether-init] 未发现持久化分区（内存态运行，重启后状态不保留）");
    None
}

/// 追加一条启动记录到 /var/boot.log（失败只忽略：记录是附加价值，不阻塞启动）。
#[cfg(target_os = "linux")]
fn append_boot_record() {
    use std::io::Write;
    let path = "/var/boot.log";
    let count = std::fs::read_to_string(path)
        .map(|s| count_boot_records(&s) + 1)
        .unwrap_or(1);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(boot_record_line(ts, count).as_bytes());
    }
}

#[cfg(not(target_os = "linux"))]
pub fn mount_persist() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_partition_two_of_common_disks() {
        // 与 aether-install 的约定一致：持久化分区号固定为 2
        for c in PERSIST_CANDIDATES {
            assert!(c.starts_with("/dev/"));
            assert!(
                c.ends_with('2') || c.ends_with("p2"),
                "{c} 应指向第 2 个分区"
            );
        }
        assert_eq!(PERSIST_CANDIDATES[0], "/dev/vda2", "virtio 应优先");
    }

    #[test]
    fn boot_record_is_appended_style() {
        let line = boot_record_line(1_700_000_000, 3);
        assert!(line.ends_with('\n'), "记录行必须以换行结尾（追加友好）");
        assert!(line.contains("1700000000"));
        assert!(line.contains("第 3 次"));
    }

    #[test]
    fn boot_counter_ignores_install_line() {
        // 安装器写的初始内容不含"启动"，首启应记为第 1 次
        let installed = "1700000000 安装完成（分区 /dev/vda2）\n";
        assert_eq!(count_boot_records(installed), 0);
        let after_first = format!("{installed}1700000001 启动（第 1 次）\n");
        assert_eq!(count_boot_records(&after_first), 1);
        // 重启后计数递增
        let next = count_boot_records(&after_first) + 1;
        assert_eq!(next, 2);
    }

    #[test]
    fn var_dirs_cover_logs_and_diag() {
        assert!(VAR_DIRS.contains(&"/var/log/aether"));
        assert!(VAR_DIRS.contains(&"/var/diag"));
    }

    /// 造一个 2KiB 的假设备头部：可指定 ext4 magic 与卷标。
    fn fake_head(magic: u16, label: &str) -> Vec<u8> {
        let mut b = vec![0u8; EXT_HEAD_LEN];
        b[1024 + 0x38..1024 + 0x3A].copy_from_slice(&magic.to_le_bytes());
        let lb = label.as_bytes();
        let n = lb.len().min(16);
        b[1024 + 0x78..1024 + 0x78 + n].copy_from_slice(&lb[..n]);
        b
    }

    /// H-3 回归：卷标必须能从超级块里读出来 —— 这是"只挂自己的分区"的唯一依据。
    #[test]
    fn ext4_label_is_parsed_from_superblock() {
        assert_eq!(ext4_label(&fake_head(0xEF53, "AETHER")).as_deref(), Some("AETHER"));
        // 不是 ext4 → None（调用方据此跳过该设备）
        assert_eq!(ext4_label(&fake_head(0x1234, "AETHER")), None);
        // 是 ext4 但没卷标 → Some("")，仍要能区分于"不是 ext4"
        assert_eq!(ext4_label(&fake_head(0xEF53, "")).as_deref(), Some(""));
        // 别人的分区卷标照原样返回 —— 正是靠它把宿主机的 root 分区挡在外面
        assert_eq!(ext4_label(&fake_head(0xEF53, "ROOT")).as_deref(), Some("ROOT"));
        // 16 字节塞满、没有 NUL 结尾 → 全取
        assert_eq!(
            ext4_label(&fake_head(0xEF53, "0123456789ABCDEF")).as_deref(),
            Some("0123456789ABCDEF")
        );
        // 头部读不全 → None，不能把"读不全"当成"没有卷标"
        let short = [0u8; 100];
        assert_eq!(ext4_label(&short), None);
        let empty: [u8; 0] = [];
        assert_eq!(ext4_label(&empty), None);
    }

    /// H-3 回归：卷标与挂载选项的约定不能被改坏。
    #[test]
    fn persist_label_and_mount_opts_follow_contract() {
        // 必须与 aether-install 的 `mkfs.ext4 -L` 一致（两侧各钉一次字面量）
        assert_eq!(PERSIST_LABEL, "AETHER");
        // 安装标记路径必须与安装器写的那一个一致（aether-install::setup_persist）
        assert_eq!(INSTALL_MARKER, "log/install-id");
        // 这两项挡掉"用持久分区提权"：设备节点与 setuid 位
        let opts: Vec<&str> = PERSIST_MOUNT_OPTS.split(',').collect();
        for want in ["nodev", "nosuid"] {
            assert!(opts.contains(&want), "挂载选项缺少 {want}：{PERSIST_MOUNT_OPTS}");
        }
        // 出错转只读：继续写只会扩大损坏面
        assert!(
            opts.contains(&"errors=remount-ro"),
            "挂载选项缺少 errors=remount-ro：{PERSIST_MOUNT_OPTS}"
        );
        // **不能有 noexec**：/var/apps 上的已装应用要能 execve（见常量文档）
        assert!(
            !opts.contains(&"noexec"),
            "不能加 noexec —— 它会让 /var/apps 里已装的应用无法运行：{PERSIST_MOUNT_OPTS}"
        );
        assert_eq!(opts.len(), 3, "不应有意外选项：{PERSIST_MOUNT_OPTS}");
    }
}
