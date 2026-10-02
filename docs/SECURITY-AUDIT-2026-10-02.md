# AetherOS 代码库全面分析与安全审计报告

- **审计日期**：2026-10-02
- **审计基线**：`git HEAD = dd6b097`（2026-10-02 13:27 +0800），工作区源码未修改
- **审计范围**：仓库根全部内容（7 个 Rust crate、`platform/`、`scripts/`、`deploy/`、`site/`、`docs/`、`更新日志/`、构建产物 ISO）
- **审计方式**：静态代码审查 + **对真实二进制的运行时实证**（起 `aetherd serve`、走真实 NDJSON 协议）+ 对已构建 ISO 的二进制解析
- **审计声明**：全程未修改仓库任何源码。实证过程中 `aetherd` 向 `D:\var\log\aether\aether-audit.log` 追加了若干行审计记录（该文件是开发机既有产物，非本次新建）；探针产生的临时文件与回收站条目已清理。

---

## 摘要（结论先行）

**这是什么**：AetherOS 是一个"Linux 内核 + 全自研 Rust 用户态 + AI 中枢"的操作系统项目。内核之上的行为层（PID 1、窗口合成器、终端、输入法、AI 中枢、IPC 协议、磁盘安装器）全部自研，7 个 crate、24,491 行 Rust、45 个源文件，产出可引导 ISO（38.49 MiB）。工程质量与验证意识**明显高于同类个人项目**：370 项单测实跑全绿、编译零警告、门禁脚本化、每一处设计取舍都写了理由。

**但本次审计发现了 6 条高危问题，其中 2 条是对本项目**最核心卖点——L0–L3 权限模型**的完整绕过，且已用真实二进制端到端复现：**

| # | 结论 | 严重度 | 状态 |
|---|---|---|---|
| 1 | **任何本机进程都能拿到 UI 密钥并自我确认 L3 操作**（整盘擦除/文件删除），权限模型形同虚设 | 高危 | **实测复现** |
| 2 | **剪贴板 UI 通道闸门被工具调用路径绕过**（同一连接走协议端点 403，走 `ToolCall` 却成功） | 高危 | **实测复现** |
| 3 | 安装器**先 dd 破坏目标盘、后校验**，而工作区 ISO 实测**非 isohybrid** ⇒ 装机必然失败却打印"安装完成" | 高危 | 静态 + 二进制实测 |
| 4 | `/var` 挂载**不校验设备身份**（不读卷标/UUID、无 fsck、默认 rw）⇒ 可能把宿主机 root 分区挂成 `/var` 并写入 | 高危 | 静态 |
| 5 | 日志写入**不防符号链接**，可把 root 的日志追加写变成任意文件写 | 高危 | 静态 + 镜像实测 |
| 6 | 生产部署脚本 `deploy-site.py` **无条件跳过 SSH 主机密钥校验 + 默认 root + 口令认证** | 高危 | 静态 |

**另外两个必须知道的事实**：

- **文档与代码存在系统性漂移**：README 自身就有"370 项 vs 362 项"的矛盾；INDEX 的逐文件行数表 42 条里 11 条是错的（4 处 `src/main.rs` 被同一数字 `3,857` 覆盖）；而 `repo-stats.py --check` 门禁**只校验合计行**，所以这些漂移它全部报 `[ OK ]` —— 门禁给了虚假保证。
- **`/var/log/aether/` 同时是"AI 可读的日志目录"和"UI 密钥 + 完整工具参数的存放地"**。这一个设计重叠，同时制造了上面的高危 #1，以及"审计日志明文记录剪贴板内容/写入文件内容"的中危问题。

**关于合成器（`aether-compositor`，15,890+ 行）**：内存安全基线良好——17 处 `unsafe` 逐条评估基本正确、生产代码 10 处 `unwrap()` / 2 处 `expect()` 经核实**全部不可 panic**、线协议解析未发现越界/溢出/死循环。真实问题集中在**外部程序可控的输入路径**：VT 解析器的无界参数表（M-14）、宽字符越界 panic（M-15）、PTY 未清环境（M-16）、终端粘贴即执行（M-13）。其中"AI 写剪贴板 → 用户粘贴 → root 执行"这条**看起来最严重的链经核实不成立**（合成器从不读取 aetherd 的剪贴板），本报告按实际可达性降级为 M-13——**验证过的"不成立"与验证过的"成立"同样重要**。

---

## 一、项目全貌

### 1.1 架构与定位

```
┌───────────────────────────────────────────────┐
│  aether-compositor + (占位) aether-shell       │  桌面 / 顶栏 / Dock / AI 指令条
│    draw · layout · text · term(vt/pty)         │
│    ime · textview · input · wayland(spike)     │
├───────────────────────────────────────────────┤
│  aetherd          AI 中枢：意图 → 路由 → 工具  │  权限闸门 L0–L3 · 审计
│  aether-ops       AI 运维：巡检 → 诊断 → 自愈  │
├───────────────────────────────────────────────┤
│  aether-ipc       统一 IPC（JSON / NDJSON）    │
├───────────────────────────────────────────────┤
│  aether-init      PID 1 / 服务管理 / 持久化    │  musl 静态
│  aether-install   整盘安装器                   │
├───────────────────────────────────────────────┤
│  glibc · 驱动栈 · 固件                         │
├───────────────────────────────────────────────┤
│  Linux kernel 6.6.22（Buildroot 2024.02.1）    │
└───────────────────────────────────────────────┘
```

设计意图明确且自洽：内核不重写（ADR-001），差异化放在"AI 能深度操作每一个系统行为"上，因此所有系统行为都经 `aether-ipc` 暴露成机器可调用的接口（ADR-002）。这是一个**有真实技术判断力**的架构取舍，不是包装。

### 1.2 规模与构成（权威口径）

`python scripts/repo-stats.py`（口径 = 换行符个数，等价 `wc -l`）：

| crate | 行数 | 文件 | 角色 |
|---|---:|---:|---|
| `aether-compositor` | 16,021 | 22 | 合成器 + 桌面 Shell 职责 + 终端 + IME + Wayland spike |
| `aetherd` | 5,565 | 11 | AI 中枢：agent / 工具 / 权限 / 路由 / 模型配置 / 回收站 / 应用安装 |
| `aether-init` | 1,319 | 6 | PID 1、服务管理、持久化分区、日志 tee |
| `aether-ops` | 736 | 3 | 巡检、诊断、自愈 |
| `aether-install` | 505 | 1 | 整盘安装器 |
| `aether-ipc` | 334 | 1 | 全系统 IPC 协议 |
| `aether-shell` | 11 | 1 | 占位骨架（确认属实） |
| **合计** | **24,491** | **45** | 与 README/INDEX 声明的合计**完全一致** |

其他口径（便于对照）：物理行 24,492、非空行 22,857、非空非注释 19,209。
**注意口径陷阱**：PowerShell 的 `(Get-Content f | Measure-Object -Line).Lines` **不计空行**，用它核对会得出"文档虚报 1,634 行"的错误结论。

### 1.3 技术栈与依赖

| 层 | 技术 | 备注 |
|---|---|---|
| 语言 | Rust 2021，`rust-toolchain.toml` 固定 `stable` | 用户态全 Rust |
| 依赖 | `serde` / `serde_json` / `anyhow` / `log` / `libc` / `fontdue` / `minifb` / `ureq` | `Cargo.lock` 已提交，版本固定 |
| TLS | `ureq 2.12.1` → `rustls 0.23.43` + `ring 0.17.14` | **证书校验默认开启**，全仓无 `danger_accept_invalid_certs` / `verify=False` / `-k` |
| 目标 | `x86_64-unknown-linux-musl`（静态）/ Windows（开发预览） | 两目标 |
| 基础镜像 | Buildroot 2024.02.1 + Linux 6.6.22 | 无包管理器、无更新通道 |
| 构建 | `platform/build-iso.sh`（Buildroot 外部树） | 见 §1.4 |
| 站点/部署 | Python（`gen-site.py` / `deploy-site.py`）+ paramiko + nginx | 生产域名 `aether.cbnac.com` |
| 许可证 | GPL-3.0-only | 内核 GPL-2.0 的聚合体说明写得准确 |

**依赖安全**：Rust 侧 `Cargo.lock` 固定、无高危废弃件；但 **Python 侧完全没有依赖清单**（无 `requirements.txt` / `pyproject.toml`），`paramiko` / `Pillow` / `fontTools` 版本随环境漂移。本次未执行 `cargo-audit` 漏洞库比对（**未验证**）。

### 1.4 构建与交付链路（含一处实测硬伤）

`platform/build-iso.sh`：下载 Buildroot 2024.02.1 → 交叉编译 7 个 crate 为 musl 静态 → 拷入 overlay 与字体 → 打包 ISO9660/El Torito。

**实测发现（本报告独立复核）**：工作区 `aetheros-0.1-amd64.iso`（40,359,936 字节）

```
前 512 字节是否全 0 : True
偏移 510/511 字节   : 0x0 0x0      （0x55 0xAA = MBR 签名）
4 个分区项          : 全为 16 个 0x00
含 EL TORITO SPECIFICATION: True
```

即**该 ISO 不是 isohybrid 镜像**，而 `aether-install` 的整盘安装前提正是 isohybrid（详见 §2.2 H-6）。`platform/build-iso.sh` 全程**没有 isohybrid 步骤**；只有 `scripts/rebuild-m4.sh:30` 在构建机 VM 内补做这一步。

> 诚实边界：本次无法确认 GitHub Release 上那份 ISO 是否经过 isohybrid 处理（**未验证**）。可以确认的是：**按仓库文档给出的构建路径（`platform/build-iso.sh`）产出的 ISO，其安装器无法工作**。

### 1.5 工程实践亮点（客观记录，不吝肯定）

这些是本次审计中反复确认过的、**做得确实好**的地方：

1. **单测密度高且真实**：`cargo test --workspace --offline` 实跑 **370 passed / 0 failed**，覆盖权限闸门、令牌一次性与参数绑定、路由隐私、回收站策略、ELF 预检、Wayland 编解码等。测试不是凑数的——`wire.rs` 16 项测试把"size 在高 16 位"这种最容易写反的地方钉死了。
2. **编译零警告**：Windows 目标全量重编 0 warning（已验证）；musl 目标 6/7 个 crate 0 warning（`aetherd` 因 `ring` 需要 `x86_64-linux-musl-gcc` 无法验证）。
3. **生产路径的 Rust 代码 unwrap/panic 极少**：全工作区生产代码仅 10 处 `unwrap()` / 3 处 `expect()` / 0 处 `panic!`，且逐处核查**全部有前置长度校验保护**（如 `wire.rs` 的 8 处 `try_into().unwrap()` 都由 `need!(4)` 先做边界检查）。
4. **命令执行零注入面**：`aether-init` 的服务命令是**编译期字面量**（`unit.rs:56-108`），服务 JSON 只能引用白名单名、**没有 argv/路径字段**；`aether-install` 的 `dd` 参数经 argv 传参；全仓无 `sh -c` 拼接。
5. **路径白名单设计正确**：`read_file` / `file_write` 用**白名单 + `canonicalize`**（先解析 `..` 与符号链接再判定），而不是黑名单；且**写白名单比读白名单更窄**（`/etc/aether`、`/var/log/aether` 可读不可写——防 AI 改自己的权限规则、防灭证）。
6. **隐私路由的修正有深度**：`router.rs` 把"历史长度"当复杂度信号的错误彻底改掉了——原实现下"两次 read_file 后的第 3 轮必然上云"，会把读到的文件内容整体带走。现在改为 `sensitive_context` 一票否决且优先级高于"复杂任务上云"。
7. **诚实边界写进代码注释**：多处主动写明"这挡不住什么"（如 `clipboard.rs:15`、`modelcfg.rs:34`、`server.rs:71`）。这种诚实是本次审计能快速定位真问题的前提。
8. **误报排除工作扎实**：`scripts/vm.py` 的 TOFU 主机密钥策略（首次记录+打印指纹，之后 `RejectPolicy`）、`vm_path_of()` 的重定根、`serve-apps.py` 的符号链接逃逸防护、`mkapp.py` 的 soname 穿越防护，都是主动设计而非巧合。

---

## 二、安全审计

### 2.1 范围、方法与证据强度

| 证据等级 | 含义 | 本次使用 |
|---|---|---|
| **A：运行时实证** | 真实二进制 + 真实协议/真实文件 | 高危 #1、#2；单测 370 项；ISO 头解析；审计日志内容 |
| **B：镜像/产物实测** | 解包 ISO 内 initramfs 核对权限、账号、服务 | `/etc/shadow` 空口令、`/var/log -> ../tmp`、`/tmp` 1777、无 sshd/iptables |
| **C：静态代码审查** | 逐文件读取 + 行号引用 | 其余全部发现 |
| **D：策略函数等价复现** | 拷贝同一份常量与判定逻辑独立编译运行 | `/var/log/aether/ui.key` 的读取策略判定 |
| **U：未验证** | 无法在本机证实 | 报告中显式标注 |

工具与环境：`cargo 1.98.0` / `rustc 1.98.0`（离线构建可用）、Python 3.11、真实 `aetherd.exe`（由当前源码构建）。**未做**：Linux 实机运行、物理机、真实 GPU/外设、真实 LLM 端点。

**关于并行审计与复核**：本报告的发现来自 4 路并行深审（脚本/部署/站点、底层组件与平台、合成器与 Wayland、文档一致性），**每一条被采纳的结论都由主审重新核对过原始代码行**。复核中明确**推翻**了一条看起来最严重的候选链（"AI 写剪贴板 → 终端粘贴 → root 执行"），见 §2.3 M-13 与 §2.7；也修正了一处行号体系差异（`Measure-Object -Line` 不计空行，且在本环境下对含中文的 UTF-8 文件会合并行号，本报告所有行号以逐行读取结果为准）。

### 2.2 高危

---

#### H-1【权限模型完全绕过】任意本机进程可自取 UI 密钥并自我确认 L3 操作

**严重度**：高危（本项目最核心安全机制被完全击穿）

**位置**：
- `aetherd/src/server.rs:22` — `const UI_KEY_PATH: &str = "/var/log/aether/ui.key";`
- `aetherd/src/tools.rs:475-482` — `READ_ALLOWED_ROOTS` 含 `"/var/log/aether"`
- `aetherd/src/tools.rs:489-499` — `READ_DENY_SUBPATHS` **不含** `ui.key`
- `aetherd/src/tools.rs:180-182` — `read_file` 是 **L0**（免确认）
- `aetherd/src/server.rs:209-234` — `ToolCall{approval: None}` **不校验 `is_ui`**，直接执行 L0/L1
- `aetherd/src/perm.rs:96,136` — `auto_approve_below = Level::L2` ⇒ L0/L1 自动放行
- `aetherd/src/server.rs:167-176` — 密钥匹配即 `is_ui = true`
- `aetherd/src/server.rs:239-246` — 令牌只发给 `is_ui` 连接；`perm.rs:212` 签发时只绑 `(tool, args)`

**攻击链（5 步，无需任何用户交互）**：

```
① 任意本机进程 connect 127.0.0.1:7311（明文 TCP，无认证、无 SO_PEERCRED）
② ToolCall{read_file, "/var/log/aether/ui.key"}     ← L0，免确认，不要求已注册
   → aetherd 以 root 身份代读 0600 的密钥文件并原样返回
③ RegisterUi{key}                                    → 本连接成为"可信 UI 通道"
④ ToolCall{install_disk, {...}}（不带 approval）
   → 服务端签发 NeedsConfirmation + 一次性令牌**给这条连接**
⑤ 原样回带 approval=令牌                              → 校验通过 → 以 root 执行
```

**实证结果（真实二进制 + 真实协议，2026-10-02 复现）**：

```
1. [未注册] ClipboardSet（协议端点）      -> error code=403 剪贴板访问必须由已注册的 UI 通道发起
2. [未注册] ClipboardGet（协议端点）      -> error code=403
3. [未注册] ToolCall clipboard_read(L1)   -> tool_result ok=True
4. [未注册] ToolCall clipboard_write(L1)  -> tool_result ok=True 已写入剪贴板（19 字节）
5. [未注册] ToolCall read_file(L0)        -> tool_result ok=True output='probe victim'
6. [未注册] ToolCall file_delete(L2)      -> error code=403 L2+ 操作必须由已注册的 UI 通道发起
7. [未注册] RegisterUi 错误密钥            -> error code=403
8. [攻击者] RegisterUi 正确密钥            -> ui_registered
9. [攻击者] ToolCall file_delete 未带令牌  -> needs_confirmation level=2 token=b2f8ab370027…
10.[攻击者] 用令牌自我确认后重发           -> tool_result ok=True 已移入回收站
临时受害文件是否已消失: True
```

第 2、3 步的对比就是**同一份数据的两个入口**：协议端点 403，工具路径放行。

**策略函数等价复现（D 级证据，Linux 分支）**：把 `tools.rs` 的 `READ_ALLOWED_ROOTS` / `READ_DENY_SUBPATHS` / `is_credential_path` / `check_read_policy` 逐字拷贝编译运行：

```
[允许读取] /var/log/aether/ui.key          ← 攻击链的关键一步
[允许读取] /var/log/aether/aether-audit.log
[允许读取] /etc/aether/model.json
[拒绝]     /home/u/.ssh/id_rsa  <- 拒绝读取凭证类路径
[拒绝]     /etc/shadow          <- 路径不在允许读取的范围内
```

**为什么这是新问题、而不是已知残余**：`docs/archive/CODE-REVIEW-2026-09.md` §3.2 第 5 条确实把"能读到 `ui.key` 的进程可注册为 UI"列为已知残余，但它给出的**风险界定是**"把攻击面从任何能连 7311 的进程收窄到**能读 0600 密钥的进程**"——这个界定的前提是"读 0600 root 文件需要特权"。而 `read_file` 工具恰恰以 root 身份、以免确认的 L0 等级、向任意未注册连接提供该文件，**前提被同一套系统的另一个功能推翻了**。已知残余因此升级为完整提权链。

**前置条件与影响**：
- 需要本机任意代码执行（任意 uid）。出厂镜像目前只有 root（空口令），所以"非特权→root"的增益暂时有限；但**权限模型本身已经失效**——L2/L3 的"用户确认"从未真正发生。一旦引入非 root 用户或应用沙箱（roadmap 方向），或 `AETHER_BIND=0.0.0.0` 被用于调试，它就是完整的本地/远程提权链。
- 可执行的操作包括：`install_disk`（整盘擦除）、`file_write` / `file_delete`（以 root 写删任意白名单内路径）、`app_install`、`reload_config`、剪贴板读写。

**修复建议（按性价比排序）**：
1. **把 `ui.key` 移出 AI 可读白名单**：改放 `/run/aether/ui.key`（`/run` 不在 `READ_ALLOWED_ROOTS` 内），或在 `READ_DENY_SUBPATHS` 加 `ui.key`。**一行改动即可切断该链**。
2. **给 `ToolCall` 加与 `Request` 同等的连接信任检查**：对 L1+ 工具（尤其 `clipboard_read` / `clipboard_write` / `desktop`）要求 `is_ui`，或按工具声明"需要可信通道"。
3. **令牌绑定确认方身份**：签发时记录连接身份（uid via `SO_PEERCRED`，或至少绑定连接实例），兑换时校验同一来源。
4. **aetherd 改 Unix socket 0600**（`docs/ipc-protocol.md` 本来就写的是 `/run/aetherd.sock`，实现却用了 TCP）——顺带解决"任何本机进程可达"。
5. 长期：引入真正的 IPC 认证（项目自己规划的 M4）。

---

#### H-2【闸门被绕过】剪贴板 UI 通道闸门在工具调用路径上失效

**严重度**：高危（第四轮审查 P1-1 的修复被绕过）

**位置**：`aetherd/src/server.rs:277-303`（协议端点有闸门）对比 `server.rs:209-234` + `aetherd/src/tools.rs:43-77`（工具路径无闸门）

**证据（实测）**：

```
未注册连接写入剪贴板 -> tool_result ok=True 已写入剪贴板（41 字节）
未注册连接读回剪贴板 -> tool_result ok=True
读回内容 -> '用户刚复制的密码：Hunter2-AETHER'          ← 机密性绕过
同一连接走 ClipboardGet 协议端点 -> error code=403 剪贴板访问必须由已注册的 UI 通道发起
```

审计日志里的同一秒三条记录，把矛盾摆得很清楚：

```
1790920534  L1 可逆写  clipboard_write  {"text":"用户刚复制的密码：Hunter2-AETHER"}  allowed
1790920534  L1 可逆写  clipboard_read   {}                                        allowed
1790920534  L1 可逆写  clipboard_get    (空)                                      rejected_no_ui
```

**根因**：P1-1 的修复加在 `Request` 变体上，而守卫测试 `server.rs:520-554 request_variants_are_gated` **只覆盖 `Request` 变体**。同一份数据经 `Request::ToolCall{tool:"clipboard_read"}` 到达时，走的是 `Gate::judge`——L1 < L2 ⇒ `Allowed`。这正是该项目自己在 `CODE-REVIEW` §2 总结的结构性教训"**逐分支手写的权限检查必然漏**"的又一次重演，只不过这次漏在"测试的覆盖面"上：门禁测试给的是**虚假信心**。

**修复建议**：把闸门从"按 `Request` 变体手写"改为"按**资源敏感度**声明"——在 `Tool` 定义里加 `requires_trusted_channel: bool`，`clipboard_read` / `clipboard_write` 置 true；`ToolCall` 分支统一检查。同时给 `request_variants_are_gated` 增加 `tool_call(L0/L1)` 用例（当前只测了 L2/L3，恰好避开漏洞）。

---

#### H-3 `/var` 挂载不校验设备身份，可能把宿主机 root 分区挂成 `/var`

**严重度**：高危

**位置**：`aether-init/src/persist.rs:15-16`、`persist.rs:34-45`

```rust
pub const PERSIST_CANDIDATES: [&str; 4] =
    ["/dev/vda2", "/dev/sda2", "/dev/hda2", "/dev/nvme0n1p2"];
for dev in PERSIST_CANDIDATES {
    if !std::path::Path::new(dev).exists() { continue; }
    let ok = std::process::Command::new("/bin/mount")
        .arg("-t").arg("ext4").arg(dev).arg("/var")
```

文件头注释声称挂载的是"安装器建好的 ext4 分区（**卷标 AETHER**）"，但代码**从不读卷标**——`PERSIST_LABEL` 只在安装器 `mkfs.ext4 -L` 时写入。启动侧无 `blkid`/UUID 校验、无 `fsck`，挂载选项为默认 `rw`（无 `nodev,nosuid,noexec,ro`）。

**后果**：`/dev/nvme0n1p2` 恰是多数现代 Linux 的 root 分区。在双系统机器上引导该镜像，宿主机分区会被以 root `rw` 挂成 `/var`，随后 logtee / ops / `append_boot_record` 向其写入 `/var/log/aether/*.log`、`/var/diag/*`、`/var/boot.log`——**静默污染另一个操作系统的根文件系统**。

**前置条件**：能在目标机引导该镜像（物理接触/U 盘/VM，无需口令）且第 2 分区为 ext4。**未在实机验证**（本机无 Linux）。

**修复建议**：按 `LABEL=AETHER` 或安装时写入的 UUID 挂载；挂载前 `fsck -p`；挂载选项加 `nodev,nosuid,noexec`。

---

#### H-4 日志写入不防符号链接，root 的日志追加可变成任意文件写

**严重度**：高危

**位置**：`aether-init/src/logtee.rs:41-46`（`OpenOptions::create(true).append(true).open(&path)` 跟随符号链接）、`logtee.rs:23-31`（轮转用 `remove_file` + `rename`）

**镜像实测基线**：`/var/log -> ../tmp`、`/var/tmp -> ../tmp`、`/tmp` 权限 **1777**。

**攻击路径**：在被挂载的持久分区上预置 `/log/aether/aetherd.log -> /etc/shadow`（或 `/etc/aether/services/aetherd.json`），启动后 root 会把服务输出**追加写入任意文件**；轮转的 `rename` 同理会覆盖目标。与 H-3 组合时，攻击者只需控制被挂载分区上的一个符号链接。

**修复建议**：`O_NOFOLLOW`（或写前 `symlink_metadata` 校验）、日志目录 `0700`、`/var/log` 不要指向 `/tmp`。

---

#### H-5 生产部署脚本无条件跳过 SSH 主机密钥校验，且默认 root + 口令认证

**严重度**：高危（生产路径）

**位置**：`scripts/deploy-site.py:81-88`（`paramiko.AutoAddPolicy()`）、`:277`（默认 `user = root`）、`:297`（脚本自述"该密码曾在聊天记录里出现过"）

```python
c = paramiko.SSHClient()
c.set_missing_host_key_policy(paramiko.AutoAddPolicy())   # 静默接受任何主机密钥
...
c.connect(host, username=user, password=env("AETHER_VPS_PASSWORD"), timeout=25)
```

**风险**：`AutoAddPolicy()` 使 MITM 可冒充生产 VPS。三个放大条件同时存在：① 默认 root 登录；② 未配密钥时用**口令认证**（口令每次连接明文经 SSH 发送）；③ 目标是跑着多个业务的生产机（脚本自述同机还有 `school / admin.school / api / blog / dili / dsh.* / teach / storage`）。一旦被冒充，攻击者拿到的不只是官网。

**值得注意的对照**：**同一个仓库里的 `scripts/vm.py:123-126` 已经实现了正确做法**（首次记录并打印指纹，之后 `RejectPolicy` 严格校验，密钥变更时提示"可能是中间人"）。生产脚本反而没有沿用。

**修复建议**：复用 `vm.py` 的 TOFU 策略；默认用户改非 root + sudo 白名单；强制密钥认证、删除口令回退；**立即轮换该 VPS 口令**。

---

#### H-6 安装器先 dd 破坏目标盘、后校验前提；工作区 ISO 实测非 isohybrid

**严重度**：高危（数据不可恢复）

**位置**：`aether-install/src/main.rs:343-351`（先 dd）→ `main.rs:246-250`（之后才校验 MBR 签名）→ `main.rs:355-360`（失败仅告警）→ `main.rs:372`（仍打印"安装完成"）

```rust
// 343：先破坏目标盘首 ~32MB
let status = Command::new("/bin/dd").arg(format!("if={SOURCE}")).arg(format!("of={disk}"))
    .arg(format!("bs={BS}")).status()?;
// 246：之后才检查源镜像是否 isohybrid
if mbr[510] != 0x55 || mbr[511] != 0xAA {
    bail!("MBR 签名缺失（ISO 是否经过 isohybrid？）");
}
```

**实测（本报告独立复核）**：工作区 ISO 前 512 字节全 0、无 `0x55AA`、分区项全空 ⇒ **非 isohybrid**；`platform/build-iso.sh` 无 isohybrid 步骤。

**后果**：用该产物装机 → 目标盘首 32MB 被覆盖、分区表缺失、不可引导、持久化分区未建立，程序却打印"安装完成"，退出码 0。这与 README/CHANGELOG 中"可安装到虚拟磁盘后独立引导"的表述冲突。

> 未验证：GitHub Release 上那份 ISO 是否经过 isohybrid（可能由 `rebuild-m4.sh` 的流程产出）。**可确认的是文档给出的构建路径产出的 ISO 不行。**

**修复建议**：把 isohybrid 校验**移到 dd 之前**（且校验源镜像本身）；`build-iso.sh` 补上 isohybrid 步骤（或在文档中明确它只产"仅 CD 引导"镜像）。

### 2.3 中危

| # | 问题 | 位置 | 说明 |
|---|---|---|---|
| M-1 | **审计日志明文记录工具参数（含机密）** | `aetherd/src/tools.rs:336` + `perm.rs:157` | `args_str = args.to_string()` 落盘，`clipboard_write` 的 `text`、`file_write` 的 `content`（≤256KB）**全文进日志**。实测日志行含 `{"text":"用户刚复制的密码：Hunter2-AETHER"}`。违反"日志不得出现密码/密钥"基线。且日志目录可被 `read_file` 读取（§2.2 H-1）⇒ 二次泄露 |
| M-2 | **审计日志文件权限未收紧** | `aetherd/src/perm.rs:153-156` | `OpenOptions::create(true).append(true)` 无显式 mode，由 umask 决定（推定 0644）；`Gate::new` 的 `create_dir_all` 同样未收紧。对比 `ui.key`/`model.json` 都显式 0600——**同一项目内的不一致** |
| M-3 | **PID 1 错误路径不回落到救援模式** | `aether-init/src/main.rs:179-187`、`manager.rs:60-62,88` | `rescue_console` 只在 `specs.is_empty()` 时触发；服务目录多一个同名 JSON（如误复制 `aetherd-copy.json`）或出现依赖环 ⇒ `?` 冒泡 ⇒ PID 1 退出 ⇒ 内核 `Attempted to kill init!`，**每次启动必复现**，只能人工救砖 |
| M-4 | **控制台与串口即 root（空口令）** | 镜像实测 `/etc/shadow: root::::::::`；`platform/br2-external/isolinux.cfg:5` 开串口控制台 | 物理或串口访问即 root。对一台会往持久分区存数据的系统，这是实质性基线缺陷 |
| M-5 | **全部服务以 root 运行，服务定义无降权字段** | `aether-init/src/unit.rs:111-127`、`manager.rs:307-320` | `ServiceSpec` 只有 name/deps/essential/restart/autostart，无 uid/gid/capability/namespace。aetherd（有 LLM 出网 + 工具执行）与 compositor 同权 |
| M-6 | **ops 日志监听非 UTF-8 时偏移永不推进** | `aether-ops/src/monitor.rs:229-248` | `read_to_string` 失败时 `continue` 不推进 offset，且无大小上限 ⇒ 每 15s 重复整读一次（root 进程），叠加 ramfs 无容量上限可致 OOM |
| M-7 | **安装器 `--disk` 未规范化** | `aether-install/src/main.rs:334,76,100` | `if disk == SOURCE` 仅字符串比较（`/dev/../dev/sr0` 可绕过防自读自写）；`metadata()` 跟随符号链接。aetherd 侧有 `valid_disk_path` 白名单（`tools.rs:784-788`），CLI 侧没有 |
| M-8 | **"整盘安装"不擦盘，且安装后 `/var/apps` 变空** | `aether-install/src/main.rs:301,343-347` | 只写首 ~32MB + 格式化第 2 分区 ⇒ 旧数据块残留可恢复；骨架目录无 `apps` ⇒ 镜像自带应用"消失" |
| M-9 | **构建链无完整性校验** | `platform/build-iso.sh:58-60`（Buildroot 无 sha256/签名）、`:27`（`cargo build` 无 `--locked`）；`scripts/vm-setup-deps.sh:13-14`（`curl sh.rustup.rs \| sh`） | 构建机被劫持即污染发布产物；`rsproxy.cn` 作为 crates 唯一源，索引与 `.crate` 同源可被同时篡改 |
| M-10 | **nginx 安全响应头因 `add_header` 继承语义静默失效** | `deploy/nginx-aether.conf:46-72`、`scripts/deploy-site.py:194-205` | 各 `location` 内又定义了 `add_header` ⇒ server 级 CSP/HSTS/X-Content-Type-Options/Referrer-Policy **全部不下发**。配置"看起来已加固"但线上不生效（nginx 语义未实测，依据官方文档） |
| M-11 | **终端用户下载 ISO 后不校验哈希** | `scripts/try-aether.sh:41-51`、`try-aether.ps1:35` | 下载即 QEMU 引导完整操作系统镜像，却无校验；而 SHA256 就在同仓库 `site/data.json:10` 与 `site/download.html:54`。属供应链风险 |
| M-12 | **`modelcfg::summary()` 在非 ASCII API Key 上 panic** | `aetherd/src/modelcfg.rs:108` | `&k[k.len() - 4..]` 按**字节**切片，非字符边界即 panic。**已独立复现**：key `"密钥ab"` → `byte index 4 is not a char boundary`。仅经 `aetherd config --show`（CLI）可达，故列中危而非高危；但这是本项目生产代码里唯一确认可达的 panic |
| M-13 | **终端粘贴把 `\n` 转成 `\r` 且无 bracketed paste ⇒ 粘贴多行文本即在 root shell 里逐行执行** | `aether-compositor/src/term.rs:258-282`、`main.rs:3245-3255` | `paste_bytes` 主动把换行转成回车（注释明说"否则命令永远不会执行"），而 `clipboard_paste` 直接把它写进 root shell 的 PTY。用户从任意来源复制一段多行文本再粘贴，就等于在 root shell 里逐行执行（经典 pastejacking）。**注意**：已核实合成器**从不读取 aetherd 的剪贴板**（只有出向 `ClipboardSet`，`desktop.clipboard` 仅由用户复制写入）⇒ "AI 写剪贴板 → 粘贴 → root 执行"这条链**不成立**，风险限于用户主动复制的不可信文本 |
| M-14 | **VT 解析器 `params` 无上限 ⇒ 终端内任意程序可令 root 合成器内存无界增长** | `aether-compositor/src/vt.rs:231,236`（push），`clear` 仅在 `:201,350,429,491` | `ESC[` 之后每收到一个 `;` 就 push 一个 `u16`，且只在**下一条转义序列开始**时才 clear。终端里的程序持续输出 `ESC[;;;;;;…` 即可让解析器内存按输入 2 倍增长（1 字节输入 → 2 字节内存），最终 OOM。合成器以 root 运行，panic/被杀会导致桌面中断 |
| M-15 | **VT 宽字符写入 `cells[idx + 1]` 无边界检查，`cols == 1` 时越界 panic** | `aether-compositor/src/vt.rs:275-298`（`:291`） | `if width == 2 { self.cells[idx + 1] = ... }` 未校验 `cur_x + 1 < cols`。`cols == 1` 且光标在末行时 `idx + 1 == cells.len()` → panic。**当前几何路径不可达**（窗口最小宽度 320px），属**潜在**缺陷——布局或分辨率一变，就是"打印一个汉字崩桌面" |
| M-16 | **PTY 子进程以 root 运行且未 `env_clear()`，继承合成器全部环境变量** | `aether-compositor/src/pty.rs:63-69` | `Command::new(SHELL).env("TERM",…).env("PS1",…).env("LANG",…)` 未清空环境。而 UI 密钥的约定恰好是"**环境变量优先**"（`main.rs:1009`、`aetherd/src/server.rs:75`）⇒ 若设置了 `AETHER_UI_KEY`，终端内任意程序可读密钥并自注册为 UI 通道。出厂镜像**未设置**该变量（已验证）⇒ 出厂不可利用；一旦按文档设置即成立 |
| M-17 | **合成器先发送 UI 密钥、后校验对端身份** | `aether-compositor/src/main.rs:1026-1040` | `register_ui` 的第一条写就是 `RegisterUi{key}`，之后才读应答。aetherd 未监听/重启期间，本机进程可抢先绑定 `127.0.0.1:7311` 直接**收到明文密钥**。与 H-1 同源（密钥保护不足），属纵深防御缺口 |

### 2.4 低危

| # | 问题 | 位置 |
|---|---|---|
| L-1 | 日志无脱敏且全量 tee 到 `/dev/console`（转义序列注入/日志伪造面） | `aether-init/src/logtee.rs:56-85` |
| L-2 | ops 跟随日志目录内符号链接并把内容写进 `/var/diag` 报告 | `aether-ops/src/monitor.rs:229`、`main.rs:40-52` |
| L-3 | init IPC 无 `SO_PEERCRED`、无连接超时/线程上限（0600 已大幅缓解） | `aether-init/src/ipc.rs:29-40,83-97` |
| L-4 | 特权二进制未 strip（`.symtab` 保留）；正面：PIE + NX + RELRO 均已启用 | 镜像实测 |
| L-5 | `mkapp.py` 的 `--id` / `--entry` 未做穿越校验，`--id ..` 可致 `shutil.rmtree` 越界删除 | `scripts/mkapp.py:614-625` |
| L-6 | 上传脚本用 `assert` 做完整性校验（`python -O` 下被剥离） | `scripts/transfer.py:61-62` 等 3 处 |
| L-7 | QMP/VNC 无认证，唯一控制是"只绑回环" | `scripts/qemu-verify.sh:26,28`、`vnc-shot.py:22-23` |
| L-8 | `setup-vm.ps1` 把 VM 口令作为命令行参数传 `VBoxManage` | `scripts/setup-vm.ps1:80-82` |
| L-9 | `serve-apps.py` 已下发 `X-SHA256` 但文档化的 guest 流程不校验 | `scripts/serve-apps.py:77,17-19` |
| L-10 | 站点生成器把 Markdown 链接 URL 直拼 `href`，未校验协议与引号（`javascript:` 可注入；与 M-10 叠加则可执行） | `scripts/gen-site.py:133-139` |
| L-11 | `scripts/` 目录 ACL 允许 `Authenticated Users` 修改（含 `.vm_known_hosts`） | ACL 实测 |
| L-12 | 安装器整盘 dd 未用 `conv=fsync`（末尾有 `sync`，已并入 M-8） | `aether-install/src/main.rs:343` |
| L-13 | 把 IPC 来源字符串写入 `/dev/tty0`（VGA 文本控制台），未过滤转义序列 ⇒ 控制台注入 | `aether-compositor/src/main.rs:63-71` |
| L-14 | 合成器的 IPC `read_line` 六处均无长度上限，AI 回复 `push_str` 亦无上限 | `aether-compositor/src/main.rs:1034,1058,1113,1159,3159,3202` |
| L-15 | PTY 主端 fd 缺 `O_CLOEXEC`；`fcntl` 返回值未校验；子进程已回收后 `Drop` 仍发 `SIGHUP`（PID 复用风险） | `aether-compositor/src/pty.rs:36,87-88,154-168` |
| L-16 | Wayland 会话层（未接线）：无连接/线程上限与超时、无跨读消息重组（16KB 读缓冲 < 64KB 消息上限）、对象表无上限且 `destroy` 不清理 | `aether-compositor/src/wayland/server.rs:83-142`、`session.rs:148,264` |

### 2.5 信息

- **I-1（正面）**：`aether-init` 服务白名单是**编译期枚举 6 项**，JSON 只能引用白名单名、无 argv/路径字段 ⇒ 服务定义**无法**变成任意命令执行。
- **I-2**：镜像权限基线：`/init` 0755、`/etc/shadow` 0600、`/root` 0700、`services/*.json` 0644、`aether-*` 0755；除 `/tmp`(1777) 外无世界可写目录。`bin/busybox` 为 04755 setuid 且无 `/etc/busybox.conf`（降权行为**未验证**）。
- **I-3**：镜像含 `/etc/inittab` 与 `/linuxrc -> busybox` ⇒ `/init` 缺失时会退回无口令 root shell；`/etc/init.d/*` 与 `/etc/fstab` 是**死配置**（无人执行 rcS / `mount -a`）。
- **I-4**：镜像指纹未改：`/etc/hostname=buildroot`、`/etc/issue="Welcome to Buildroot"`、os-release 仍是 Buildroot。
- **I-5**：无 sshd/dropbear/telnetd、无 iptables/nftables、无 SELinux/AppArmor/seccomp（**未验证**）、无模块签名配置（**未验证**）、无更新通道。
- **I-6**：`aetherd` 的 `AETHER_BIND` 可把 IPC 暴露到 `0.0.0.0`（出厂未设置，但调试脚本会注入）——与 H-1 叠加即为网络可达的提权链。
- **I-7**：`docs/ipc-protocol.md` 称 aetherd 走 `/run/aetherd.sock`，实际是 `127.0.0.1:7311` TCP（**文档与实现不一致**）。
- **I-8**：`aether-install` 头注释称 ISO 经 isohybrid 处理，`build-iso.sh` 中无此步骤（同 §1.4）。
- **I-9**：`Gate::auto_approve_below` 是 `pub` 字段，当前恒为 `L2`（全仓无改写点）；若将来有代码把它设为 `L0`，等于全局关闭确认。建议改为构造参数 + 断言。
- **I-10**：`server.rs:126` 的 `take(MAX_LINE_BYTES)` 是**连接级累计**上限而非单行上限（代码注释已诚实说明），1MB 后连接会被静默关闭。

### 2.6 OWASP Top 10 映射

| 类别 | 本项目结论 |
|---|---|
| A01 访问控制失效 | **最严重的一类**：H-1（权限模型完全绕过）、H-2（闸门被工具路径绕过）、H-4（符号链接写） |
| A02 加密失败 | 无禁用 TLS 校验、无弱算法；但 M-1（审计日志明文存机密）、M-2（日志权限未收紧）、`cloud_base` 允许 `http://` 明文回传 API Key（`main.rs:60-67` 未强制 HTTPS） |
| A03 注入 | **未发现**：命令执行全为字面量/argv，无 SQL/NoSQL，无 XML，无 `eval`；模板注入不适用 |
| A04 不安全设计 | H-3（/var 挂载无身份校验）、M-5（服务全 root）、M-4（空口令 + 串口）、`ui.key` 放在 AI 可读目录（H-1 的设计根因） |
| A05 安全配置错误 | M-10（nginx 安全头失效）、M-4（空口令）、I-4（默认指纹）、I-3（死配置与兜底 shell） |
| A06 脆弱过时组件 | M-9（Buildroot 2024.02.1 / Linux 6.6.22，无更新通道）；`cargo-audit` 未跑（**未验证**） |
| A07 认证与会话失效 | H-1（令牌不绑身份）、H-5（SSH 无主机校验 + 口令认证）、L-7（QMP/VNC 无认证） |
| A08 软件与数据完整性失效 | H-6（安装器）、M-9（构建链）、M-11（ISO 无校验）、L-6（assert 被剥离） |
| A09 日志与监控失效 | M-1（日志含机密）、M-2（权限）、L-1（无脱敏）；**正面**：6 种裁决值区分度高、`rejected_no_ui` 能识别入侵尝试、8MB 轮转、写失败显式告警 |
| A10 SSRF | 不适用（所有 HTTP 目标为配置常量，无用户可控 URL 入参） |

### 2.7 误报排除（看起来可疑但实际安全）

审计的严谨性同样体现在**不把安全的东西报成漏洞**：

| 可疑点 | 结论 | 依据 |
|---|---|---|
| `wayland/wire.rs` 生产代码有 8 处 `unwrap()` | **安全** | 每处 `try_into().unwrap()` 前都有 `need!(4)` 长度校验；`raw.last().unwrap()` 前有 `len == 0` 分支。`size` 来自高 16 位（≤65535），无无界分配 |
| Wayland 线协议解析可被外部客户端喂任意字节 | **当前无攻击面，但已进编译路径** | 精确表述是"**已接入编译路径、未接入运行路径**"：`main.rs:42` 确有 `mod wayland;`（模块会编进二进制），但 `WaylandServer::bind` **全仓无调用点**（不创建任何 socket）、`sync_wayland` 只在测试里被调用、`attach_pool_data` 仅测试调用。README 的"未接生产路径"说法**成立**；接线（W5）后 L-16 等缺陷立即可达 |
| 终端粘贴 → root 命令执行（"AI 写剪贴板即可"） | **链不成立，但原语危险** | 合成器与 aetherd 的剪贴板同步是**单向**的（`main.rs:3200` 只发 `ClipboardSet`，全文件无 `ClipboardGet`/`ClipboardText` 处理），`desktop.clipboard` 仅由用户复制写入 ⇒ AI/攻击者**无法**注入被粘贴的内容。但 `paste_bytes` 的 `\n→\r` 转换本身构成 pastejacking 风险（见 M-13） |
| `shm.rs::read_pixels` 的 `stride < width * 4` 用 i32 可能溢出 | **实际不可利用** | 溢出后仍需通过 `need > pool.len()` 的 usize 越界校验，需 ≥4GB 的 pool 才可能；且该模块未接生产路径 |
| `ui.key` / `model.json` 的 0600 | **权限设置正确** | 问题在**读取路径**（H-1），不在文件权限本身 |
| `perm.rs` 的 `random_token()` 用 `RandomState` 而非 CSPRNG | **实践上可接受** | `RandomState` 由系统随机源播种的 SipHash 密钥，输出不可预测；但**不是**文档化的密码学 API。建议改用 `getrandom`（低危改进项） |
| `Approvals::redeem` 只绑 `(tool, args)` 不绑连接 | **H-1 的组成部分** | 已并入 H-1，不重复计 |
| 审计日志 `writeln!("{ts}\t{level}\t{tool}\t{args}\t{verdict}")` 存在换行注入面 | **不可利用** | `args` 是 `serde_json::Value::to_string()`，换行/制表符已被 JSON 转义为 `\n` / `\t` 字面量 |
| `aether-init` 的 socket bind→chmod 窗口、`remove_file`+`bind` 的 TOCTOU | **非特权不可利用** | `/run` 为 0755 root，非特权用户无法预置文件/链接 |
| `aetherd` 的 `Gate::auto_approve_below` | **当前无改写点** | 全仓仅 `Gate::new` / `for_test` 两处赋值，均为 `L2`（见 I-9） |
| 全仓 `chmod 777/666`、`verify=False`、`curl -k`、`StrictHostKeyChecking=no` | **零命中** | 全仓扫描；凭据全走环境变量，唯一"凭据类"文件 `.claude/settings.local.json` 内容为命令白名单 |
| `vm.py` 用了 `AutoAddPolicy` | **不是漏洞** | 它是**条件 TOFU**：有记录时 `RejectPolicy`，仅首次连接记录并打印指纹（`vm.py:123-126`）——这正是 H-5 应当效仿的实现 |

### 2.8 修复优先级

| 优先级 | 项 | 一句话 |
|---|---|---|
| **P0（立即）** | H-1 | `ui.key` 移出 AI 可读白名单（一行）+ `ToolCall` 补连接信任检查 + 令牌绑身份 |
| **P0（立即）** | H-2 | 工具声明"需要可信通道"，`ToolCall` 分支统一检查；门禁测试补 L0/L1 用例 |
| **P0（立即）** | H-5 | 部署脚本改 TOFU 主机密钥 + 非 root + 强制密钥；**轮换 VPS 口令** |
| **P1（本迭代）** | H-6、H-3、H-4 | 安装器前置校验 + `build-iso.sh` 补 isohybrid；`/var` 按卷标/UUID 挂载 + fsck + `nodev,nosuid,noexec`；日志写入 `O_NOFOLLOW` |
| **P1（本迭代）** | M-1、M-2、M-10、M-11 | 审计日志参数脱敏 + 收紧权限；nginx 安全头改 `include` 片段并验证响应头；下载 ISO 校验 SHA256 |
| **P1（本迭代）** | M-3 | PID 1 全错误路径回落 `rescue_console`（这是"变砖"级风险） |
| **P2** | M-4~M-9、M-12、M-14、M-16 | 空口令/串口、服务降权、ops 日志偏移、安装器 `--disk` 规范化、构建链校验、`summary()` 改字符边界安全切片、VT 参数表加上限、PTY `env_clear()` |
| **P2** | M-13、M-15、M-17 | 终端启用 bracketed paste 并过滤控制字符；`vt.rs:291` 补 `cur_x + 1 < cols` 边界；合成器改"先校验对端再发密钥" |
| **P3** | L-1~L-16、I-1~I-10 | 见上表（含 Wayland 会话层接线前必修的 L-16） |

---

## 三、文档与声明一致性核查

### 3.1 核实为**一致**的声明

| 声明 | 结论 |
|---|---|
| 合计 24,491 行 / 45 源文件 / 7 个 crate | ✅ 与独立复核完全一致 |
| `aether-shell` 是 11 行占位 | ✅（468 字节，1 文件，0 测试） |
| 权限模型 L0–L3 分级、令牌一次性且绑参数、6 种裁决值 | ✅ 代码与 `docs/ai-permissions.md` 一致（**机制实现是真的**；问题在于可被绕过，见 H-1/H-2） |
| 写白名单比读白名单窄 | ✅ `WRITE_ALLOWED_ROOTS` ⊂ `READ_ALLOWED_ROOTS` |
| `read_file`/`clipboard_read` 强制本地推理 | ✅ `sensitive_output` 机制真实且优先级正确 |
| Windows 目标 0 编译警告 | ✅ 全量重编验证 |
| ISO 约 38.5 MB | ✅ 40,359,936 字节 = 38.49 MiB |
| README/INDEX 的文档链接 | ✅ 41 个唯一引用无真正失效 |
| Wayland 是未接生产路径的 spike | ✅ 已验证（无监听套接字、无生产调用点） |

### 3.2 **矛盾 / 漂移**的声明

| # | 声明 | 实际 | 问题 |
|---|---|---|---|
| 1 | README 表头"**370** 项单元测试全绿" | 实跑 **370 passed / 0 failed** | ✅ 正确 |
| 2 | README 正文三处"**362** 项"、快速开始"Windows 362 项" | 362 是过时值 | ❌ **README 内部自相矛盾（370 vs 362）** |
| 3 | README"Linux 358 为按 aetherd 增量推算" | 静态推算应为 381 | ❌ 无法复现 |
| 4 | INDEX L307=362、L349=362/358、**L351-352=333** | 同节内 358 与 333 直接冲突 | ❌ |
| 5 | README.en.md L44/L196=**334**；L53 把"370 项"说成 VT 解析器的测试数 | 中文版写 29（`vt.rs` 实测 29 项） | ❌ 过时 + **翻译错误** |
| 6 | `docs/LAUNCH.md`=334、`LAUNCH-COPY.md`=356、`更新日志/2026-10-01.md`=356、`2026-10-02.md`=368/370 | 370 才是当前真值 | ❌ 四处过时 |
| 7 | README 逐 crate：compositor **15,890** | 实测 **16,021** | ❌ 差 131；且 README 各 crate 之和 = 24,360 ≠ 它自己的表头 24,491 |
| 8 | INDEX 逐 crate：compositor **15,926** | 实测 16,021 | ❌ 差 95 ⇒ **同一 crate 三个数字**（15,890 / 15,926 / 16,021） |
| 9 | INDEX 逐文件表 42 条 | **11 条错** | ❌ 其中 **4 处 `src/main.rs` 全被写成 `3,857`**（aetherd 实际 636 / init 210 / ops 175）——一次全局替换的误伤 |
| 10 | INDEX L359-368「测试分布」表 | 填的是**行数/文件数**不是测试数 | ❌ 整表已损坏不可用 |
| 11 | INDEX 内部：compositor 逐文件之和 15,879 vs 小计 15,926；`wayland/` 2,194 vs 2,192 | 自相矛盾 | ❌ |
| 12 | 走查图张数：README **10 张** / INDEX **19 张**（同文件另处又写 10） | 实际 20 张文件 / 19 张在门禁清单 | ❌ 三处不一致 |
| 13 | INDEX L49 ISO"约 30MB" | 38.5 MB（同文件 L187 又写 41 MB） | ❌ |
| 14 | CHANGELOG"架构上系统内**不存放上游 API Key**" | `modelcfg.rs` 持久化 `api_key` 到 `/etc/aether/model.json`，且默认 `cloud_base` 就是 GLM 官方端点 | ⚠️ 架构意图与实际能力不符（未强制走用户网关） |
| 15 | README/CHANGELOG"可安装到虚拟磁盘后独立引导" | 工作区 ISO 实测非 isohybrid，安装器前提不成立 | ⚠️ 与 H-6 冲突（发布产物是否 isohybrid 未验证） |
| 16 | `docs/ipc-protocol.md` 称 aetherd 走 `/run/aetherd.sock` | 实际 `127.0.0.1:7311` TCP | ❌ |

### 3.3 根因：门禁给了**虚假保证**

`scripts/repo-stats.py --check` 实跑输出：

```
[ OK ] INDEX.md: 24,491 行 / 45 文件
[ OK ] README.md: 24,491 行 / 45 文件
[ OK ] docs/roadmap.md: 24,491 行 / 45 文件
exit=0
```

门禁 3/3 全绿，但**同一个 compositor 在 README 写 15,890、在 INDEX 写 15,926、实测 16,021**。原因：`DECL_INDEX` / `DECL_README` / `DECL_ROADMAP` 三条正则**只抓合计行**，逐 crate、逐文件的数字完全不受保护。这是一个**机制性缺陷**：门禁只能防住它恰好覆盖的那一个数字。

### 3.4 未跟踪文件：归档搬迁后的残留旧副本

`git status` 长期挂着 5 个 `??`（全在 `docs/` 下）。SHA256 比对显示：4 个与 `docs/archive/` 下同名文件**逐字节相同**，第 5 个 `docs/UNIMPLEMENTED-2026-09-27.md` 比 archive 版**少 16 行**——少的正是那段"⚠️ 本文已大量过时"的警示头。

即：**这不是待提交的新内容，而是归档搬迁（提交 `25312c6`，R100 重命名）之后被重新写回工作区的旧副本**。危害：① 同一文档两份，可能改到没人引用的那份；② 缺警示头的那份最容易被误当现状依据；③ 长期挂着的 `??` 会掩盖真正的新增文件。

---

## 四、工程质量评估

### 4.1 优势（真实且不常见）

- **测试是真跑过的**：370 项全绿，且测试写的是"为什么"（如 `user_denial_makes_denied_reachable`、`tool_rounds_alone_do_not_force_cloud` 都是针对真实缺陷的回归）。
- **注释质量高**：几乎每处非显然设计都写了理由与边界，甚至写明"这挡不住什么"。本次审计能在数小时内定位到 H-1/H-2，靠的就是这种诚实。
- **结构性防御意识**：`request_variants_are_gated` 这种"把权限检查固化成门禁"的思路是对的（只是覆盖面不够，见 H-2）。
- **误报率低的设计**：路径白名单 + `canonicalize`、写白名单更窄、服务命令编译期字面量、令牌不进 LLM 上下文——这些判断都对。
- **交付完整**：从内核配置、rootfs、ISO、安装器到官网与部署脚本，是一条能跑通的完整链路，而不是 demo。

### 4.2 短板（结构性的，不是零散 bug）

1. **权限检查是"逐入口手写"的，而不是"按资源声明"的**。项目自己总结过这条教训（`CODE-REVIEW` §2.1），但 H-1/H-2 说明**教训没有转化成机制**：修一个入口，另一个等价入口还开着。真正的解法是让工具/资源**自己声明**敏感度与所需信任级别，由**单一函数**裁决。
2. **安全机制之间缺少交叉校验**。`ui.key`（0600，防读）与 `read_file`（L0，root 代读）分属两个模块，各自都对，合起来就破了。缺少"安全资产清单"与"哪些代码路径能碰到它"的对照检查。
3. **门禁覆盖范围窄于声明范围**。`repo-stats.py --check` 只盖合计；`request_variants_are_gated` 只盖 `Request` 变体。两处都是"门禁通过 ≠ 事实正确"。
4. **文档数字靠手写同步**，而手写同步在多处同时发生时就必然漂移（370/362/356/334/368 五种数字并存）。
5. **特权组件没有降权与沙箱**（全 root），"AI 能操作机器"与"AI 中枢是 root"叠加后，任何一处绕过都是 root 级。

---

## 五、行动建议（按投入产出比排序）

**立刻做（每项 ≤ 1 小时，收益最大）**
1. `READ_DENY_SUBPATHS` 增加 `"ui.key"`（或把 `ui.key` 移到 `/run/aether/`）——**切断 H-1 攻击链**。
2. `ToolCall` 分支对 L1+ 工具要求 `is_ui`——**修复 H-2**。
3. `deploy-site.py` 换成 `vm.py` 的 TOFU 策略；轮换 VPS 口令。
4. README/INDEX 的测试数字统一为 **370**；`repo-stats.py --check` 扩展为**逐 crate 门禁**。
5. 清理 `docs/` 下 5 个残留旧副本。

**本迭代做**
6. `Gate::audit` 的参数脱敏（`clipboard_write` 的 text、`file_write` 的 content 不落原文，只落长度/哈希）+ 审计文件显式 0600。
7. PID 1 全错误路径回落 `rescue_console`（防变砖）。
8. 安装器：isohybrid 校验前置 + `build-iso.sh` 补 isohybrid。
9. `/var` 按 `LABEL=AETHER` 挂载 + fsck + `nodev,nosuid,noexec`；日志写入 `O_NOFOLLOW`。
10. nginx 安全头改 `include` 片段，并用 `curl -sI` 验证响应头真实存在。

**中期（架构级）**
11. 把"权限闸门"从逐入口手写改为**工具声明式**（`sensitivity` + `requires_trusted_channel` + `requires_confirmation`），由单一裁决函数处理；给 `request_variants_are_gated` 加"新增工具必须有声明"的门禁。
12. IPC 从明文 TCP 迁到 Unix socket + `SO_PEERCRED`，令牌绑定确认方 uid。
13. 服务降权（至少 aetherd / compositor 分权），引入 seccomp/cgroup。
14. 建立"安全资产清单"（`ui.key`、`model.json`、审计日志、持久分区）与"可达路径"矩阵，作为审查固定动作。
15. 建 `requirements.txt` 固定 Python 依赖；构建链引入哈希校验与 `--locked`。

---

## 附录 A：本次验证方法与原始证据

| 验证项 | 方法 | 结果 |
|---|---|---|
| 权限模型绕过 | 以当前源码构建 `aetherd.exe`，起 `serve`，用 Python 走真实 NDJSON 协议发 10 条请求 | 见 §2.2 H-1 的 10 行实测输出 |
| 剪贴板闸门绕过 | 同上，未注册连接写入后读回 | 读回明文 `用户刚复制的密码：Hunter2-AETHER`；同连接协议端点 403 |
| Linux 读取策略 | 逐字拷贝 `tools.rs` 的常量与判定函数，用 `rustc` 独立编译运行 | `/var/log/aether/ui.key` → **允许读取** |
| `modelcfg` panic | 独立编译复现 `&k[k.len()-4..]` | `"密钥ab"` → `byte index 4 is not a char boundary` |
| 单元测试 | `cargo test --workspace --offline` | 370 passed / 0 failed |
| 编译警告 | Windows 全量重编；musl `cargo check`（6/7 crate） | 0 warning |
| ISO 引导结构 | 直接读前 512 字节 | 全 0、无 0x55AA、分区表空 ⇒ 非 isohybrid |
| 镜像基线 | 解出 ISO 内 initramfs（58,726,400 B / 1319 项）核对账号/权限/服务/网络 | root 空口令、`/var/log -> ../tmp`、`/tmp` 1777、无 sshd/iptables |
| 文档一致性 | `repo-stats.py` + 独立 Python 换行计数 + 全量逐文件比对 + SHA256 | 见 §三 |
| 依赖与 TLS | 读 `Cargo.lock` + 全仓扫描危险开关 | rustls + ring，证书校验默认开启，零危险开关 |
| 合成器内存安全 | 全文阅读 `wire.rs` / `shm.rs` / `vt.rs` / `pty.rs` + 全仓 `unsafe`/`unwrap` 统计 | 17 处 `unsafe`（fbdev 4 / pty 13）、生产代码 10 `unwrap` + 2 `expect` + 0 `panic!`，逐条核实不可 panic |
| Wayland 是否接生产路径 | 全仓搜 `WaylandServer::bind` / `sync_wayland(` / `attach_pool_data` 调用点 | 仅测试调用 ⇒ 未接运行路径（但已编进二进制） |
| 剪贴板注入链是否存在 | 全仓搜 `ClipboardGet` / `ClipboardText` / `desktop.clipboard =` | 合成器**从不**读取 aetherd 剪贴板 ⇒ 该链不成立（M-13 降级） |

**取证产物**：`D:\var\log\aether\aether-audit.log` 尾部含本次探针的 `allowed` / `rejected_no_ui` 记录（开发机既有文件，非本次新建）。

## 附录 B：未验证项（避免误用）

| # | 未验证项 | 原因 |
|---|---|---|
| 1 | Linux 实机行为：`fbdev` stride/modeset、evdev、`persist` 挂载、`--pid1`、安装全链路 | 本机无 Linux 环境 |
| 2 | `aetherd` 在 `x86_64-unknown-linux-musl` 的编译与警告数 | `ring` 需要 `x86_64-linux-musl-gcc`，本机没有（与 INDEX 自述一致） |
| 3 | Linux 侧真实测试数（文档的 358/333 无法复现） | Windows 不能跑 Linux 二进制；静态推算为 381 |
| 4 | GitHub Release 上那份 ISO 是否 isohybrid | 未下载验证 |
| 5 | 12h43m 长跑 / 3053 轮巡检 / 10 张图 10/10 零差异 / 四轮审查未修项归零 | 超出本次范围，仅确认证据文件存在 |
| 6 | `cargo-audit` 漏洞库比对、busybox setuid 降权行为、内核 `MODULE_SIG`/LSM 配置 | 未执行 |
| 7 | nginx `add_header` 继承语义 | 本机无 nginx，依据官方文档；**建议线上 `curl -sI` 复核** |
| 8 | 日志文件实际权限（umask 推定 0644） | 需 Linux 实机 |
| 9 | `docs/PRODUCTION-PLAN-2026-09-28.md` 声称的"六个硬门禁 6/6 达成" | 未逐条复核 |
| 10 | M-15（`cols == 1` 越界 panic）的**可达性** | 当前几何路径算不出 `cols == 1`（窗口最小宽 320px），但未穷举所有布局/分辨率组合 |
| 11 | M-16（PTY 继承环境变量）在出厂镜像中的可利用性 | 出厂镜像未设 `AETHER_UI_KEY`（已验证），故出厂不可利用；设置该变量的部署**未实测** |
| 12 | 合成器 `fbdev.rs` 的 `bytes_pp`/`line_length` 边界（依赖真实驱动返回值） | 无 Linux 实机与真实 framebuffer |

---

## 附录 C：修复状态（2026-10-02 当日补记）

> 本附录记录**本报告的发现被修复到什么程度**，与上文（审计当时的快照）分开看。
> 每条都已跑过回归：`cargo test --workspace` **385 项全绿**、Windows/musl 双目标
> **0 编译警告**、`repo-stats.py --check`（合计/逐 crate/逐文件三层）与
> `gen-site.py --check`（9 个页面）全绿。

| 发现 | 状态 | 提交 | 说明 |
|---|---|---|---|
| **H-1** UI 密钥泄露链 | ✅ 已修 | `99971ea` | `READ_DENY_SUBPATHS` 增加 `ui.key`/`model.json`；密钥迁到 `/run/aether/ui.key`；**实测**：修复前未注册连接可 `read_file` 取密钥，修复后拒绝 |
| **H-2** 剪贴板闸门绕过 | ✅ 已修 | `99971ea` | `TRUSTED_CHANNEL_TOOLS` 单一判定表，`ToolCall` 与 `agent_run` 共用；**实测**：未注册连接读写剪贴板均 403，已注册通道仍可用 |
| **H-3** `/var` 挂载无身份校验 | ✅ 已修 | `f5e0e97` | ext4 卷标（超块偏移 1024+0x78）判定 + 挂载前 `fsck -p` + `nodev,nosuid,noexec`；纯函数单测 |
| **H-4** 日志符号链接 | ✅ 已修 | `f5e0e97` | `symlink_metadata` 预检 + Linux `O_NOFOLLOW`；日志目录 0700 |
| **H-5** 部署脚本 `AutoAddPolicy` | ❌ **未修** | — | 需要 VPS 凭据与生产窗口；修法已在 §2.2 写明（复用 `vm.py` 的 TOFU 策略 + 非 root + 轮换口令） |
| **H-6** 安装器先 dd 后校验 | ⚠️ **代码已修，产物未重建** | `365bc26` | 校验改为**动盘前**执行 + `build-iso.sh` 补 isohybrid 与自检。但**实测已发布的 v0.1.0 资产同样非 isohybrid**（见下），装机要等重新构建并上传 |
| M-1 审计明文 | ✅ 已修 | `99971ea` + 审计补漏 | 执行路径与**用户拒绝路径**共用同一套脱敏（自查发现拒绝路径原本漏了） |
| M-2 审计文件权限 | ✅ 已修 | `99971ea` | 0600 / 目录 0700（含旧文件收紧） |
| M-3 PID 1 不回落救援 | ✅ 已修 | `365bc26` | `pid1_or_rescue()` 返回 `!`，兜住错误与 panic |
| M-4 空口令 / 串口即 root | ❌ 未修 | — | 属镜像基线决策（要不要设 root 口令、是否关串口控制台），需要产品判断 |
| M-5 服务全 root | ❌ 未修 | — | 架构级（需要引入用户/沙箱模型），不在本轮范围 |
| M-6 ops 日志偏移不推进 | ❌ 未修 | — | 修法明确（非 UTF-8 时也推进偏移 + 读取上限），本轮未做 |
| M-7 安装器 `--disk` 未规范化 | ❌ 未修 | — | 修法明确（`canonicalize` + 复用 `valid_disk_path`），本轮未做 |
| M-8 不擦盘 / `/var/apps` 变空 | ❌ 未修 | — | 属产品行为决策 |
| M-9 构建链无校验 | ❌ 未修 | — | 需要构建机侧改造 |
| M-10 nginx 安全头静默失效 | ✅ 已修 | `365bc26` | `deploy/security-headers.conf` 片段 + 每层 include；`deploy-site.py` 部署后**回读真实响应**校验（**第三遍审计发现这个自检只告警不失败，已改成不达标即 `sys.exit(1)`**） |
| M-11 ISO 无校验 | ✅ 已修 | `365bc26` + `dfbc90f` | 下载脚本内置哈希；**并发现站点公布的哈希与实际下载不符**（见下）；第三遍审计又加了"脚本哈希 == release.json"的门禁 |
| M-12 `summary()` 非 ASCII panic | ✅ 已修 | `99971ea` | 改按字符取末 4 位；新增多字节用例 |
| M-13 终端粘贴即执行 | ⚠️ **部分** | `365bc26` | 控制字符剥离 + 跟踪 DECSET 2004 的括号粘贴**都已实现**；但出厂镜像只有 busybox（`aetheros_defconfig` 无 bash），`/bin/sh` 若不发 2004 就永远走旧路径 —— **需在 guest 里实测多行粘贴**。第三遍审计指出这一点，措辞已下调 |
| M-14 VT 参数无上限 | ✅ 已修 | `f5e0e97` | `MAX_CSI_PARAMS = 32` |
| M-15 宽字符越界 panic | ✅ 已修 | `f5e0e97` | `cells.get_mut(idx + 1)` |
| M-16 PTY 继承合成器环境 | ✅ 已修 | `f5e0e97` | `env_clear()` + 显式白名单 |
| M-17 注册先发密钥 | ✅ 已修 | `f5e0e97` | Unix socket（0600）优先且**不回退 TCP**；显式 `AETHER_IPC_TCP=1` 才走 TCP。残余：服务端仍无 `SO_PEERCRED`，仓库自带 e2e 脚本仍走 TCP（仅 root 能读密钥 ⇒ 不可利用） |
| L-1 ~ L-16 | ⚠️ 部分 | — | 仅修了与上述高危/中危同源的（日志权限、控制字符等）；**L-5 在第三遍审计被证明危害更高，已修**（见附录 D） |
| 文档一致性（§三） | ✅ 已修 | `dfbc90f` | 数字统一 + 门禁补到三层（含负向测试）；第三遍审计又补了"集合相等"断言与站点事实重测 |
| `docs/` 5 个残留旧副本 | ✅ 已清理 | *（不在 git 历史里）* | 那 5 个文件**从未被 git 跟踪**，删除发生在工作区层面，`git log --diff-filter=D` 查不到 —— 第三遍审计指出本表原先把它归到 `dfbc90f`，属**归因错误**，已更正 |

### 修复过程中新发现的问题（不在原报告里）
1. **站点公布的 SHA256 与用户实际下载的文件对不上**。site 的 ISO 三要素由
   `gen-site.py` 从**本地**构建产物测出（40,359,936 字节 / `43c53f4c…`），而 Release
   资产是 **40,327,168 字节 / `7c50f481…`** —— 官网让用户拿一个必然失败的哈希去校验。
   修法：新增 `site/release.json` 作为"已发布事实"的单一来源，`gen-site.py` 优先采用；
   下载脚本的期望哈希同步改为资产值。
2. **站点写的 ISO 文件名是错的**：8 处写作 `aetheros-v0.1.0-amd64.iso`，实际资产名是
   `aetheros-0.1-amd64.iso`（照抄站点命令会找不到文件）。
3. **已发布的 v0.1.0 ISO 同样不是 isohybrid**（2026-10-02 实际下载复核：前 512 字节
   全 0、无 MBR 签名）。因此 README/CHANGELOG/下载页"可安装到虚拟磁盘后独立引导"
   对本版本**不成立**，已加更正；装机需重新构建。
4. **站点"17 张界面走查图"是手写常量**，与门禁清单的 19 张长期不一致（同 `iso_bytes`
   一类问题：`patch_values` 只做"上次值 → 本次值"，常量一旦脱节就永远补不上）。
   修法：改为从 `archive-ui-shots.py` 的清单计数，并纳入 `--check` 必现项。

### 仍未验证（与 §附录 B 一致，且本轮改动新增两条）

- `aetherd` 的 `x86_64-unknown-linux-musl` 编译（`ring` 缺 `x86_64-linux-musl-gcc`）；
  本轮新增的 Unix socket 代码用**一一对应的 API 探针**在可 musl 检查的 crate 上验证过，
  但**未在目标机实跑**。
- 批次 B/D 的 Linux 专属行为（`fsck` 实际调用、`O_NOFOLLOW` 生效、括号粘贴的真实 shell
  行为、Unix socket 连接）**均需在构建机/实机复核**。

---

## 附录 D：第三遍审计（独立对抗审查）发现与处置

> 前面两遍由修复方自查，**第三遍由独立审查代理执行**（只读、不含修复方上下文），
> 目的正是抓"自己修完自己评"的盲区。结果证明这一步不可省：它推翻了本报告附录 C 中
> **4 条 ✅**，并指出 2 处"修好一个问题、引入另一个问题"。下表逐条给出处置。

### D.1 被推翻的"已修"结论（全部为真实缺陷）

| # | 发现 | 为什么自查没抓到 | 处置 |
|---|---|---|---|
| 1 | **M-1 只堵了审计日志，同源明文仍落盘**（高危 · 可利用）：`aetherd/src/main.rs` 的 `eprintln!("工具调用: {name} {args}")` 未脱敏 → 经 aether-init 的 logtee 写进 `/var/log/aether/aetherd.log` → 而该目录在 `read_file` 白名单里、`read_file` 是 L0 免确认 ⇒ 任意本机进程可读回明文 | 自查只审了"审计日志"这一个 sink，没顺着 `eprintln` → logtee → 白名单 走一遍数据流 | ✅ 已修：该行改用 `tools::audit_args`；并把 `Gate::audit` 拆成 `audit_call`（收 `&Value`，内部脱敏）/ `audit_note`（无参数），原 `audit` 变私有 —— **类型层面不可能再漏** |
| 2 | **H-3 的 `noexec` 打断了"装应用"**（高危 · 功能回归）：`/var` 是持久分区挂载点，而已装应用住在 `/var/apps`，包装脚本要 `exec /var/apps/<id>/<bin>` 且 `dlopen` 同目录 `lib/`；`MS_NOEXEC` 同时挡 execve 与 PROT_EXEC 映射 ⇒ 装机后（`persist` 默认开）应用全部跑不起来 | 只想了"这个分区放日志"，**没有核对分区上还放了什么** | ✅ 已修：挂载选项改为 `nodev,nosuid`，注释写明真实理由与残余风险，并加"**不得出现 noexec**"的回归断言 |
| 3 | **H-4 只修了 logtee 一侧，审计日志整条没修**：`Gate::audit` 打开审计文件无 `O_NOFOLLOW`，`rotate_if_needed` 同样跟随链接 | 把 H-4 当成"日志管道"的问题，没意识到审计写入是**同一类**问题 | ✅ 已修：审计路径加符号链接预检；`rotate` 不引入新面 |
| 4 | **新引入：`set_permissions(0700)` 跟随符号链接 → root chmod 任意目录**（中危）：`/var/log/aether` 若被预置成指向 `/etc` 的软链，root 会把 `/etc` 改成 0700 | 加"收紧权限"时没想到 chmod 会跟随链接 | ✅ 已修：aether-init 用 `O_NOFOLLOW\|O_DIRECTORY` + fchmod（无竞态）；aetherd 用 `symlink_metadata` 预检并**去掉了对已存在文件的 chmod**（不再提供"root 改任意文件权限"的原语） |

### D.2 仍然可绕过 / 未修的（第三遍新增结论）

| # | 发现 | 处置 |
|---|---|---|
| 5 | **`read_file` 是 H-2 的等价入口**（高危 · 可利用）：与 `clipboard_read` 同为 `sensitive_output`，却 L0 免确认、未注册可调、以 root 读 `/home` 等白名单 | ✅ 已修：纳入 `TRUSTED_CHANNEL_TOOLS`（合成器始终先注册，正常使用不受影响；`aetherd chat` 这类无 UI 命令行用法有意收紧） |
| 6 | **连接无写超时**（中危 · 可用性）：对端"连上不读"会让 `write_all` 永久阻塞，32 条即占满连接槽，合成器再也连不进来 | ✅ 已修：TCP 与 Unix 两条 accept 路径都设 `set_write_timeout` |
| 7 | **`verify_security_headers()` 永远不会失败**（中危）：只 `print("⚠️ …")`，`main()` 照常返回 0 —— M-10 的"自检"退化成提示 | ✅ 已修：不达标即 `sys.exit(1)` |
| 8 | **门禁三处结构漏洞**（中危）：逐 crate 层删一行不报错；逐文件层无条数断言（删行/格式漂移静默脱管）；仓库新增文件不在表里也检测不到 | ✅ 已修：三层都改成**集合相等**断言（并补齐 INDEX 里漏登记的 3 个单文件 crate，核对条数 42 → 45）；做了 3 项负向测试（删汇总行 / 删文件行 / 新增文件未登记）全部变红 |
| 9 | **`gen-site.py --check` 不重测任何事实**（中危）：只验"页面 == data.json"，改了 `release.json` 或代码规模而忘了 `--refresh` 仍全绿；`need` 只盖 4/9 页且是非锚定子串匹配 | ✅ 已修：`--check` 重新测量（ISO 事实/规模/走查图张数）并与 data.json 比对；`need` 扩到 faq 页；匹配改为**带量词的锚定正则**（中英各一套量词）；`count_archived_shots()` 命中 0 改为报错而非静默沿用旧值 |
| 10 | **ISO SHA256 四处副本无门禁**（中危）：`try-aether.sh` / `.ps1` / `release.json` / `data.json` 各一份，漏改一处会拒收合法下载 | ✅ 已修：`--check` 断言两个脚本里的哈希等于 `release.json`（负向测试已验证） |
| 11 | **`scripts/mkapp.py` 的 `--id ..`**（原报告 L-5，实际危害更高）：字符白名单含 `.` ⇒ `--id ..` 通过 → `shutil.rmtree(Path(out)/"..")` **删掉 --out 的父目录**；`--entry` 同样可写出包外 | ✅ 已修：`valid_app_id`（首字符必须字母数字）+ `valid_rel_path`（拒绝绝对路径与 `.`/`..` 组件），并在 `--selftest` 里加了 15 项断言 |
| 12 | **`fsck_before_mount` 是 fail-open**（中危）：工具缺失/执行失败都返回 true，与"三道门缺一不可"的表述不符 | ⚠️ 保留 fail-open，**但把理由与后果写进代码注释**：镜像 defconfig 含 e2fsprogs（工具在），而 fail-closed 会让"缺工具"变成"系统彻底失去持久化"。这是有意的取舍，不是疏漏 |
| 13 | **H-3 身份判据可伪造**（高危，前提 = 能预置介质）：只验 ext4 魔数 + 卷标，攻击者自造 `mkfs.ext4 -L AETHER` 的分区即可 | ❌ **未修**。彻底修法要在装机时把分区 UUID/凭据写到只读启动介质，属安装器+init 的联合改造，超出本轮范围；已在代码注释与本表标注为**已知残余** |
| 14 | **Unix socket 建立失败时静默降级为 TCP-only**（中危 · 可用性）：`/run` 只读或路径被占成目录时，桌面会失去确认/剪贴板能力且只有一行日志 | ❌ **未修**（安全影响低：需 root 或只读 `/run`）。修法：把 socket 失败升级为致命错误或重试 |
| 15 | **`audit_args` 是字段名白名单**（低危）：新增携带内容的字段即失效 | ⚠️ **部分修**：入口已收敛为 `audit_call(&Value)`（调用方无法传明文），但字段白名单本身仍是"默认放行"。彻底修法是"默认脱敏 + 显式声明可记录"，属行为变更，未做 |
| 16 | **M-13 对默认 shell 可能无效**（中危）：镜像无 bash，busybox ash 若不发 DECSET 2004 则括号粘贴永不触发 | ⚠️ 代码正确但**未在 guest 验证**；状态已从 ✅ 下调为 ⚠️ |
| 17 | **测试有效性**：16 条新增测试里，至少 4 条在对应修复被回滚时**不会变红**（M-1 的"接线"、H-4 的 `O_NOFOLLOW` 开关、H-6 的校验时机、H-3 的挂载选项实参） | ⚠️ 部分改善：M-1 的接线现在由 `audit_call` 的类型约束保证（不可能回滚成明文），拒绝路径也进了 e2e 第 12 项；H-3 的挂载选项加了"不得含 noexec"断言。**H-6 的时机与 `O_NOFOLLOW` 开关仍只能靠人工审查** |
| 18 | **仓库无 CI**：`cargo test`、两个门禁全靠手工；所有 `#[cfg(unix)]`/`cfg(linux)` 断言在 Windows 开发机上**从不执行** | ❌ **未修**（需要 CI 环境）。当前缓解：README 已如实标注"Linux 侧未实测"，Linux 专属断言全部在构建机复核清单里 |

### D.3 第三遍审计给出的正面复核（避免误伤）

- `pid1_or_rescue() -> !` 用类型系统兜住"永不返回"，且 `catch_unwind` 有效的前提（未设 `panic=abort`）成立 —— **高质量修复**。
- `H-2` 的单一判定表 + "每个工具必须分类"的数量对账，是"按资源判定"的正确解法。
- `spawn_unix_listener` 的 `remove_file` 先行序、socket `bind→chmod` 窗口、`READ_DENY_SUBPATHS` 的大小写/`..`/符号链接绕过、审计日志换行注入 —— **逐条核查后均不成立**（详见审查记录），未做无谓改动。
- 批次 A–D 的提交信息经独立复算**无夸大**（385/0 警告/门禁三层在 HEAD 态全绿）；`audit()` 用 `opts.mode(0o600)` 在创建期收紧权限，**没有** TOCTOU。

---

## 附录 E：全量处置表（后续批次 E–H 之后）

> 本附录是**整份报告所有发现**的最终处置状态。附录 C/D 记录的是当时那一刻的状态，
> 这里给出收尾后的完整账。提交对应：
> `ec0ce9a`（E·交付链）/ `f987264`（F·安装器与镜像）/ `83ca7ab`（G·进程与 IPC）/
> `f8902cf`（H·收尾）。
>
> ⚠️ 全程遵守一条硬约束：**只做代码级修复与本地验证，不对真实机器、真实磁盘或生产主机
> 执行安装、刷写、部署**。因此凡涉及真机的验证（装机、持久分区、串口控制台、VPS 部署）
> 一律标注为"未实机验证"，代码修复本身不依赖这些操作。

### 高危（6 条）

| 编号 | 状态 | 提交 | 说明与残余 |
|---|---|---|---|
| H-1 UI 密钥泄露链 | ✅ 已修 | A | 密钥迁 `/run/aether/ui.key` + `READ_DENY_SUBPATHS` 拒绝；实测复现前后对照 |
| H-2 剪贴板闸门绕过 | ✅ 已修 | A + G | 单一判定表 + `read_file` 纳入；两处入口共用同一函数 |
| H-3 `/var` 挂载无身份校验 | ✅ 已修 | D + F + H | 卷标 → **只读探测安装标记** → fsck → rw 挂载；`nodev,nosuid,errors=remount-ro`（`errors` 是 H 批次加的）。残余：判据可伪造（需能预置介质），彻底修法要安全启动/TPM |
| H-4 日志/审计符号链接 | ✅ 已修 | D + 后续 | logtee `O_NOFOLLOW`；审计写入同样加符号链接预检 |
| H-5 部署脚本 AutoAddPolicy | ✅ **代码已修**（未部署） | E | TOFU 主机密钥 + 默认非 root + 默认密钥认证 + `sudo -n` 提权（8 处）。**没有对生产机执行过部署** |
| H-6 安装器先 dd 后校验 | ⚠️ 代码已修，**产物待重建** | B + E + F | 校验前置 + `build-iso.sh` 补 isohybrid 与自检（B）+ 产出 `.sha256`（E）+ 安装标记与镜像基线（F）。已发布的 v0.1.0 资产实测仍非 isohybrid ⇒ 装机要等重新构建上传 |

### 中危（17 条）

| 编号 | 状态 | 提交 | 说明与残余 |
|---|---|---|---|
| M-1 审计明文 | ✅ 已修 | A + 后续 + H | 执行路径 + 拒绝路径 + stderr 一路脱敏；入口收敛为 `Gate::audit_call(&Value)`；**H 起改为默认脱敏**（未登记字段一律脱敏） |
| M-2 审计文件权限 | ✅ 已修 | A + 后续 | 0600/0700，且 chmod 不再跟随符号链接 |
| M-3 PID 1 不回落救援 | ✅ 已修 | B | `pid1_or_rescue() -> !`，类型系统兜住"永不返回" |
| M-4 空口令/串口即 root | ✅ 已修（含上锁开关） | F | `/etc/issue` + 启动横幅明确写出；构建时设 `AETHER_ROOT_PW_HASH` 即需登录。**未在真机验证登录行为** |
| M-5 服务全 root | ✅ 机制落地 | G | `ServiceSpec.user/groups` + exec 前 setgid→setuid + 解析单测；5 个服务**显式**声明 `root` 并有测试钉住。诚实说明：当前 5 个确实都需要 root，本步产出是机制与可见性 |
| M-6 ops 偏移不推进 | ✅ 已修 | F | 按字节读 + 有损解码，坏字节不再让文件永久失明。该测试本身**没有平台门**，是整个 `aether-ops` crate 在 Windows 上不参与编译（`ServiceStatus` 少字段），所以本机不执行 —— 由 CI 的 Linux 任务覆盖 |
| M-7 `--disk` 未规范化 | ✅ 已修 | F | `canonicalize_disk` + `same_device`（含回归测试） |
| M-8 不擦盘 / 骨架缺 apps | ✅ 已修 | F | `--wipe`（默认关）+ 每次 `wipefs -a` + 骨架补 `apps` |
| M-9 构建链无校验 | ✅ 已修 | E | `--locked` + Buildroot 压缩包 TOFU 哈希对账 + 产出 ISO `.sha256` |
| M-10 nginx 安全头静默失效 | ✅ 已修 | B + 后续 | 片段 include + 部署后回读响应；**自检不达标现在会 `sys.exit(1)`** |
| M-11 ISO 无校验 | ✅ 已修 | B + C + 后续 | 脚本内置哈希 + `release.json` 单一来源 + "脚本哈希 == 发布资产"门禁 |
| M-12 `summary()` panic | ✅ 已修 | A | 按字符取末 4 位 + 多字节用例 |
| M-13 粘贴即执行 | ⚠️ 代码已修，**待真机确认** | B + G | 控制字符剥离 + 括号粘贴；镜像加 bash（**注意：`BR2_PACKAGE_BASH` 需要同时开 `BR2_PACKAGE_BUSYBOX_SHOW_OTHERS`，否则 kconfig 会丢掉它** —— 已补）且 compositor 优先选它。busybox 的 2004 支持**未实机验证** |
| M-14 VT 参数无上限 | ✅ 已修 | D | `MAX_CSI_PARAMS = 32` |
| M-15 宽字符越界 | ✅ 已修 | D | `cells.get_mut(idx + 1)` |
| M-16 PTY 继承环境 | ✅ 已修 | D | `env_clear()` + 显式白名单 |
| M-17 注册先发密钥 | ✅ 已修 | D + 后续 + H | 客户端默认只走 0600 Unix socket 且**不回退 TCP**；socket 建不起来时 aetherd **直接失败退出**（不再静默降级）。残余：**aether-init 侧有** `SO_PEERCRED`（L-3），**aetherd 侧没有** —— 因为它的 socket 已经是 0600 root-only，在没有用户模型之前该检查恒真、收益为零 |

### 低危（16 条）

| 编号 | 状态 | 提交 | 说明 |
|---|---|---|---|
| L-1 日志无脱敏全量 tee | ✅ 已修 | G | `sanitize_for_display`（CSI/OSC/C0/DEL），文件与控制台都净化，8 项单测。**字节级那份不丢 C1 字节**（0x80–0x9F）—— 那会把中文的 UTF-8 续字节切碎；字符级那份（`sanitize_for_tty`）会丢 C1，因为那里输入已是合法 UTF-8。两条理由都写在代码里 |
| L-2 ops 跟随符号链接 | ✅ 已修 | G | `symlink_metadata` 判定后跳过 |
| L-3 init IPC 无对端校验/无上限 | ✅ 已修 | G | `SO_PEERCRED`（fail-closed）+ 连接上限 16 + 读超时 + accept 失败不空转 |
| L-4 特权二进制未 strip | ✅ 已修 | H | `[profile.release] strip = "symbols"`。代价：`RUST_BACKTRACE=1` 退化为纯地址（panic 消息本身不受影响，它用的是编译期字符串）；"靠 debug 版复现"还要求构建环境一致 |
| L-5 `mkapp.py` 路径穿越 | ✅ 已修 | 后续 | `valid_app_id` / `valid_rel_path` + 15 项 selftest |
| L-6 用 assert 做完整性校验 | ✅ 已修 | E | 5 个脚本改为显式判断 + 抛错（`-O` 下不再静默消失） |
| L-7 QMP/VNC 无认证 | ✅ 已修（QMP 侧） | E | QMP 本就是 Unix socket，补 `chmod 600`；VNC 保持只绑回环 —— **不加口令**是因为 VNC 经典认证基于 DES（本项目禁用），已在脚本里写明理由 |
| L-8 VM 口令走命令行 | ✅ 已修 | E | 改 `--password-file` + 临时文件，用完即删 |
| L-9 `X-SHA256` 无人校验 | ✅ 已修 | E + H | 新增 `aetherd app verify`（ring）+ 文档流程改为"先校验再解包"；与 `sha256sum` 逐字节一致（公开向量测试 + 运行时实测）。**H 起不给 `--sha256` 会按用法错误退出（码 2）** —— 否则"没校验"的退出码 0 会被脚本当成通过 |
| L-10 站点链接未校验协议 | ✅ 已修 | E | 协议白名单 + 拒绝引号/控制字符；新增 12 项 `--selftest` |
| L-11 `scripts/` ACL 过宽 | ⚠️ **只记录，未修** | — | 这是**宿主环境**问题（Windows ACL），仓库里改不了；需在机器上 `icacls` 收紧。已在此表登记 |
| L-12 dd 无 `conv=fsync` | ✅ 已修 | F | 加上（"dd 成功"不再只代表进了页缓存） |
| L-13 /dev/tty0 未过滤转义 | ✅ 已修 | G + 审查后 | `sanitize_for_tty`（并纳入 C1）；**此前注释自称"两处都有各自的单测"与事实不符** —— 现已补测试（`sanitize_for_tty_strips_escapes_and_c1`） |
| L-14 6 处 read_line 无上限 | ✅ 已修 | G + 审查后 | `read_line_capped`（1MiB）+ AI 回复 4MiB 上限；边界（恰好上限 / 上限+1 / 带不带换行）**此前零测试**，现已补两份（合成器与 aetherd） |
| L-15 PTY O_CLOEXEC/fcntl/SIGHUP | ✅ 已修 | G | 三项全改（含 PID 复用防护） |
| L-16 Wayland 会话层无限 | ✅ 已修（未接线代码） | G | 跨读重组（16KB 缓冲 < 64KB 消息）+ 连接上限 16 + 对象表上限 512。`destroy` 清理仍未做（对象表已有上限，不再无界） |

### 信息级（10 条）

| 编号 | 状态 | 说明 |
|---|---|---|
| I-1 服务白名单是编译期枚举（正面） | — | 无需处置 |
| I-2 busybox 04755 setuid | ✅ 已修 | 新增 Buildroot 后处理脚本去掉 setuid 位（镜像无普通用户，不需要它） |
| I-3 `/etc/inittab` 兜底 root shell | ⚠️ 部分 | `/init` 缺失是"系统已坏"的路径，保留 shell 是有意的救援设计；已通过 `/etc/issue` 与启动横幅明确"控制台即 root"，并提供上锁开关（同 M-4）。**未实机验证** |
| I-4 镜像指纹仍是 Buildroot | ✅ 已修 | 后处理写入 hostname / os-release |
| I-5 无 sshd/防火墙/SELinux/更新通道 | ⚠️ 记录 | 属产品范围决策（要不要远程管理、要不要强制访问控制、要不要更新通道），不在本轮 |
| I-6 `AETHER_BIND` 可暴露 IPC | ✅ 已修 | 非回环需再显式设 `AETHER_ALLOW_REMOTE_IPC=1`，否则拒绝启动。**诚实说明**：这是"两个环境变量门"，不是交互式确认 —— 能改环境的人也能改第二个变量；它的价值是把"手滑暴露"变成"必须刻意两次"，并让启动日志留下明确记录 |
| I-7 ipc-protocol 文档与实现不符 | ✅ 已修 | 文档改为"双通道（socket 优先 + TCP 备用）" |
| I-8 安装器注释称 isohybrid 但构建无此步 | ✅ 已修 | `build-iso.sh` 已补该步骤，注释与实现一致 |
| I-9 `auto_approve_below` 是 pub | ✅ 已修 | 转私有 + 只读访问器，并在启动日志打印实际阈值 |
| I-10 `take()` 是连接级累计上限 | ✅ 已修 | H + 审查后 | 改为按行限长（`read_line_capped`）。**代码审查指出 H 只修了 aetherd 与合成器两处**，`aether-init`（PID 1）仍是连接级累计 —— 现已一并改掉，三处一致 |

### 仍标注"未实机验证"的清单（不得当作已验证）

1. 装机全流程（含新的卷标/标记/fsck/挂载选项）—— 需构建机 + 虚拟盘。
2. `/var` 持久分区的实际挂载行为与 `errors=remount-ro` 生效。
3. 串口/控制台登录行为与 `AETHER_ROOT_PW_HASH` 上锁效果。
4. 括号粘贴在 bash 下的实际表现（M-13）。
5. `aetherd` 的 musl 交叉编译（ring 需要 musl-gcc）；其 Unix socket 代码用 API 探针在
   可检查的 crate 上验证过，但**未在目标机实跑**。
6. 生产部署脚本的全部改动（H-5）—— 按约束**没有执行任何部署**。
7. CI 工作流（`.github/workflows/ci.yml`）本地无法验证，首次真跑可能立刻暴露
   Linux 侧从未执行过的断言。

---

## 附录 F：第二遍独立审查（批次 E–H）的处置

> 附录 E 写完后又做了一遍**独立对抗审查**（只读、不含修复方上下文）。它推翻了
> 附录 E 里若干"✅"，也指出报告本身有夸大与事实错误。下表是逐条处置 ——
> **本附录的结论优先于附录 E**（附录 E 保留原样，便于对照"当时以为的状态"）。

### F.1 被推翻或需修正的修复（全部已改）

| # | 发现 | 处置 |
|---|---|---|
| 1 | **`audit_args` 非对象参数整串明文**（中危·可利用）：模型可传字符串 `arguments`，`args.to_string()` 直接进审计与 stderr | ✅ 非对象参数一律脱敏（带长度与指纹），并加测试 |
| 2 | **指纹可离线枚举**（低危）：无盐 FNV-1a + 精确长度 ⇒ 短秘密（PIN/口令片段）可字典校验 | ✅ 改为**进程级随机盐 + SHA-256 前 8 字节**；同一次运行内可关联，跨重启不可比对（有意取舍，已写明） |
| 3 | **M-5 默认路径不清附加组**（机制失效）：`setgroups` 只在 `groups` 非空时调用 ⇒ 降权后仍是组 0 成员；多组只保留最后一个 | ✅ 先收集 gids 再一次 `setgroups(len, ptr)`（空列表即清空），组不存在则拒绝启动；`/etc/passwd` 读失败改为报错而不是静默空文本 |
| 4 | **H-3 探测吞掉卸载失败**（可用性回归）：残留挂载点会让持久分区**永久挂不上**且无根因日志 | ✅ 探测前先清理、卸载失败即判定探测失败并打印原因；标记要求是**普通文件**（`is_file()` 跟随软链可被 `log/install-id -> boot.log` 骗过） |
| 5 | **`/etc/shadow` 会进仓库**（中危·新引入）：`build-iso.sh` 把口令哈希写进**已被 git 跟踪**的 overlay 目录 | ✅ 改由 post-build 脚本写入**构建产物**（哈希经环境变量传递，不落仓库）；同时 `.gitignore` 加该路径 |
| 6 | **I-10 只修了两处**：`aether-init` 仍是连接级累计 `take`（超长行静默截断、正常长连接被关） | ✅ 同样改成按行限长（三处一致） |
| 7 | **`errors=remount-ro` 之后静默停写**：logtee 的 `let _ = write_all` 丢弃 EROFS，文件日志悄悄停止 | ✅ 写失败报一次（去重）并放弃文件侧、控制台继续；提示里点明"分区是否已转只读" |
| 8 | **`same_device` 用路径比较**：设备别名（`/dev` 内 bind mount、hardlink）可绕过 | ✅ 改用 **`st_rdev` 设备号**比较（内核给的唯一标识），非设备节点才回退路径比较 |
| 9 | **`resolve_bind` 回环判定是三个字面量**：`127.0.0.2`/`127.1` 等合法回环被误拒 | ✅ 解析成 `IpAddr` 后用 `is_loopback()`；解析不了按非回环处理（保守） |
| 10 | **`sanitize_for_tty` 放行 C1 且零测试**（注释自称有单测，与事实不符） | ✅ 字符级实现改为同时丢弃 C1（U+0080–U+009F），移出 `cfg(linux)` 使其可测，并补测试。**字节级那份仍不丢 C1 字节** —— 那会把中文（UTF-8 续字节在 0x80–0xBF）切碎，已在代码里写明理由 |
| 11 | **M-9 的 TOFU 可绕过**：对账被包在 `if [ ! -d buildroot-* ]` 里，解包目录存在即不对账 | ✅ 对账移到条件之外：只要 `br.tar.gz` 在就核对 |
| 12 | **L-9 不给 `--sha256` 时返回 0**：文档说"校验通过才解包"，而退出码 0 会被脚本当成通过 | ✅ 不给期望值按用法错误处理（退出码 2），强制带期望值 |
| 13 | **`.vps_known_hosts` 实际没进 `.gitignore`**（提交信息声称已加，但那次编辑失败了） | ✅ 已加，并用 `git check-ignore` 实测确认 |
| 14 | **CI 不带 `--locked`**：`Cargo.lock` 漂移会被静默接受，与 M-9 的主张矛盾 | ✅ `gates.py` 的 build/test/musl 三步都加 `--locked` |
| 15 | **构建脚本零门禁**：`post-build.sh` / `overlay/init` 不在任何检查里 | ✅ `gates.py` 新增 shell 语法检查（本机用 POSIX shell，Linux CI 用 `/bin/sh`）；musl 被跳过时改为醒目提示"这一步没有执行" |
| 16 | **权限位未进 git**：`/init` 与 post-build 脚本在 git 里是 0644 —— 全新克隆后 `/init` **不可执行（引导失败）**、Buildroot 后处理脚本不可执行（构建失败） | ✅ 43 个 shell 脚本（含 `/init`、post-build、build-iso）在 git 索引里设为 0755 |

### F.2 测试补强（针对"回滚不变红"）

第二遍审查列出**至少 12 条修复在回滚时不会变红**。本附录处置后补了这些：

| 补的测试 | 覆盖的修复 |
|---|---|
| `audit_args_redacts_unknown_string_fields_by_default`（含非对象参数、嵌套对象、数组、超长白名单字段、指纹一致性） | 默认脱敏的全部绕过路径 |
| `read_line_capped_boundaries`（aetherd 与 compositor 各一份：恰好上限 / 上限+1 / 带不带换行 / 空输入） | L-14 与 I-10 的限长 |
| `sanitize_for_tty_strips_escapes_and_c1` | L-13 |
| `resolve_user_and_group_from_text`、`shipped_services_declare_their_user` | M-5 的解析与"必须声明" |
| `persist_label_and_mount_opts_follow_contract`（含 `errors=remount-ro`、不含 `noexec`） | H-3 的挂载选项 |
| `sha256_matches_published_vectors`（公开向量，非同源对比） | L-9 的算法正确性 |
| 门禁负向测试（删汇总行 / 删文件行 / 新增文件未登记 / 站点非发布哈希 / 脚本哈希不符） | 门禁本身不是空转 |

**仍然没有测试、只能靠人工审查的**（已在附录 E 的"未实机验证"清单里，这里补全）：
`has_install_marker` 的三条失败分支、`spawn_with_logs` 的降权链路、
`peer_uid`（SO_PEERCRED）、连接上限、`resolve_bind`（内含 `process::exit`，不可单测）、
`aetherd app verify` 的 CLI 退出码分支、`post-build.sh` 的实际执行效果。

### F.3 报告本身的更正（诚实性）

* 附录 E 里 **I-6 的"二次确认"实际只是第二个环境变量**，不是交互确认 —— 已按实情改写；
* **M-17 的残余表述**易被误读：`SO_PEERCRED` 在 **aether-init 侧有**、aetherd 侧没有；
* **M-6 的"测试是 Linux 专属，本机不执行"**：该测试本身无 `#[cfg]` 门，
  是**整个 crate** 在 Windows 上不参与编译 —— 结论相同，措辞已更正；
* **L-4 的代价**补全：`strip = "symbols"` 会让 `RUST_BACKTRACE=1` 退化为纯地址，
  "靠 debug 版复现"还要求构建环境一致；
* **M-9 的 TOFU**不是密码学意义上的完整性保证（无签名），只是"变了就停"的对账；
* 附录 E 当时**未提交**（只存在于工作区），第二遍审查据此判定"报告状态 ≠ 仓库状态"——
  本轮已连同全部修复一起提交。

---

## 附录 G：第三/四遍审计（收尾自查 + 全量门禁实测）的处置

> 按规程"所有任务全部完成后整体执行 3 遍审计"，这里记录后两遍的结果。
> 结论：**没有新的安全发现**，但抓到两处"门禁给虚假保证"和一处环境问题。

### G.1 第三遍：负向测试逐个验证新门禁

做法：对每个新门禁**人为改坏输入**，断言它必须变红（跑完还原，`git status` 必须干净）。
8 项里 7 项按预期变红，**1 项是"看着守住了其实没有"**：

| 门禁 | 破坏方式 | 结果 |
|---|---|---|
| 测试分布表（属性数） | 把 `aether-ops` 属性数改成 99 | ✅ 变红 |
| 测试分布表（合计） | 把属性合计改成 999 | ✅ 变红 |
| 规模声明 | 改掉 README 的合计行 | ✅ 变红 |
| shell 语法 | 让 `post-build.sh` 的 `if` 不闭合 | ✅ 变红 |
| **站点（data.json）** | **改坏 `rust_lines_num`** | ❌ **没变红** → 已修 |
| 站点生成器自检 | 把 `javascript:` 加进协议白名单 | ✅ 变红 |
| 打包器自检 | 削弱 `valid_app_id` | ✅ 变红 |
| `.gitignore` | 检查 TOFU 记录与 `/etc/shadow` 是否真被忽略 | ✅ 忽略生效 |

**修的问题**：`gen-site.py --check` 只比对 5 个手挑的键
（`iso_bytes`/`iso_sha256`/`rust_lines`/`shots`/`iso_size`），于是
`rust_lines_num`（机器可读的那个数字）被改坏时**门禁照样全绿** —— 页面渲染用的是
`rust_lines`，所以"页面 == data.json"的比对也发现不了。改为按 `measure()` 产出的
**全部键**逐个比对。

### G.2 第四遍：跑完整门禁（含 cargo test）暴露的问题

单独跑 `cargo test --workspace` 是绿的，但跑完整 `gates.py`（含 cargo 三件套）时
`perm::tests::audit_rotates_when_oversized` 变红。根因是**环境 + 测试设计**叠加：

* 宿主 **C 盘可用空间为 0**；
* 该测试用 `set_len(8MB + 1)` 造超限文件，而 NTFS 的 `set_len` 是**真分配**
  ⇒ `StorageFull`。

**修的问题**：轮转逻辑测试不该依赖宿主磁盘空间（也不该逼 CI 准备 8MB 连续空间）。
新增 `audit_limit()`：test 构建用 1KB，生产仍 8MB，并加断言钉住两者。
（附带一个 Rust 细节：`if cfg!(test) {A} else {B}` 不行 —— `cfg!` 是值层面的条件，
两个分支仍会被名字解析，测试专用常量在生产构建里不存在，报 E0425；必须用
`#[cfg]` 分开的语句块。）

**未擅自处理的环境问题**：C 盘 0 字节可用。占空间的是与本项目无关的数据
（`odis_download_dest` 4.1 GB、`codex-trace-*.json` 834 MB 等），已报告给用户，
**没有删除**。只清掉了本会话自己产生的 73 个临时日志（0.9 MB）。

### G.2b 一个"试了但回退了"的改动（留档，避免后人重犯）

针对"门禁会因磁盘空间间歇性失败"，还试过一个看似合理的修法：让 `gates.py` 把子进程的
`TEMP`/`TMP`/`TMPDIR` 指到 `<repo>/target/gates-tmp`（与构建产物同盘、空间充裕）。

**结果：破坏了测试语义，已回退。** 原因是 `aetherd` 的**写工具白名单只允许"用户数据区
（家目录、/tmp）"**，而多个测试用 `std::env::temp_dir()` 造路径 —— 临时目录一旦落到仓库里，
`delete_then_restore_*` 与 `rename_refuses_to_overwrite_existing` 会**正确地拒绝写入**而失败
（这不是 bug，是安全策略在正常工作）。

结论：**临时目录的位置是测试前提的一部分，不能为了"绕过磁盘满"而随手改**。
磁盘满按环境问题处理；测试自身的空间占用在测试里解决（轮转测试改 1KB 上限即属此类）。
这段理由已写进 `scripts/gates.py` 的注释里，防止后人"顺手优化"。

### G.3 一个正面结果：新门禁抓到了我自己

第三遍加的"测试分布表"门禁，在第四遍同步数字时**当场抓到我的不一致**
（改了 Windows 列却漏了属性列：合计 418 vs 求和 419）。门禁按设计生效 ——
这正是"把口径变成可执行断言"的价值。

### G.4 收尾状态

* `cargo build --all-targets --locked`：**0 警告**
* `cargo test --workspace --locked`：**400 项全绿**（Windows）
* 6 crate musl check：**0 警告**
* `gates.py`（完整 10 项）：**全过**；`gates.py --fast` 7 项全过
* 工作区干净，全部改动已提交（`f34fd1c` 为最后一次）

---

*本报告由代码审查 + 运行时实证生成。所有发现均给出 `文件:行号`；标注"未验证"的结论不得当作事实使用。*
