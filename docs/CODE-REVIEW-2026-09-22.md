# AetherOS 代码审查报告 · 第二轮（增量）

日期：2026-09-22
审查范围：`a0f1268`（第一轮修复）之后的 6 个提交 —— `205a7a3` `2381f99` `350c26a` `c486d41` `ce32b0f` `fc6dcfd`
增量规模：约 2446 行改动，集中在 `aetherd`（权限链路）、`aether-compositor`（UI 重绘）、`aether-ipc`（协议扩展）
方法：增量源码逐文件阅读 + `cargo test --workspace` 全量回归 + 对 `aetherd serve`（127.0.0.1:7311）做对抗性 IPC 复现 + **双假 LLM 端点（local/cloud）端到端验证路由外泄链** + 审计日志取证 + 令牌/路由/工具注册表的交叉核对

> 环境说明：本次复现打在一个 **2026-09-20 23:30 构建**的 `aetherd.exe` 上，HEAD 提交时间为 09-20 23:36 —— 二进制与 HEAD 内容一致（同一轮工作的产物）。原残留进程（PID 33344）已在补齐端到端验证时终止，随后以显式环境变量重启新实例（云端 key 指向本地假端点）。
> 诚实边界：下文凡标注「**未验证**」的条目，均为源码级推断，未经实机复现，不与已证结论混列。

---

## 0. 构建与测试基线

| 检查项 | 结果 |
|---|---|
| `cargo test --workspace` | **65 项全过**（4 + 13 + 9 + 4 + 0 + 0 + 35 + 0） |
| 上一轮唯一确定性失败项 `supervision_restarts_and_backs_off` | 已修（改为 `wait_for_state` 轮询） |
| `scripts/e2e-permission-confirm.py` | 13 项断言全 PASS |
| `scripts/shot-diff.py` | 存在；**本轮未复跑**（见 §4） |

测试从 43 项增至 65 项，全部通过。上一轮的确定性失败已消除。

---

## 1. 上一轮 37 项的复核结论

### 1.1 已确认修复（实证）

| 原编号 | 问题 | 复核方式 | 结论 |
|---|---|---|---|
| P0-1 | `install_disk` 错标 L1，整盘擦除零确认 | 实机请求无令牌 → 返回 `needs_confirmation`，`level=3`，`echo_required="/dev/vda"`，`consequence` 完整 | ✅ 已修 |
| P0-2 | 工具失败谎报 `ok:true` | 实机：工具失败返回 `ok:false`；`server.rs` 按 `ExecOutcome` 分流 | ✅ 已修 |
| P0-3 | 出厂镜像 `AETHER_BIND=0.0.0.0` | `platform/overlay/init` 已移除该 export；`server.rs` 注释确认 | ✅ 已修 |
| P1-2 | `essential` 字段无读取点 | 已接入 `aether-ops/src/monitor.rs:72` 崩溃升级告警，并有 `essential_crash_escalates` 测试 | ✅ 已修 |
| P1-3 | 两套监督者对 `restart:false` 处理矛盾 | `ServiceStatus` 新增 `restart` 字段统一 | ✅ 已修 |
| P1-4 | 自动重启丢日志管道 | 新增 `spawn_with_logs(spec)` 统一原语，首次启动与重启共用 | ✅ 已修 |
| P1-5 | spawn 失败形成 200ms 重试风暴 | 新增 `spawn_failure_backs_off` 测试 | ✅ 已修 |
| P2-2 | Monocle 布局空操作 | `layout.rs` 已实现 | ✅ 已修 |
| P2-3 | 拖拽索引越界 panic | 改为 `desktop.wins.get_mut(i)` | ✅ 已修 |
| P2-4 | 安装向导 Done 后可二次擦盘 | `begin_install` 加防护 | ✅ 已修 |
| P2-5 | LLM 多工具调用致下一轮 400 | 逐 `tool_call` 补 tool 消息 | ✅ 已修 |
| P2-6 | `read_line` 无长度上限 | 加了上限，但**实现有误** —— 见 P2-17 | ⚠️ 部分修复 |
| P2-7 | 审计失败被静默吞掉 | 审计失败前置于输出 + stderr | ✅ 已修 |
| P2-11 | 监督重启测试确定性失败 | 轮询化 | ✅ 已修 |
| P3 | 各类死代码 / 魔数 | 收口进 `theme` 令牌模块 | ✅ 大部分已修 |

### 1.2 复核中确认的一处措辞歧义（非缺陷）

- **`draw_cursor` 的归属**：上一轮 P3-11 把它列在"按平台条件收敛"条目下（要求加平台条件属性，而非断言它从未被调用）。核实结果：调用点 `main.rs:437` 位于 `run_fbdev()`（`#[cfg(target_os = "linux")]`，起始于 `main.rs:67`）之内 —— Linux fbdev 无硬件光标，软件绘制**必要且正确**；Windows 侧由 minifb 提供光标，不会重复绘制。上一轮的**建议已正确落地**：`draw.rs:1109` 现为 `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]`。此处无需修改。

---

## 2. 本轮新发现

### P0-4 任意文件读取 × 必定上云：一条完整的"读文件 → 送云端"外泄链

**这是本轮最严重的问题，且不在上一轮报告覆盖范围内。**

三处独立缺陷串成一条链：

#### (a) `read_file` 是 L0 工具，无路径白名单、无任何确认

- 位置：`aetherd/src/tools.rs:53-64`（`level: Level::L0`）、`aetherd/src/tools.rs:206-213`（`tool_read_file`）
  ```rust
  fn tool_read_file(args: &Value, _ctx: &mut ToolCtx) -> Result<String> {
      let Some(path) = args.get("path").and_then(|v| v.as_str()) else { bail!("缺少 path 参数"); };
      let content = std::fs::read_to_string(path)?;
      Ok(content.chars().take(4000).collect())
  }
  ```
  参数只有一句"文件绝对路径"，**没有前缀约束、没有黑名单、没有符号链接检查**。
- L0 < `auto_approve_below`(L2)，因此 `Gate::judge` 永远返回 `Allowed` —— **不进确认流程**。
- **实证**：单连接、不带任何令牌，`read_file("C:/Windows/win.ini")` 直接返回文件内容：
  ```
  C:/Windows/win.ini
    -> type=tool_result ok=True output='; for 16-bit app support\r\n[fonts]\r\n[extensions]...'
  ```
- 目标平台后果更重：`aether-init` 是 PID 1（root），服务定义 `platform/overlay/etc/aether/services/aetherd.json` 只有 `{"name":"aetherd","deps":["network"],"essential":true}` —— **全链路无 uid/gid 降权**，`manager.rs` 中亦无 `setuid`。所以 aetherd 以 **root** 运行，`read_file` 可读 `/etc/shadow`、`~/.ssh/id_rsa`、`/proc/self/environ`（内含 `AETHER_API_KEY`）等一切。

#### (b) 混合路由用 `history_len` 作复杂度信号，工具调用两轮后**必然**上云

- 位置：`aetherd/src/main.rs:97-106`、`aetherd/src/router.rs:39-60`
  ```rust
  let task = router::Task {
      text: user_text,              // ← 只看用户原文，工具结果不参与
      history_len: messages.len(),  // ← 消息条数被当作"复杂度"
      ...
  };
  ```
  ```rust
  if complex || task.history_len >= 6 || task.text.chars().count() > 120 {
      return Channel::Cloud;
  }
  ```
- 隐私关键词闸门（`PRIVACY_HINTS`）**只检查用户原文**。用户说"帮我分析一下这台机器"就足够触发 `COMPLEX_HINTS` 的「分析」→ 云端；即便没触发，`messages.len()` 也会长：
  - 初始 `[system, user]` = 2
  - 第 1 轮工具调用后 = 4
  - 第 2 轮工具调用后 = 6 → **第 3 轮起必然 `history_len >= 6` → Cloud**
- 而 `llm::complete` 每轮上传**完整** `messages`（`llm.rs:37` `"messages": messages`），包括此前所有 tool 消息。
- **结论**：一个多步工具任务，前两轮读到的文件内容会在第 3 轮被整体发往 `AETHER_API_BASE`（默认 `https://open.bigmodel.cn/api/paas/v4`）。用户全程看不到任何提示 —— `channel` 只回传给合成器顶栏做三态渲染，**不上报"内容已离机"**。

#### (c) **端到端实证**：链条已闭合

用两个假 LLM 端点（local :18101 / cloud :18102，均记录收到的完整 `messages`）替换真实端点，aetherd 以
`AETHER_LOCAL_URL=http://127.0.0.1:18101/v1` + `AETHER_API_BASE=http://127.0.0.1:18102/v1` + `AETHER_API_KEY=dummy` 启动。
用户输入刻意选为**不含隐私关键词、不含复杂信号、仅 13 字**的一句：「帮我看一下机器上都有什么」—— 即路由只能由 `history_len` 驱动。
两个 canary 文件内容为 `CANARY_ROUND1_8f3a91_SECRET` / `CANARY_ROUND2_4b7c22_SECRET`。

实测输出：

```
  [local 第 1 次] messages=2 条, 携带 canary: 无
  [local 第 2 次] messages=4 条, 携带 canary: ['CANARY_ROUND1_8f3a91_SECRET']
  [cloud 第 1 次] messages=6 条, 携带 canary: ['CANARY_ROUND1_8f3a91_SECRET', 'CANARY_ROUND2_4b7c22_SECRET']

[aetherd 输出]
  [aetherd] 通道: Local · 模型: mock-local
  [aetherd] 工具调用: read_file {"path":".../aether-canary/r1.txt"}
  [aetherd] 通道: Local · 模型: mock-local
  [aetherd] 工具调用: read_file {"path":".../aether-canary/r2.txt"}
  [aetherd] 通道: Cloud · 模型: mock-cloud

local 端点收到 2 轮请求
cloud 端点收到 1 轮请求
cloud 端点收到的 messages 中出现的 canary：['CANARY_ROUND1_8f3a91_SECRET', 'CANARY_ROUND2_4b7c22_SECRET']
```

**关键点**：
- 用户那句 13 字的话**没有任何隐私或复杂信号**，路由从 Local 跳到 Cloud 的唯一原因就是 `messages.len()` 从 4 变成 6。
- **第 3 轮的请求里同时带着前两轮读到的两份文件内容** —— 不是"可能带"，是逐字节到达了 `AETHER_API_BASE` 指向的端点。
- 前提条件：需配置 `AETHER_API_KEY`（否则 `router` 第 3 步 `!cloud_available` 会永远返回 Local）。这是**唯一**的前置条件 —— 而配置云端 key 正是这个"混合推理"卖点的推荐用法。

#### (d) 攻击链闭合

```
提示词注入（来自被读文件/网页/历史消息）
  → 模型输出 tool_call: read_file({"path": "/etc/shadow"})   # L0，零确认，root 可读
  → 内容进 messages（tool 消息）
  → 第 3 轮路由到 Cloud（history_len>=6 或原文含"分析"）
  → 完整 messages 上传至云端端点
```

**修复方向（三条独立生效，建议全做）**：

1. **`read_file` 加路径约束**：限定白名单根（如 `/home`、`/etc/aether`、`/var/log/aether`），拒绝 `/proc`、`/sys`、`/dev`、符号链接与 `..`；或把等级提到 L1/L2 使其进入确认流程。
2. **路由不看 `history_len`**：改用"本轮任务本身是否需要长上下文"的显式信号（工具轮次、结果累计字节数），并让**工具结果的敏感度**参与决策 —— 例如读过非白名单文件后强制 `local_only`。
3. **离机可见**：`ChatChunk` 增加"本轮含工具结果已上传云端"的显式标记，或至少在顶栏三态之外给出一次性提示。当前"隐私内容不上云"的承诺在**多轮工具场景下不成立**。

> 严重级别定 P0 的理由：这不是理论风险，而是**已端到端实证**的确定性行为 —— 一句 13 字、无任何隐私信号的用户输入，两次 `read_file`，第 3 轮两份文件内容就完整到达了云端端点。触发条件（一次工具调用 + 两轮对话）在日常使用中自然发生，无需任何特殊构造。且它与 `install_disk` 的 L3 保护无关 —— 数据外泄不需要危险操作权限。

---

### P1-8 确认令牌不绑定确认者：任何 IPC 连接都能自助兑现自己触发的令牌

- 位置：`aetherd/src/perm.rs:94-138`（`Approvals`）、`aetherd/src/server.rs:93-110`
- 设计意图（`perm.rs:96-99` 注释）：*"令牌只经 IPC 发给 UI 客户端，绝不进入 LLM 上下文，因此模型无法自我授权。"*
- 实际情况：`issue()` 把令牌写进**发起请求那条连接**的响应流；`redeem()` **不校验是谁在兑现** —— 不绑连接、不绑 session、不需要任何第二方在场证明。
- **实证**（单条 TCP 连接，全程零人工介入）：
  ```
  [连接] 127.0.0.1:7311  fd=756  —— 全程同一条连接，无任何人工介入

  [第1步] ToolCall(install_disk) 不带 approval
    <- {"type":"needs_confirmation","payload":{...,"level":3,
        "echo_required":"/dev/vda","token":"be024e0b3d43d3a0aa8774a52254628e"}}

  [第2步] 同一条连接内，带 approval=上一步拿到的令牌，原样重发
    <- {"type":"tool_result","payload":{"tool":"install_disk","ok":false,
        "output":"[工具错误] 安装器运行失败: 系统找不到指定的路径。 (os error 3)"}}

  [结论] 令牌在同一连接内被兑现，权限闸门放行。
  ```
  注意第 2 步的错误来自**工具体内部**（安装器不存在）—— 说明请求已穿过权限闸门。在 Linux 目标机上 `/dev/vda` 存在，这一步就是真的擦盘。
- **评价**：
  - 对**纯 LLM 路径**，设计目标成立 —— `agent_run` 调 `execute(..., false)` 从不带令牌，模型确实拿不到。这点作者推导正确。
  - 但机制的**实际安全边界远窄于其陈述**：它只防"模型"，不防"任何能连上 7311 的进程"。结合已记录的 P2-1（IPC 无认证），一次性令牌对本地攻击者提供的额外保护约为**零**。
  - `docs/ai-permissions.md` 把它作为"注入防线"呈现，读者会以为它提供了"人在回路"的保证 —— 实际只提供了"合成器是诚实的"这一假设。
- **审计日志侧的佐证**（`D:\var\log\aether\aether-audit.log`，即 Windows 下 `/var/log/aether/` 解析到当前盘根）：
  ```
  1790076820	L3 危险	install_disk	{"confirm":"/dev/vda","disk":"/dev/vda"}	needs_confirmation
  1790076820	L3 危险	install_disk	{"confirm":"/dev/vda","disk":"/dev/vda"}	allowed
  ```
  `1790076820` = 2026-09-22 19:33:40，正是本次自助授权复现的时刻。**同秒内 `needs_confirmation` 紧跟 `allowed`** —— 这就是一次完整的"自助确认"在审计日志里的样子。而一次**真人**确认（如 19:27 那批 e2e 断言）在日志里留下的是**完全相同**的两行。见 P1-9。
- **修复方向**：令牌绑定**确认方身份**而非请求方 —— 例如服务端为 UI 客户端分配会话身份，`NeedsConfirmation` 只投递给已注册的 UI 通道，且**发起 ToolCall 的连接不得兑现自己触发的令牌**；或在确认时要求带一个只有交互式客户端持有的凭证。最低成本的临时措施：`issue()` 记录发起连接 id，`redeem()` 校验兑现连接 ≠ 发起连接（能挡住本次复现，但挡不住第二个本地进程 —— 需配合 IPC 认证才彻底）。

---

### P1-9 「拒绝」在整个系统中没有表示，审计无法区分"人工确认"与"令牌自助兑现"

两个缺陷叠加，导致权限系统的核心审计问题 —— **"这次放行是用户批准的吗？"** —— 无法回答。

#### (a) `Verdict::Denied` 不可达，「拒绝」无协议表示

- `perm.rs:54` 声明了 `Denied(String)`，`tools.rs:155` 映射为 `"denied"` 字符串，`tools.rs:182` 有 `bail!("DENIED: {reason}")` 分支 —— 但**全仓没有任何一处构造 `Verdict::Denied`**（`grep -rn "Denied" --include=*.rs .` 只有这 4 处：1 处声明 + 1 处 match 臂 + 1 处字符串 + 1 处 `#[allow(dead_code)]` 注释）。`Gate::judge` 只返回 `Allowed` 或 `NeedsConfirmation`。
- `aetherd` 与 `aether-ipc` 中**不存在** `revoke` / `cancel` / 任何表示"用户拒绝"的消息类型。
- 实证（审计日志 29 条记录，全量裁决分布）：
  ```
  denied 记录数 = 0
        17 allowed
        12 needs_confirmation
  ```
  **`denied` 从未出现过一次。**

#### (b) UI 的拒绝路径不回传服务端

- `aether-compositor/src/main.rs:165-170`（Esc）与 `main.rs:275-280`（点"拒绝"）都只做 `confirm.take()` + 本地 toast，**不发送任何 IPC 消息**。合成器显示的"已拒绝「install_disk」"纯粹是本地 UI 状态。
- 后果：
  1. 令牌在服务端继续存活最长 300 秒。"用后即废"只覆盖"用过"，不覆盖"明确拒绝过"。
  2. 用户拒绝这件事在服务端**不留痕**。事后审计只能看到 `needs_confirmation`，然后要么什么都没有（拒绝），要么 `allowed`（放行）—— 而后者与令牌自助兑现的记录**逐字节相同**（见 P1-8 的日志片段）。

#### 合并结论

`docs/ai-permissions.md` 定义了 `Denied` 这一档语义，实现里也留了分支，但**整条拒绝通路从 UI 到协议到裁决到审计，没有一环是通的**。当前系统只有两种可观测状态："要求确认"和"已放行"。这对一个把"用户知情同意"作为核心承诺的权限模型来说，是**结构性缺口**，不是实现疏漏。

- **修复方向**：
  1. 协议加 `Request::ConfirmCancel { token }`（或 `Response::ConfirmRejected`），UI 拒绝时回传；
  2. `Approvals::revoke(token)` + 落一条 `denied_by_user` 审计；
  3. 审计行区分确认来源（`confirmed_by=ui` / `redeemed_by=connection#N`），使自助兑现可被事后识别。

---

### P2-17 `take(MAX_LINE_BYTES)` 是整连接累计上限，与注释语义不符

- 位置：`aetherd/src/server.rs:66-68`
  ```rust
  // take() 限制单次 read_line 的读取量：超长行不会撑爆内存
  let mut reader = BufReader::new(stream.try_clone()?).take(MAX_LINE_BYTES);
  ```
- `std::io::Read::take` 是**整个 reader 生命周期**的累计字节上限，不是"单次 read_line 上限"。注释与行为不一致。
- **实证**：
  ```
  [断连] 第 61682 次请求后连接被服务端关闭：ConnectionAbortedError: [WinError 10053]
    已发送字节: 1048594 (1.000 MB)
    已收到响应: 61681
    服务端上限常量 MAX_LINE_BYTES = 1048576 (1 MiB)
  ```
- 影响：任何长驻客户端（合成器的持久连接若复用）在累计 1 MiB 上行后会被**静默掐断**，且客户端只看到连接关闭、无任何错误码。1 MiB ÷ 17 字节/ping ≈ 6.2 万次请求 —— 日常使用（尤其是含大 `arguments` 的 ToolCall）会显著更快触顶。
- 修复方向：`read_line` 后手工检查 `line.len()`，超限返回结构化错误；或每轮用 `by_ref().take(...)` 重置限制。顺带修正注释。
- 关联：这个上限目前**顺带**约束了 `Approvals` 的内存（见 P3-24）—— 修 P2-17 时不要把内存约束一起拆掉。

---

### P2-18 确认弹窗对参数值截断到 24 字符，且从不展示原始参数

- 位置：`aether-compositor/src/draw.rs:1276-1290`
  ```rust
  let shown = if v.chars().count() > 34 {
      let head: String = v.chars().take(24).collect();
      format!("{head}...")
  } else { v.clone() };
  ```
- 同时 `ConfirmRequest.raw_arguments`（`main.rs:494`）**只用于带令牌重发**（`main.rs:548`），**从未进入渲染路径** —— 用户在弹窗上看到的永远是截断后的版本，无处查看完整值。
- 影响：确认弹窗的唯一职责是"让用户看清将要执行什么"。把参数裁到 24 字符，等于把知情同意降级为"看到一个以良性前缀开头的字符串"。
  - 当前**被 `echo_required` 兜住**：`install_disk` 要求用户手输完整目标，截断显示反而会让回显失败（fail-safe）。
  - 但这是**唯一一个** L2+ 工具。未来任何没有 `echo_field` 的 L2 工具，其确认弹窗就只靠这 24 个字符 —— 攻击者只需把恶意值放在第 25 字符之后。
- 修复方向：弹窗内提供完整参数的查看方式（换行/可滚动/点开全文），或对超长值至少保留头尾两段（`head...tail`）。

---

### P2-20 system prompt 与工具注册表不一致，且 prompt 完全未提 `read_file`

- 位置：`aetherd/src/main.rs:20-22`（`SYSTEM_PROMPT`）vs `aetherd/src/main.rs:48-55`（`tools_json()` 从 `tools::registry()` 全量生成）
  ```rust
  const SYSTEM_PROMPT: &str = "你是 Aether，…用 desktop 工具切换窗口布局、打开应用、关闭窗口；\
  用 sys_probe/sys_info 了解系统状态。…";
  ```
  prompt 提到 3 个工具（desktop / sys_probe / sys_info），但 `tools_json()` 会把**全部 5 个**（含 `read_file`、`install_disk`）喂给模型。
- 影响：
  - 模型看到了 `read_file`（描述为"读取一个文本文件的内容（只读）"，看起来完全无害）与 `install_disk`（描述里明写 L3 需确认），却没有 prompt 层的使用约束。上一轮已指出"prompt 未提 install_disk 是内部矛盾"，本轮确认**同样问题也覆盖 read_file** —— 而 read_file 才是真正无防护的那个（P0-4a）。
  - 对 P0-4 是**放大器**：模型倾向于调用它"看得见但没被告知别用"的工具。
- 修复方向：prompt 与注册表保持一致；对 `read_file` 明确写入使用边界（如"只在用户明确要求时读取指定文件，不得主动遍历"）。

---

### P3 级 / 代码质量

| 编号 | 问题 | 证据 |
|---|---|---|
| P3-21 | `color::WARNING` 的 `#[allow(dead_code)]` **已过期** —— 它实际被 `draw.rs:404` 的 `badge_color()` 使用。属性留着会掩盖将来真正的死代码 | `draw.rs:53-54` vs `draw.rs:404` |
| P3-22 | `color::INFO` **确实从未被使用**（全仓仅 1 处 = 定义本身）。注释声称"Step 4 消费"，实际工具结果气泡用 SUCCESS/DANGER 左侧状态条实现，INFO 未落地 | `draw.rs:57-59` |
| P3-23 | 截断无省略号提示：气泡文本 `while measure(&shown) > max_w { shown.remove(0) }`（`draw.rs:992-994`）**从头部删字符、保留尾部**，且不加省略号；后果说明则 `shown.pop()`（`draw.rs:1346-1348`）**从尾部删**，也不加省略号。两种方向相反且都无提示 | `draw.rs:906-910`、`992-994`、`1188-1199`、`1346-1348` |
| P3-24 | `Approvals` 无容量上限，`issue()` 只按 TTL 清理。**但**受 `MAX_LINE_BYTES × MAX_CONNECTIONS` 间接约束，最坏约 32 MB（1 MiB × 32），实际风险低 | `perm.rs:110-120` |
| P3-25 | 通道标识靠字符串匹配，跨 crate 无类型保障：`aetherd` 发 `"local"`/`"cloud"`，`aether-compositor` 用 `match channel.as_deref() { Some("cloud") => Cloud, _ => Local }`。新增第三个通道会被静默当作 Local。有 `channel_labels_are_stable` 测试兜底，可接受 | `main.rs:208-212`、`main.rs:1096-1100`、`router.rs:129-133` |
| P3-26 | `AiEvent::Confirm` 无条件覆盖既有待确认项（`confirm = Some((*req, String::new()))`，`main.rs:228-231`），无守卫、无日志、无队列。**注**：`docs/ui-design-handover.md` §4.3 已自行记录此限制，故此处仅作确认，不重复计分 | `main.rs:228-231` |

---

## 3. Step 1–4 UI 重绘与协议扩展的审查结论

### 3.1 设计令牌收口（Step 1 / `350c26a`）—— 通过

`draw.rs` 的 `theme` 模块六个分区**全部真实接线**，未发现"定义了但没人用"的令牌：

| 分区 | 引用次数 | 结论 |
|---|---|---|
| `color::` | 137 | 除 `INFO` 外全部使用 |
| `font::` | 67 | 5 档字阶 + GLYPH 全部使用 |
| `radius::` | 48 | 三档全部使用 |
| `elevation::` | 8 | 4 档全部使用 |
| `state::` | 8 | 4 档全部使用 |
| `metric::` | 32 | 全部使用（`TITLE_HIT_H` 在 `main.rs:352/1205` 消费，`GAP` 在 `layout.rs` 消费） |

唯一的例外是 `color::INFO`（P3-22）与过期的 `allow(dead_code)`（P3-21）。**这是一次干净的重构。**

### 3.2 协议扩展的向后兼容（Step 4 / `c486d41` `fc6dcfd`）—— 通过

- `Request::ToolCall.approval`：`#[serde(default)] Option<String>` ✅
- `Response::ChatChunk.channel`：`#[serde(default)] Option<String>` ✅
- `ServiceStatus.restart` / `essential`：均有默认值 ✅
- 两个方向的兼容测试都在：`chat_chunk_roundtrip_with_channel`（新收新）、`chat_chunk_without_channel_decodes_to_none`（新收旧）✅
- `channel` 全链路已接线：`router::Channel::label()` → `AgentOutcome.channel` → `handle_chat` → `ChatChunk.channel` → `AiEvent::Reply(text, channel)` → `ai_status` ✅

### 3.3 字体渲染（Step 2 / `2381f99`）—— 源码合理，**视觉效果未验证**

`text.rs` 的三处改动（`px.round()` 统一入口、`gx`/`baseline` 取整到像素网格、≤14px 覆盖率增益 `sharpen`）在源码层面自洽，`vcenter` 用「国」字墨迹中心做垂直居中也有明确注释说明依赖。但**本轮未复跑 `scripts/shot-diff.py`**，无像素级证据，不作视觉结论（见 §4）。

### 3.4 确认弹窗绘制顺序（Step 4b–4d / `c645034`）—— 通过

`render_frame` 顺序：背景 → snap → 窗口 → 菜单栏 → 下拉 → 气泡 → AI 指令条 → Dock → 安装向导 → **确认弹窗** → Toast。模态在最上层、Toast 压在其上（保证拒绝/回显不匹配的提示可见），层级正确。鼠标命中区在 `confirm.is_some()` 时**独占**（`main.rs:257-283`），模态语义正确。

---

## 4. 未验证清单（明确不计入上述结论）

1. **`scripts/shot-diff.py` 的 18 项像素断言**：本轮未复跑。Step 1–3 的视觉改动（令牌落地、字体质量、组件重绘）**无本轮像素级证据**，仅有提交信息与交接文档的自述。
2. **Linux 实机行为**：所有实机复现均在 Windows 上完成。`read_file` 读 `/etc/shadow`、`/proc/self/environ`、root 权限下 aetherd 的实际可达范围，均为**源码推断**（可信度高，但未实机验证）。
3. **`aetherd` 的审计日志**：源码硬编码 `/var/log/aether/aether-audit.log`（`main.rs:207`、`server.rs:32`），Windows 下解析到**当前工作盘根** —— 本次实测落盘于 `D:\var\log\aether\aether-audit.log`。已确认 L0 工具**会**留痕：
   ```
   1790077058	L0 只读	read_file	{"path":"C:/Windows/win.ini"}	allowed
   1790077058	L0 只读	read_file	{"path":"C:/Users/cbn/.ssh/id_rsa"}	allowed
   ```
   （`1790077058` = 2026-09-22 19:37:38，本次复现）→ **审计覆盖本身没有缺口**；缺口在于日志内容无法区分确认来源（P1-9）。
4. ~~**`P0-4` 的攻击链端到端**~~ —— **已于本次补齐**，见 §2 P0-4(c) 的双假端点实测。
   仅剩一点未覆盖：**真实的提示词注入**（构造一段能让模型主动去读敏感文件的输入）未做，我用假端点直接下发了 `read_file` 工具调用作为替身。即：链条的"服务端侧"已实证，"模型是否会被说服"属于提示词工程范畴，未测。
5. **`elevation` 的阴影性能**：交接文档 §P4-2 自述"10fps 下是否掉帧没有实测"，本轮未补充。
6. **`install_disk` 并发绕过回显门**：`tool_install_disk` 的 `confirm == disk` 校验无并发保护。理论上两个并发请求都满足回显条件，但我**未构造出可达序列**（合成器侧同一时刻只有一个 `confirm`），故不列为缺陷。

---

## 5. 建议修复顺序

| 优先级 | 项 | 理由 |
|---|---|---|
| 1 | **P0-4(a)** `read_file` 加路径白名单 / 提权 | 单点改动、风险最高、与云端路由解耦后即可独立生效 |
| 2 | **P0-4(b)** 路由不再用 `history_len` 作复杂度信号 | 阻断"多轮工具任务自动上云" |
| 3 | **P1-8** 令牌绑定确认方身份 | 让"一次性确认"的陈述与实际安全边界一致 |
| 4 | **P1-9** 打通拒绝通路（协议 + 撤销 + 审计来源标注） | 让"用户拒绝了"成为系统中存在的事实；顺带使自助兑现可被事后识别 |
| 5 | **P2-20** prompt 与工具注册表对齐 | 与 #1 配合，降低模型调用 `read_file` 的倾向 |
| 6 | **P2-17** `take()` 改为单行长度检查 | 顺带修正注释；注意保留内存约束 |
| 7 | **P2-18** 弹窗展示完整参数 | 为将来的 L2 工具做前置准备 |
| 8 | **P3-21/22/23** 清理过期属性、死色板、截断提示 | 低风险收尾 |

---

## 6. 一句话总结

**Step 1–4 的工程质量明显高于上一轮**：37 项修复全部落地、测试 43→65 全绿、令牌机制与协议扩展的向后兼容做得干净、设计令牌收口是一次真实且彻底的重构。

**但安全边界没有随工程量一起提升。** 上一轮的核心问题是"危险操作没有闸门"；本轮的核心问题变成了三件事：

1. **闸门只挡模型，不挡别的** —— 令牌不绑确认者，任何 IPC 连接都能自助兑现自己触发的确认（P1-8，已实测）。
2. **真正没有闸门的那个工具已经足够完成一次完整外泄** —— `read_file` 是 L0、无路径约束、root 权限，而多轮工具任务必定路由到云端（P0-4，**已用双假端点端到端跑通：13 字无隐私信号的输入 → 两份文件内容到达云端端点**）。
3. **"用户拒绝了"这件事在系统里不存在** —— 协议没有、裁决不可达、审计无记录，且放行记录与自助兑现逐字节相同（P1-9，审计日志 29 条中 `denied` 出现 0 次）。

L3 擦盘现在很难被误触，但 L0 读文件 + 多轮上云这条路径，读走的东西比擦盘更多；而且事后审计无法告诉你，那次放行到底有没有人点过头。
