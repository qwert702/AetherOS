# AetherOS 修复报告 · 2026-09-28

- **对应审查**：`docs/CODE-REVIEW-2026-09-28.md`（第四轮增量审查）
- **对应计划**：`docs/PRODUCTION-PLAN-2026-09-28.md` §2「安全增补」
- **范围**：审查发现的 **1 个 P1 + 3 个 P2 + 5 个 P3**，全部修复
- **方式**：源码修改 → 单测 → 真实 IPC 端到端复现（不 mock）

---

## 0. 结果摘要

| 指标 | 修复前 | 修复后 |
|---|---|---|
| `cargo test --workspace --offline` | 185 项 | **192 项全绿**（+7） |
| `scripts/e2e-permission-confirm.py` | 17 项 | **19 项全 PASS** |
| 剪贴板复现（未注册连接读写） | **成功（缺陷）** | **403 全部拒绝** |
| 审计裁决值种类 | 5 | **6**（新增 `rejected_no_ui`） |
| 代码编译警告 | 0 | **0**（Windows 文件占用噪音不计） |
| 未修的审查项 | —— | **0**（`pty.rs` 的类型检查除外，见 §3） |

---

## 1. 修复清单

### P1-1 · 剪贴板 IPC 纳入权限模型

**位置**：`aetherd/src/server.rs`（`ClipboardSet` / `ClipboardGet` 分支）、`aether-compositor/src/main.rs::sync_clipboard_to_daemon`

**改法**：
1. 两个分支在读写前检查 `is_ui`，未注册连接返回 `403` 并落 `rejected_no_ui` 审计；
2. 合成器推送剪贴板前先 `register_ui()` —— 它本来就有这个辅助函数（`send_tool_call` / `query_aether` 都在用），只是剪贴板路径漏了。

**为什么这样修**：剪贴板与 L2+ 令牌**同一门槛**（已注册 UI 通道）。这不是"另加一道检查"，而是把漏掉的入口接回既有模型。

### P2-1 · `clipboard_write` 提级 L0 → L1

**位置**：`aetherd/src/tools.rs`

写操作标成 L0（只读）是分级语义错误；且当时**读（L1）比写（L0）级别高，方向是反的**。改为 L1，与 `clipboard_read` 同级。

### P2-2 · 审计日志轮转 + 写失败告警

**位置**：`aetherd/src/perm.rs::rotate_if_needed`、`aetherd/src/server.rs::audit_or_warn`

**改法**：
1. `audit()` 追加前检查大小，超过 8MB 则轮转（旧档留一份 `.1`）；
2. 新增 `audit_or_warn()` —— 把原先的 `let _ = gate.audit(...)` 换成显式报错。

**为什么**：`docs/ai-permissions.md` 机制 2 白纸黑字承诺"审计写入失败不阻断执行，但**不允许静默零留痕**"，而实现里所有调用都是 `let _ =`。磁盘写满后审计会静默停止，系统继续正常运行 —— 这比明显坏掉更危险。**修完这一项，文档承诺与实现才一致。**

### P2-3 · 剪贴板推送的端口劫持

**位置**：`aether-compositor/src/main.rs::sync_clipboard_to_daemon`

原先"连上就发"，不校验对端身份。现在先注册 UI 通道 —— **冒充 aetherd 的进程拿不到 `ui.key`，注册不过去**，服务端那侧会因密钥不匹配拒绝。与 P1-1 一次修掉（同源：整条 IPC 链缺对端认证）。

### P3 系列（5 项）

| # | 位置 | 改法 |
|---|---|---|
| P3-1 | `server.rs:219` | `L{level}` → `L{}` + `level as u8`。`Level` 的 `Display` 已含 "L3 危险"，原先拼成 `LL3 危险`；与 `handle_chat` 路径统一 |
| P3-2 | `main.rs::clipboard_copy` | 只有**显式拖选**的终端内容才同步给 aetherd。无选区时复制的是整屏（可能含 `cat /etc/shadow` 输出），随手按一下复制键不该等于"授权 AI 读取这一屏"。**本地粘贴不受影响** |
| P3-3 | `pty.rs::write_all` | 对 `EAGAIN` 做**有界重试**（最多 50ms）。主端是 `O_NONBLOCK`，原先 `n <= 0` 直接 break，会静默丢掉用户键入的字节 —— 大段粘贴时表现为"粘贴少了一截"，极难排查 |
| P3-4 | `clipboard.rs` | 内容加 **300 秒存活上限**（`TTL`），过期读取返回空并顺手清空。aetherd 以 root 运行，内容永久驻留等于给"AI 能读到的敏感数据"开永久窗口 |
| P3-5 | `aether-ipc/src/lib.rs` | 把塞在 `ui_registration_and_cancel_roundtrip` 里的剪贴板断言拆成独立的 `clipboard_roundtrip` |

### 附带改进：裁决值常量模块

**位置**：`aetherd/src/perm.rs::verdict`

原先裁决值是散落的字符串字面量（`"allowed"` / `"denied_by_user"` / …）。现收进 `verdict` 模块，全项目统一引用，并新增第 6 个值：

```rust
/// 未注册为 UI 通道的连接发起了受限请求（剪贴板 / L2+ 令牌兑现）。
pub const REJECTED_NO_UI: &str = "rejected_no_ui";
```

**为什么需要单独一个值**：它必须与 `denied`（"用户拒绝过"）区分开 —— 前者是**入侵尝试信号**，后者是用户意图。混在一起，审计就无法回答"有没有人在敲门"。

---

## 2. 验证证据

### 2.1 单元测试（192 项全绿）

新增 7 项：

| 测试 | 覆盖 |
|---|---|
| `perm::audit_rotates_when_oversized` | 超限轮转，且新日志只含新记录 |
| `perm::audit_creates_file_when_absent` | 首次写入不被轮转检查干扰 |
| `clipboard::content_expires_after_ttl` | 过期读为空且**确实被清掉** |
| `server::clipboard_requires_registered_ui` | **P1-1 回归** |
| `server::request_variants_are_gated` | **2.2b：闸门覆盖门禁** |
| `server::unprivileged_variants_still_work` | 反向断言（防"一律拒绝"作弊） |
| `ipc::clipboard_roundtrip` | 编解码往返（从错位函数中拆出） |

> **`request_variants_are_gated` 是这次最重要的产出**：它不是修一个 bug，而是把"每个能读用户数据或改系统状态的 `Request` 变体都必须被拦"固化成门禁。**新增变体若忘了加门槛，测试会红** —— P1-1 的根因（`handle_request` 逐分支手写权限）从此有了防线。

### 2.2 端到端（`e2e-permission-confirm.py`，19 项全 PASS）

```
PASS  6 未注册连接不得兑现令牌: 403 确认令牌兑现失败：本连接未注册为 UI 通道
PASS  7 未注册连接不得触发 L2+: 403 L2+ 操作（install_disk）必须由已注册的 UI 通道发起
PASS  9 拒绝后同操作直接 Denied: DENIED: 该操作已被用户拒绝（install_disk），300 秒内不再重复询问
```

日志侧同时确认了 P3-1 的修复：

```
[aetherd] 工具 install_disk 需 L3 确认，已下发确认请求      ← 修复前是「需 LL3 危险 确认」
```

### 2.3 剪贴板复现（9 项全 PASS）

```
PASS  未注册 · 读剪贴板被拒: 403 剪贴板访问必须由已注册的 UI 通道发起
PASS  未注册 · 写剪贴板被拒: 403 剪贴板访问必须由已注册的 UI 通道发起
PASS  已注册 · 写剪贴板成功: clipboard_written {'bytes': 12}
PASS  已注册 · 读回内容一致: clipboard_text {'text': '合规内容'}
PASS  已注册 · 超长整体拒绝: 413 超过上限 65536（拒绝整段，不做截断）
PASS  未注册 · L3 无令牌被拒: 403
PASS  已注册 · L3 需确认: needs_confirmation level=3
PASS  read_file 仍拒系统区: 路径不在允许读取的范围内（C:/Windows/win.ini）
PASS  clipboard_write 无需令牌即可执行（L1<L2）
```

**对照修复前的同一测试**：未注册连接可以读写剪贴板（写入 `PWNED-BY-UNAUTH` 后回读成功）。**同一个端点上，权限模型此前只守住了工具调用，没守住剪贴板。**

### 2.4 审计日志（6 种裁决值齐备）

```
     52 allowed
     35 needs_confirmation
      7 failed
      2 rejected_no_ui        ← 新增：本次测试的未注册访问尝试
      2 denied_by_user
      2 denied
```

顺带确认了两个设计意图确实生效：
- `read_file` 对 `C:/Windows/win.ini` 留下 **`allowed` + `failed` 两条** —— 闸门放行但路径白名单拒绝，二者在审计里可区分（这是 P0-4 修复时的设计目标）；
- `denied` 与 `denied_by_user` **均非零** —— P1-9 的拒绝通路确实可达。

另：剪贴板的审计 `args` 列是**空串**，不记录剪贴板内容本身（隐私正确）。

### 2.5 编译警告

`cargo check --workspace --offline` 在代码层面 **零警告**（输出中剩余的 `error copying object file … 拒绝访问` 是 Windows 文件占用导致的增量编译噪音，与代码无关）。

---

## 3. 未验证

| 项 | 原因 | 验证方法 |
|---|---|---|
| **`pty.rs` 改动的类型检查** | 本机只有 `x86_64-unknown-linux-musl` 的 **rust target**，没有 `x86_64-linux-musl-gcc` 交叉工具链（`minifb` / `ring` 的构建脚本需要它），`cargo check --target …` 失败。**已做语法检查**（`rustc` 解析阶段无 `expected …` 错误，报的全是缺 `libc` 这类名字解析错误） | 构建机（VM）上 `cargo build --target x86_64-unknown-linux-musl -p aether-compositor` |
| 终端真机行为（PTY、拖选、`Ctrl+Shift+C/V`） | 同前几轮，Windows 开发机无法运行 `pty.rs`；`--shot` 测不了点击 | QEMU/VMware 实机 |
| 归档图与 HEAD 的像素一致性 | 本轮改动未重建归档图（改动集中在逻辑层） | `scripts/archive-ui-shots.py` + 像素比对 |
| 门禁 1（8 小时不崩） | 需实机长跑 | 实机 |

**未验证的项不计入 §0 的"0 未修"结论。**

---

## 4. 对计划的影响

`docs/PRODUCTION-PLAN-2026-09-28.md` 中受本次修复影响的项：

| 计划项 | 原状态 | 现状态 |
|---|---|---|
| 2.2a 剪贴板 IPC 纳入权限模型 | ⬜ P1 | **✅ 已完成** |
| 2.2b `Request` 变体闸门覆盖测试 | ⬜ P1 | **✅ 已完成** |
| 2.2c 剪贴板内容超时清理 | ⬜ P3 | **✅ 已完成** |
| 4.3b 审计日志轮转 + 写失败告警 | ⬜ P2 | **✅ 已完成** |
| P2-3 端口劫持 | ⬜ P2 | **✅ 已完成** |
| P3 系列 5 项 | ⬜ | **✅ 已完成** |
| 2.3 中文 IME | ⬜（前置 2.2a） | **前置已解除**，可排期 |
| 2.4 统一文本交互 | ⬜（前置 2.2a） | **前置已解除**，可排期 |
| 4.1 可写文件操作 | ⬜（前置 4.3b） | **前置已解除** —— 审计缺口补齐后，L2 敏感写的追溯手段才成立 |

**按修订后的排期，下一个该做的是 0.7（真实 LLM 端到端验证）** —— 它是当前最大的架构级未知（agent 循环按 OpenAI 兼容假设写的，从未与真模型对过）。

---

## 5. 改动文件清单

| 文件 | 改动 |
|---|---|
| `aetherd/src/server.rs` | 剪贴板闸门 · `audit_or_warn` · LL3 修复 · 4 项新测试（mod 改名 `ipc_gating_tests`） |
| `aetherd/src/perm.rs` | `rotate_if_needed` · `MAX_AUDIT_BYTES` · `verdict` 常量模块 · 2 项新测试 |
| `aetherd/src/clipboard.rs` | `TTL` 超时清理 · 1 项新测试 · 文档更正 |
| `aetherd/src/tools.rs` | `clipboard_write` L0→L1 · 引用 `verdict` 常量 |
| `aether-compositor/src/main.rs` | `sync_clipboard_to_daemon` 注册 UI · 只同步显式选区 |
| `aether-compositor/src/pty.rs` | `write_all` 有界重试 |
| `aether-ipc/src/lib.rs` | 拆出 `clipboard_roundtrip` 测试 |
| `docs/ai-permissions.md` | 入口规则 · `rejected_no_ui` · 机制 10（剪贴板受限端点） |
