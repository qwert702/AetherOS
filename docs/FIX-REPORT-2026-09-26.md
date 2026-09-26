# AetherOS 修复报告 · 2026-09-26

对应审查：`docs/CODE-REVIEW-2026-09-26.md`（第三轮增量）
范围：该报告 §5 修复顺序中的全部 9 项 + 上轮遗留 5 项
验证：`cargo test --workspace --offline` **74 项全过**（原 65 + 新增 9）· `scripts/e2e-permission-confirm.py` **17 项全 PASS**（原 13 + 新增 4）· 对比度与像素级实测

> 环境：Windows 开发机。`cargo` 必须带 `--offline`（不带会卡在 registry 访问，实测 17 分钟无编译活动）。
> 所有实机复现均打在本轮重建的 `target/debug/aetherd.exe` 上。

---

## 1. 修复清单

| # | 项 | 文件 | 状态 |
|---|---|---|---|
| 1 | P1-8 令牌绑定确认方身份 | `aether-ipc/src/lib.rs`、`aetherd/src/server.rs`、`aether-compositor/src/main.rs` | ✅ |
| 2 | P0-4a `read_file` 路径白名单 | `aetherd/src/tools.rs` | ✅ |
| 3 | P0-4b 路由不再用 `history_len` | `aetherd/src/router.rs`、`aetherd/src/main.rs`、`aetherd/src/tools.rs` | ✅ |
| 4 | P1-9 打通拒绝通路 | `aether-ipc`、`aetherd/src/perm.rs`、`aetherd/src/server.rs`、`aether-compositor` | ✅ |
| 5 | P2-19 归档图基线重建 | `docs/host-ui-*.png`（8 张） | ✅ |
| 6 | P2-20 主按钮对比度（含同类 3 处） | `aether-compositor/src/draw.rs` | ✅ |
| 7 | P2-18 参数头尾展示 / P3-23 截断加省略号 | `draw.rs` | ✅ |
| 8 | P3-27 Dock 托盘接 `glass_alpha()` / P3-28 `fill_rect` 回卷 | `draw.rs` | ✅ |
| 9 | 文档同步 | `docs/ai-permissions.md`、`docs/ui-design-handover.md` | ✅ |

---

## 2. 安全链路（P1-8 / P0-4a / P0-4b / P1-9）

### 2.1 P1-8：确认令牌绑定"确认方"而非"请求方"

**问题**：令牌不绑确认者，任何能连上 7311 的进程（甚至**新建连接**）都能兑现自己触发的令牌。

**改动**：

- 协议新增 `Request::RegisterUi { key }` / `Response::UiRegistered`
- aetherd 启动时确定 UI 密钥：`AETHER_UI_KEY` 环境变量优先，否则生成随机值写入 `/var/log/aether/ui.key`（Unix 0600）
- 连接级状态 `is_ui`：只有已注册通道才 **① 收到 L2+ 确认令牌**、**② 兑现令牌**
- 合成器在每次请求前先在同一条连接上注册（`register_ui()`）

**实测（修复前后对比）**：

```
修复前：[未注册连接] 带令牌重发 -> tool_result
        output = '[工具错误] 安装器运行失败: 系统找不到指定的路径。 (os error 3)'
        （错误来自工具体内部 = 请求已穿过闸门）

修复后：[未注册连接] 带令牌重发 -> error
        message = '确认令牌兑现失败：本连接未注册为 UI 通道'

        [未注册连接] 无令牌触发 L2+ -> error
        message = 'L2+ 操作（install_disk）必须由已注册的 UI 通道发起'
```

**配套修改（关键）**：`scripts/e2e-permission-confirm.py` 的第 2 项断言原先依赖"新连接带令牌即放行"——**等于把缺陷固化成验收标准**。已重写为：注册后兑现成功 + 新增"未注册连接不得兑现 / 不得触发 L2+"两条回归断言。

**诚实边界**：这挡不住**能读到 `ui.key`** 的本机进程。IPC 尚无用户级认证（M4 规划）。它的实际收益是：把攻击面从"任何能连 7311 的进程"收窄到"能读到 0600 密钥的进程"，并让"确认方"成为服务端可断言的事实——后者正是原实现完全缺失的一环。

### 2.2 P0-4a：`read_file` 加路径白名单

**改动**：新增 `resolve_readable()`——先 `canonicalize`（同时解析 `..` 与符号链接），再校验白名单根，最后拒绝凭证类子路径。

- 允许根（Linux）：`/home`、`/etc/aether`、`/var/log/aether`、`/run/aether`、`/tmp`
- 拒绝子路径：`.ssh`、`.gnupg`、`.aws`、`.kube`、`id_rsa`、`id_ed25519`、`id_ecdsa`、`.netrc`、`.git-credentials`
- 用**白名单**而非黑名单：黑名单永远漏（符号链接、大小写、`..` 回绕）

**实测**：

```
read_file('C:/Windows/win.ini')
  修复前 -> ok=True，返回 207 字符（'; for 16-bit app support\r\n[fonts]...'）
  修复后 -> ok=False，'路径不在允许读取的范围内（C:/Windows/win.ini）'

read_file('C:/Windows/System32/drivers/etc/hosts')
  修复后 -> ok=False，'路径不在允许读取的范围内'

read_file('C:/Users/.../__aether_absent__.txt')   # 白名单内、文件不存在
  修复后 -> ok=False，'无法访问 ...（系统找不到指定的文件）'   ← 策略放行，未误伤
```

### 2.3 P0-4b：路由不再用 `history_len` 判复杂度

**改动**：`router::Task` 移除 `history_len`，新增 `sensitive_context`；`route()` 中"敏感上下文"的优先级**高于**"复杂任务上云"。`read_file` 标记 `sensitive_output: true`，其输出进入上下文后 agent 循环置位 `sensitive_context`，后续轮次强制本地。

**为什么必须这么改**：每轮工具调用给消息表 +2 条，旧规则 `history_len >= 6` 导致"两次 `read_file` 之后的第 3 轮必然上云"——前两轮读到的文件内容会被整体发往云端端点。

**新增断言**（`router.rs`）：

- `sensitive_context_forces_local_even_when_complex`：含敏感上下文时，即使任务复杂、即使本地不可用，也留在本地
- `tool_rounds_alone_do_not_force_cloud`：13 字无隐私无复杂信号的输入，无论上下文多长都留在本地；同时确认"真有复杂信号时仍上云"（云端通道没被禁用）

### 2.4 P1-9：让"用户拒绝"成为服务端的事实

**改动**（三处协同）：

1. 协议新增 `Request::ConfirmCancel { token }` / `Response::ConfirmCancelled { token }`
2. `Approvals::revoke(token)` → 返回被撤销的 `(tool, args)`
3. `Gate` 新增拒绝冷却表 `denied` + `mark_denied()` / `is_denied()`；`judge()` 改为先查冷却、再走等级闸门
4. 合成器三处拒绝路径（Esc、鼠标点"拒绝"、预览路径）全部回传 `cancel_confirm()`

**`Verdict::Denied` 从此可达**：拒绝后同一 (tool, 参数) 在 300 秒内直接拒绝，不再反复弹卡片。

**实测**：

```
[已注册连接] 请求 install_disk -> needs_confirmation
[拒绝] confirm_cancel -> confirm_cancelled { token: 17a25b3d... }
[再兑现同一令牌] -> error 403 '确认令牌无效或已被使用'
[再请求同一操作] -> tool_result ok=False
   '[工具错误] DENIED: 该操作已被用户拒绝（install_disk），300 秒内不再重复询问'

审计日志裁决分布（累计）：
     30 allowed
     21 needs_confirmation
      1 denied_by_user      ← 新增，此前不存在
      1 denied              ← 新增，此前恒为 0
```

### 2.5 附带：审计区分"放行"与"真的执行成功"

原实现下，`read_file` 通过闸门但被路径白名单拒绝时，审计只留一条 `allowed`，会误导为"读到了"。现在工具执行失败会追加一条 `failed`：

```
1790386734	L0 只读	read_file	{"path":"C:/Windows/win.ini"}	allowed
1790386734	L0 只读	read_file	{"path":"C:/Windows/win.ini"}	failed
```

---

## 3. UI 层修复（P2-20 / P2-18 / P3-23 / P3-27 / P3-28）

### 3.1 对比度（P2-20，含同类扩展）

| 元素 | 模式 | 修复前 | 修复后 | 判定 |
|---|---|---|---|---|
| 允许一次 | 深色 | 2.07:1 | **5.31:1** | PASS |
| 允许一次 | 明亮 | 4.11:1 | **5.59:1** | PASS |
| 拒绝 | 深色 | 3.22:1 | **5.02:1** | PASS |
| 拒绝 | 明亮 | 4.87:1 | **6.56:1** | PASS |
| L3 徽章 | 深色 | 2.56:1 | **5.17:1** | PASS |
| L3 徽章 | 明亮 | — | **6.71:1** | PASS |

（WCAG AA 正文门槛 4.5:1；数值由区域最暗/最亮像素的相对亮度实算，避开抗锯齿边缘。）

新增 token：`accent_strong()`（主按钮底）、`danger_text()` / `warning_text()`（彩色文字变体）、`ConfirmUi::badge_text_color()`。

### 3.2 信息完整性

- **P2-18**：确认弹窗参数值由"只留头 24 字符"改为"**头 20 + 尾 10**"并用 `…` 标出截断——"良性前缀 + 第 25 字符起的恶意值"不再能躲过视线
- **P3-23**：回复气泡由"从头部删字符"改为 `ellipsize`（保留开头 + 省略号）——消息的"要干什么"不再被吃掉；后果说明同理

### 3.3 令牌接线与防御性修复

- **P3-27**：Dock 托盘 alpha 0.68（硬编码）→ `color::glass_alpha(hover)`（明亮 0.95 / 深色 0.75），与 `draw.rs` 注释和 handover §12.2 的声称一致
- **P3-28**：`fill_rect` 的 `(rect.x + rect.w) as usize` 加 `.max(0)`（i32 负值 `as usize` 会回卷成巨值，`.min(w)` 再把它钳成 `w` → 误填整行）

---

## 4. 归档图基线重建（P2-19）

`docs/host-ui-desktop.png` 等深色图**停留在 §9（09-24 20:07）**，从未随 §10 更新，却被 `2cccb6a` 用作"深色像素级零回归"的基线。实测该图与当时实际渲染差 **1.727%**（15272 像素，最大差 15）。

**已重建全部 8 张**（深色 4 + 明亮 4），生成命令记入 `docs/ui-design-handover.md §13.2`，使基线可复现。

---

## 5. 回归结果

| 检查 | 结果 |
|---|---|
| `cargo test --workspace --offline` | **74 项全过**（4 + 13 + 9 + 6 + 0 + 0 + 42 + 0） |
| `scripts/e2e-permission-confirm.py` | **17 项全 PASS** |
| 新增单元测试 | 9 项（aether-ipc +2、perm +2、router +2、tools +3） |
| 新增 e2e 断言 | 4 项（未注册不得兑现 / 未注册不得触发 L2+ / 拒绝回执 / 拒绝后不可兑现 / 拒绝后直接 Denied） |
| 归档图 | 8 张全部重新生成并目视通过 |

---

## 6. 未覆盖 / 剩余风险（诚实边界）

1. **UI 密钥是单机共享密钥，不是用户级凭证**。能读到 `/var/log/aether/ui.key` 的进程仍可注册为 UI 并兑现令牌。彻底解决需要 IPC 认证（M4 规划），本轮做的是"提高门槛 + 让确认方成为可断言的事实"。
2. **`read_file` 白名单的 Windows 分支是开发机专用**（`C:/Users`、`C:/Temp`）。目标平台的 Linux 分支（`/home`、`/etc/aether` 等）**未在实机验证**，仅由单元测试覆盖（`#[cfg(target_os = "linux")]` 分支）。
3. **真实提示词注入未测**：本轮验证的是"服务端侧链条已断"，"模型是否会被说服去读敏感文件"属于提示词工程范畴。
4. **P0-4b 的端到端未复跑**：未用双假 LLM 端点重跑"读文件 → 上云"链。依据是单元测试（`sensitive_context` 强制本地）与源码改动，**不是端到端实测**。
5. **`fill_rect` 回卷（P3-28）未构造出可达序列**，属防御性修复。
6. **`scripts/shot-diff.py` 仍无法运行**（缺 `before-r1/r2-l*.bmp` 输入），本轮用归档图重建 + 区域对比度测量替代。
7. **`shadow()` 多窗口性能、交互态目视**（拖拽/悬停）仍未覆盖。
