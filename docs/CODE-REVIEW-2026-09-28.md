# AetherOS 代码审查报告 · 第 4 轮（增量）

- **日期**：2026-09-28
- **审查范围**：`2b16d5d..998ab9b`（14 个提交，上轮安全修复之后）
- **增量规模**：34 个文件，+5601 / −221
- **方法**：源码直读 → 关键索引运算逐条验算 → 真实 IPC 对抗性复现（不 mock）

> **环境说明**：Windows 开发机，`cargo test --workspace --offline` + 真实 `aetherd serve` 回环 IPC。
> **诚实边界**：`aether-compositor/src/pty.rs` 与终端真机行为**无法在本机运行**（Linux 专属），相关结论只到源码层面；
> 本轮**未做**像素回归比对（归档图与 HEAD 存在版本差，见 §4）。

---

## 0. 构建与测试基线

| 项 | 结果 |
|---|---|
| `cargo test --workspace --offline` | **185 项全绿**（105 / 16 / 9 / 6 / 49，另 3 个 crate 无测试） |
| 上轮基线 | 74 项 |
| 增量 | **+111 项**（终端解析器、剪贴板、日志轮转贡献了绝大部分） |
| `aetherd` 二进制 | 需 `cargo build -p aetherd` 重建 —— `cargo test` **不更新 bin target**，直接用旧二进制会得到"无法解析的请求"（本轮踩到，已记录） |

本轮增量结构：**后端（aetherd / aether-init）改动小但触及权限面，前端（compositor）改动大**。

| 层 | 文件 | 规模 |
|---|---|---|
| aetherd | `clipboard.rs`（新）、`server.rs`、`tools.rs`、`main.rs` | ~220 行 |
| aether-init | `logtee.rs`、`main.rs`、`manager.rs`、`ipc.rs` | ~165 行 |
| aether-ipc | `lib.rs` | +36 |
| compositor | `pty.rs`（新）、`vt.rs`（新）、`term.rs`（新）、`input.rs`、`draw.rs`、`main.rs` | ~3900 行 |

---

## 1. 上一轮结论的复核

上一轮（第三轮，2026-09-26）报出的项，本轮逐条实测：

| 上轮项 | 复核方式 | 结论 |
|---|---|---|
| **P1-8** 令牌不绑确认方 | 未注册连接带令牌兑现 L3 | ✅ **已修复**。返回 `403 确认令牌兑现失败：本连接未注册为 UI 通道` |
| **P1-8** 未注册连接触发 L2+ 不下发令牌 | 未注册连接发 `install_disk` | ✅ **已修复**。返回 `403 L2+ 操作（install_disk）必须由已注册的 UI 通道发起` |
| **P0-4a** `read_file` 任意读 | 发 `C:/Windows/win.ini` | ✅ **已修复**。`路径不在允许读取的范围内（C:/Windows/win.ini）` |
| **P0-4a** 白名单不误伤 | 发 `C:/Users/.../SOUL.md` | ✅ **正常读取**（白名单内放行） |
| **P1-9** 拒绝通路 | 未复测 | ⚠️ **未验证**（见 §4） |
| 主按钮对比度 / 归档图基线 | 未复测 | ⚠️ **未验证**；归档图本轮又落后 2 个提交（见 §4） |
| 误报修正 | —— | 本轮无新增误报修正 |

**判定要点**：`git diff --stat 2b16d5d..HEAD -- aetherd/src/perm.rs aetherd/src/router.rs` **为空** —— 权限闸门与路由决策一行未动，所以上轮的语义修复在源码层面必然保持；实测 4 条抽样全部符合预期。

> ⚠️ 但 **`tools.rs` 变了**（新增 2 个工具）。工具注册表是权限模型的输入，所以"闸门没改"不等于"安全边界没变"。本轮的 P1-1 正是从这个缺口进来的。

---

## 2. 本轮新发现

### P1-1 · 剪贴板 IPC 端点完全绕过权限闸门

**位置**：`aetherd/src/server.rs:239-250`

```rust
Request::ClipboardSet { text } => {
    let _ = gate.audit("clipboard_set", Level::L0, "", "allowed");
    match crate::clipboard::set(&text) { ... }        // ← 直接调，没有 judge
}
Request::ClipboardGet => {
    let _ = gate.audit("clipboard_get", Level::L1, "", "allowed");
    vec![Response::ClipboardText { text: crate::clipboard::get() }]   // ← 直接读
}
```

`handle_request` 里其他分支都有门槛：`RegisterUi` 验密钥、`ToolCall` 走 `Gate::judge` 且 L2+ 要求令牌 + 已注册 UI 通道。**只有剪贴板这两个分支既不验密钥、也不要求注册、也不签发令牌** —— 只补了一行审计。

**实证输出**（同一次 `aetherd serve` 会话，未注册连接）：

```
A. 未注册 · clipboard_get   → clipboard_text {'text': ''}
B. 未注册 · clipboard_set   → clipboard_written {'bytes': 15}
C. 未注册 · 回读           → clipboard_text {'text': 'PWNED-BY-UNAUTH'}
```

对照组（同一次运行，同一端点）：

```
E. 未注册 · L3 无令牌       → error code=403 'L2+ 操作（install_disk）必须由已注册的 UI 通道发起'
F. 未注册 · L3 伪造令牌     → error code=403 '确认令牌兑现失败：本连接未注册为 UI 通道'
```

**同一份代码里，权限模型对工具调用生效、对剪贴板完全不生效。**

**影响**：
1. 本机任意进程（无需密钥文件、无需注册）可**读取剪贴板全文** —— 而剪贴板按项目自己的定位就是"常被用来复制密码"（`clipboard.rs:7`）。
2. 可**覆写剪贴板** —— 经典的剪贴板劫持：用户复制钱包地址/一条命令，攻击者进程替换成自己的。用户粘贴时拿到的是攻击者的内容。

**定 P1 的理由**：源码直读即可确认，实测无任何门槛，触发条件在日常使用中自然发生（用户复制密码 → 任意本机进程读取）。**不定 P0** 的理由：危害止于本机（监听 127.0.0.1，非远程可达），且本机进程本来就能通过其它途径（如读 `/proc`）获得部分信息；但它**显著宽于** `clipboard.rs:9-10` 注释自述的边界 —— 注释把剪贴板类比为 `ui.key`（"能连上 7311 的本机进程仍可读写"），而 `ui.key` 至少需要读 0600 文件的权限，剪贴板连这个都不需要。**这是"设计意图"与"实际安全边界"的差值**。

**修复方向**：把 `ClipboardGet` / `ClipboardSet` 纳入同一套模型。最小改动是要求 `is_ui`（与 L2+ 令牌一致）；若希望非 UI 客户端（如 ops）也能用，则给剪贴板单独定义等级并走 `Gate::judge`。

---

### P2-1 · `clipboard_write` 分级为 L0（只读），实为写操作

**位置**：`aetherd/src/tools.rs:58`

```rust
Tool {
    name: "clipboard_write",
    level: Level::L0,          // ← 写操作标成"只读"
    ...
}
```

而 `clipboard_read` 是 `L1`（`tools.rs:43`）。**读比写级别高，方向反了。**

**实证**（无需令牌）：

```
K. clipboard_write 工具     → tool_result ok=True out='已写入剪贴板（13 字节）'
L. clipboard_read 回读      → tool_result ok=True out='AI-WROTE-THIS'
```

L0 在 `Gate::judge` 里直接放行，无确认、无冷却、无审计分级。所以 AI（或被提示注入的 AI）可静默写入最多 64KB 任意文本。

**当前无可利用链（已读代码确认）**：AI 写的是 aetherd 进程内的 `static CLIPBOARD`，而合成器粘贴读的是自己的 `desktop.clipboard`；`sync_clipboard_to_daemon`（`main.rs:2237`）**只有 compositor → aetherd 单向**，没有回同步。所以"AI 篡改 → 用户粘贴"这条路**现在不通**。

**定 P2 的理由**：分级本身错误（L0 的语义是"只读"，写剪贴板不是只读），且与 `clipboard_read` 的等级不对称；一旦将来接上反向同步，它立刻变成"AI 静默篡改用户剪贴板"的 P1。**现在就改，比将来发现便宜。**

---

### P2-2 · 审计日志没有大小上限（服务日志有，安全日志没有）

**位置**：`aetherd/src/perm.rs:125-135`

```rust
pub fn audit(&self, tool: &str, level: Level, args: &str, verdict: &str) -> std::io::Result<()> {
    ...
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.audit_path)?;
    writeln!(f, "{ts}\t{level}\t{tool}\t{args}\t{verdict}")
}
```

只 append，**无 `metadata().len()` 检查、无轮转**。

对比本轮新增的 `aether-init/src/logtee.rs`：服务日志有 `MAX_LOG_BYTES = 8MB`，且新增了**运行期轮转**（`rotate_in`，带 3 个单测）。**安全关键的审计日志反而没有这个保护。**

**放大因素**：所有 `audit()` 调用点都是 `let _ = gate.audit(...)` 忽略返回值（`server.rs:241`、`248`，以及工具路径）。所以磁盘写满后：审计静默停止 → **无任何告警**，而系统继续正常运行。一个"看起来在工作"的审计系统比一个明显坏掉的更危险。

**定 P2 的理由**：长期运行必然触发（aetherd 是常驻守护进程，`restart` 默认 true，不重启就不会清盘），后果是安全机制静默蒸发。

**修复方向**：把 `rotate_in` 从 `logtee` 提取成共享工具，`audit()` 在追加前检查大小；写失败要 `eprintln!` 而不是静默。

---

### P2-3 · 合成器推送剪贴板时不对 aetherd 做身份校验

**位置**：`aether-compositor/src/main.rs:2237-2250`

```rust
fn sync_clipboard_to_daemon(text: String) {
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<()> {
            let addr: std::net::SocketAddr = ([127, 0, 0, 1], aether_ipc::DEFAULT_PORT).into();
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
            stream.write_all(aether_ipc::encode(&Request::ClipboardSet { text }).as_bytes())?;
            ...
```

明文 TCP 连 `127.0.0.1:7311`，**不注册、不握手、不验密钥、不校验对端身份**，直接把剪贴板内容发出去。

**影响**：aetherd 不在的时间窗内（崩溃后重启的间隔、手动停服），任何本机进程都能绑定 7311 冒充 aetherd，**收到合成器推送的剪贴板内容**。用户的 Ctrl+Shift+C 会直接把内容送给冒充者。

**缓解**：`aetherd.json` 未指定 `restart`，而 `unit.rs:122` 的 `default_restart()` 返回 `true` → aetherd 会自动重启，窗口较短。

**定 P2 的理由**：是真实可实施的攻击（不需要 root、不需要密钥），但需要 aetherd 不在的时机；与 P1-1 同源 —— **整条 IPC 链缺少对端认证**。修 P1-1 时一并考虑（例如引入共享密钥后，合成器推送也带上）。

---

### P3-1 · 日志格式 bug：`LL3 危险`

**位置**：`aetherd/src/server.rs:219`

```rust
eprintln!("[aetherd] 工具 {tool} 需 L{level} 确认，已下发确认请求");
```

`level` 是 `Level` 枚举，而 `perm.rs:24-34` 的 `Display` 已经输出 `"L3 危险"`，于是拼成 **`LL3 危险`**。

**实证**（本轮 stderr 原样）：

```
[aetherd] 工具 install_disk 需 LL3 危险 确认，已下发确认请求
```

**同一文件里两条路径不一致**：`server.rs:322`（`handle_chat` 路径）用的是 `p.level`（`u8`），打印正常。所以走 LLM 的确认请求日志是对的、走直接 ToolCall 的是错的 —— 排查时容易误判。

**修复方向**：改成 `L{level:?}` 或去掉 `L` 前缀，两处统一。

---

### P3-2 · 无选区时"复制"会把整屏推到剪贴板

**位置**：`aether-compositor/src/main.rs:2254`（`clipboard_copy`）→ `term.rs:163-167`

```rust
pub fn selection_text(&self) -> String {
    let Some(sel) = self.sel else {
        // 没有选区 = 复制全部可见内容
        return self.screen.to_lines().join("\n");
    };
```

设计意图是"`Ctrl+Shift+C` 在没拖选时也能用"，但退化目标是**整个屏幕**。终端里出现过 `cat /etc/shadow` 的输出、`passwd` 提示、环境变量打印时，整屏会被推给 aetherd 并常驻其内存（见 P3-4）。

**定 P3 的理由**：需要用户主动按复制键，且 aetherd 侧已标 `sensitive_output`（强制本地推理）；但"复制"的语义应是"复制我选的东西"，整屏是超范围。

---

### P3-3 · `Pty::write_all` 在非阻塞主端上静默丢数据

**位置**：`aether-compositor/src/pty.rs:109-124`

```rust
pub fn write_all(&mut self, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let n = unsafe { libc::write(...) };
        if n <= 0 { break; }        // ← EAGAIN 也走这里，直接放弃剩余字节
        off += n as usize;
    }
}
```

主端在 `spawn` 里被设成 `O_NONBLOCK`（`pty.rs:80-81`）。shell 来不及消费时 `write` 返回 `EAGAIN`，函数**静默 break** —— 用户键入的字符消失，无重试、无提示。

**定 P3 的理由**：PTY 写缓冲通常远大于单次按键，正常打字触发不到；但**粘贴大段文本**（`paste_bytes` 可产生几十 KB）会显著提高命中概率，表现是"粘贴少了一截"，很难排查。

---

### P3-4 · 剪贴板内容无生命周期管理

**位置**：`aetherd/src/clipboard.rs:17`

```rust
static CLIPBOARD: Mutex<String> = Mutex::new(String::new());
```

常驻进程内存，**无超时清空、无登出清理、无上限外的淘汰**。aetherd 以 root 运行（`platform/overlay/etc/aether/services/*.json` 均无 `uid`/`gid` 字段，`unit.rs` 也没有该字段），所以用户复制的密码会一直留在 root 进程的地址空间里。

**定 P3 的理由**：需要 root 级手段才能 dump 进程内存，实际危害低；但与项目自己"剪贴板 = 敏感数据"的定位不一致 —— 既然为它设了 `sensitive_output` 和审计，就该有清理。

---

### P3-5 · 测试组织：剪贴板断言塞进了 UI 注册测试

**位置**：`aether-ipc/src/lib.rs:262`（`ui_registration_and_cancel_roundtrip`）

函数体里被插入了 4 组剪贴板编解码断言（`ClipboardSet` / `ClipboardGet` / `ClipboardText` / `ClipboardWritten`）。函数名与内容不符，失败时的报错信息会指向错误的方向。

**修复方向**：拆成独立的 `clipboard_roundtrip` 测试。

---

## 3. 本轮的正面结论

不只报问题 —— 以下是有实证或严格验算支撑的正面结论：

### 3.1 VT 解析器的索引运算全部有界（逐条验算）

终端解析器处理的是**完全不可信的输入**（PTY 输出的任意字节，包括 `cat /dev/urandom`），而 `compositor` 是 `essential: true` + `restart: false`（`compositor.json`）—— **一次 panic 就等于桌面永久死掉，需要手动重启**。所以这类越界值得逐条算。

逐条验算结果（`vt.rs`）：

| 位置 | 运算 | 边界结论 |
|---|---|---|
| `put_char` (287-298) | `cur_y*cols+cur_x`、`idx+1` | `cur_x ≤ cols-1`（300-302 夹取）；`width==2` 时前置条件保证 `cur_x+2 ≤ cols` → 安全 |
| `scroll_up` (316-326) | `copy_within(n*cols.., 0)` | `n ≤ rows` → `n*cols ≤ len` → 安全 |
| `reverse_index` (328-338) | `rows - 1` | `Screen::new`/`resize` 均 `rows.max(1)` → 无下溢 |
| `dispatch_csi` L/M/P/X (390-425) | 多处 `copy_within` + 索引 | 每处的 `n` 都先 `.min(...)`，src/dest 长度相等且上界 ≤ `len` → 安全 |
| `erase_display` / `erase_line` (432-466) | `cur_y*cols+cur_x` | `cur_x ≤ cols-1`、`cur_y ≤ rows-1` → 安全 |
| `param()` (340-345) | `v as usize`，v 是 `u16` | 参数解析处 `v.min(65535) as u16`（228）→ 最大 65535，`cur_y + n` 在 64 位 usize 下不溢出 |

**未发现可被 PTY 输出触发的越界 panic。** 渲染侧也做了双重夹取（`draw.rs:2107-2108` 的 `rows.min(...)` / `cols.min(...)`）。

### 3.2 AI 的 Action 通道无法驱动终端 —— "AI 在 root shell 里执行命令"这条链不成立

这是本轮最需要确认的一条。前提事实：

- `compositor` 以 **root** 运行（服务定义无 `uid`/`gid`，`unit.rs` 无降权字段）
- 终端 PTY 直接 `exec /bin/sh`（`pty.rs:23`）→ **root shell**
- 所以"能往 PTY 写字节" == "能在 root shell 里执行任意命令"

检查 `apply_action`（`main.rs:999-1033`）—— AI 的 `Action` 通道**只支持三个动作**：

```rust
match name {
    "layout_set" => ...      // 改布局
    "open_app"   => ...      // 开窗口（含终端）
    "close_active" => ...    // 关窗口
    _ => None,               // 其余一律忽略
}
```

**没有**任何向 `term.write()` / PTY 写入的路径。`term.rs` 的 `write()` 只被键盘输入和粘贴调用。所以：

> **AI（或提示注入）无法通过 Action 通道在终端里执行命令。** 它能做的最多是"打开一个终端窗口"，而窗口里的命令仍需用户亲自输入。

这条边界目前是干净的，且它是本轮新增 PTY 之后**最容易被无意打破**的地方（例如将来加一个"AI 帮你跑条命令"的 action）。建议在 `apply_action` 上留一条注释把这个边界钉住。

### 3.3 PTY 用了正确的 fork 方式

`pty.rs:52-76` 用 `Command + pre_exec` 而不是手写 `fork`，注释也说明了理由（合成器是多线程进程，`fork` 后只能调 async-signal-safe 函数）。`setsid` + `TIOCSCTTY` 的顺序正确，`ptsname_r` 而非 `ptsname`（避免静态缓冲竞态），`Drop` 里 `SIGHUP` + `kill` + `wait` 三重兜底防孤儿进程。这是一份写得克制的系统调用代码 —— 作者在文件头写了"刻意保持薄：逻辑越少，看不见的部分越少"，这个判断是对的。

### 3.4 救援模式从"假死"变成"有出路"

`aether-init/src/main.rs` 把原来的 `loop { sleep(3600) }` 换成 `rescue_console()`：打印**可操作的原因**（含"单个文件写坏只会被跳过，全部不可用才会进到这里"这种真正有用的诊断提示）、给出排查路径、并开一个 `/dev/console` shell。`#[cfg]` 分支处理正确（非 Linux 不启 shell，只保活）。

`logtee` 的运行期轮转同理 —— 原来的"只在打开时检查大小"对一个连续运行数周的服务等于没有上限。两处都是**从"看起来做了"到"真的做了"**的修正，且都配了单测。

### 3.5 其他

- `devpts` 挂载前先检查 `/dev/pts/ptmx` 是否存在，避免重复挂载产生 `EBUSY` **假告警** —— 这种"避免日志说谎"的细节做得对。
- `aether-init/src/manager.rs` 把 `unsafe { libc::WIFEXITED }` 改成安全调用（该宏在 Linux 上已是安全函数），`aether-init/src/ipc.rs` 给 `TcpListener` 加 `#[cfg(not(target_os = "linux"))]` —— 都是正确的清理。

---

## 4. 未验证清单（不计入上述结论）

| 项 | 原因 |
|---|---|
| 终端在真机（Linux VM）上的行为 | `pty.rs` 在 Windows 上无法运行，本轮只做到编译检查 + 源码审读 |
| 终端拖选复制 / `Ctrl+Shift+C` / `Ctrl+Shift+V` 的实际交互 | `--shot` 只能出静态帧，**测不了点击与拖拽**；需 QEMU/VMware 实机验证 |
| **P1-9 拒绝通路回归** | 本轮未跑 `confirm_cancel` 复现（§1 表中标 ⚠️） |
| `input.rs`（+306）/ `draw.rs`（+1038）/ `main.rs`（+1173） | 未逐行审；只做了 panic 风险点扫描（未发现可被外部输入触发的 `unwrap`/越界，`main.rs:225` 与 `602/623` 的 `unwrap` 分别有 `is_some` 保护与长度约定） |
| **归档图与 HEAD 的像素一致性** | `docs/host-ui-desktop.png` / `host-ui-threecol.png` 停在 `b01dbf9`（09-27 14:02），HEAD 是 `998ab9b`（18:19），**落后 2 个提交**。中间 `1b83a70` 改了 compositor 的剪贴板同步。是否影响像素**未跑比对**（本轮改动以逻辑为主，视觉面未见明显变化，但**不能据此断言零回归** —— 上轮正是栽在"用过期归档图当基线"上） |
| 主按钮对比度 | 未复测 |
| 审计日志 `denied` 计数 | 未复测 |

---

## 5. 建议修复顺序

| 优先级 | 项 | 位置 | 理由 |
|---|---|---|---|
| **P1** | 剪贴板 IPC 端点纳入权限模型 | `server.rs:239-250` | 唯一"零门槛读用户数据 + 写用户环境"的端点；实测可复现 |
| **P2** | `clipboard_write` 提级（L0 → L1） | `tools.rs:58` | 分级语义错误，且与 `clipboard_read` 不对称；现在改最便宜 |
| **P2** | 审计日志加轮转 + 写失败告警 | `perm.rs:125-135` | 静默失效的安全机制等于没有 |
| **P2** | 合成器推送剪贴板时校验对端 | `main.rs:2237` | 端口劫持；与 P1 同源，建议一并设计 |
| **P3** | 修 `LL3 危险` 双重前缀，两处日志统一 | `server.rs:219` / `322` | 一行修，影响排查准确性 |
| **P3** | 收敛无选区时的复制范围 | `term.rs:163-167` | 超范围复制敏感屏幕内容 |
| **P3** | `Pty::write_all` 对 `EAGAIN` 做短重试 | `pty.rs:109-124` | 大段粘贴可能丢字节 |
| **P3** | 剪贴板内容加超时清理 | `clipboard.rs:17` | 与"敏感数据"定位对齐 |
| **P3** | 拆出独立的剪贴板编解码测试 | `aether-ipc/src/lib.rs:262` | 测试可读性 |

**修复后必须回归**：
1. `cargo test --workspace --offline`（应 ≥185 项）
2. `scripts/e2e-permission-confirm.py`（应 ≥17 项全 PASS）
3. **重建 `aetherd` 二进制后再跑** —— `cargo test` 不更新 bin target，否则测的是旧代码（本轮已踩）

---

## 6. 一句话总结

**工程质量明显在涨（185 项测试、解析器边界严谨、PTY 与救援模式写得克制），但安全边界这轮没有跟着涨：新增的剪贴板通道把"零门槛读写用户数据"这个口子开在了唯一有密钥保护的 IPC 端点旁边 —— 权限模型守住了 L3 的危险操作，却没守住用户刚复制的密码。**

核心三问：
1. **为什么工具调用有闸门、剪贴板没有？** —— 因为剪贴板走的是 `Request` 变体而不是 `ToolCall`，而 `handle_request` 的权限检查是**逐分支手写**的，新增分支容易漏。这是结构问题，不是疏忽。
2. **审计日志为什么没有服务日志的轮转？** —— 两套日志各写各的，`logtee` 的 `rotate_in` 没有被复用。
3. **AI 能往 root shell 写命令吗？** —— **不能**（`apply_action` 只有 3 个动作）。这条边界目前是干净的，但它是本轮新增 PTY 之后最容易被无意打破的一处，值得加注释钉死。
