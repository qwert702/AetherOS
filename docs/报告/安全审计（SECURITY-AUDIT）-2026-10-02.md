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

**为什么这是新问题、而不是已知残余**：`docs/归档/代码审查总集（CODE-REVIEW）-2026-09-29.md` §3.2 第 5 条确实把"能读到 `ui.key` 的进程可注册为 UI"列为已知残余，但它给出的**风险界定是**"把攻击面从任何能连 7311 的进程收窄到**能读 0600 密钥的进程**"——这个界定的前提是"读 0600 root 文件需要特权"。而 `read_file` 工具恰恰以 root 身份、以免确认的 L0 等级、向任意未注册连接提供该文件，**前提被同一套系统的另一个功能推翻了**。已知残余因此升级为完整提权链。

**前置条件与影响**：
- 需要本机任意代码执行（任意 uid）。出厂镜像目前只有 root（空口令），所以"非特权→root"的增益暂时有限；但**权限模型本身已经失效**——L2/L3 的"用户确认"从未真正发生。一旦引入非 root 用户或应用沙箱（roadmap 方向），或 `AETHER_BIND=0.0.0.0` 被用于调试，它就是完整的本地/远程提权链。
- 可执行的操作包括：`install_disk`（整盘擦除）、`file_write` / `file_delete`（以 root 写删任意白名单内路径）、`app_install`、`reload_config`、剪贴板读写。

**修复建议（按性价比排序）**：
1. **把 `ui.key` 移出 AI 可读白名单**：改放 `/run/aether/ui.key`（`/run` 不在 `READ_ALLOWED_ROOTS` 内），或在 `READ_DENY_SUBPATHS` 加 `ui.key`。**一行改动即可切断该链**。
2. **给 `ToolCall` 加与 `Request` 同等的连接信任检查**：对 L1+ 工具（尤其 `clipboard_read` / `clipboard_write` / `desktop`）要求 `is_ui`，或按工具声明"需要可信通道"。
3. **令牌绑定确认方身份**：签发时记录连接身份（uid via `SO_PEERCRED`，或至少绑定连接实例），兑换时校验同一来源。
4. **aetherd 改 Unix socket 0600**（`docs/进程间通信协议（IPC-PROTOCOL）-2026-09-05.md` 本来就写的是 `/run/aetherd.sock`，实现却用了 TCP）——顺带解决"任何本机进程可达"。
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
- **I-7**：`docs/进程间通信协议（IPC-PROTOCOL）-2026-09-05.md` 称 aetherd 走 `/run/aetherd.sock`，实际是 `127.0.0.1:7311` TCP（**文档与实现不一致**）。
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
| 权限模型 L0–L3 分级、令牌一次性且绑参数、6 种裁决值 | ✅ 代码与 `docs/AI权限模型（AI-PERMISSIONS）-2026-09-05.md` 一致（**机制实现是真的**；问题在于可被绕过，见 H-1/H-2） |
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
| 6 | `docs/发布/发布清单（LAUNCH）-2026-09-29.md`=334、`发布文案（LAUNCH-COPY）-2026-10-01.md`=356、`更新日志/2026-10-01.md`=356、`2026-10-02.md`=368/370 | 370 才是当前真值 | ❌ 四处过时 |
| 7 | README 逐 crate：compositor **15,890** | 实测 **16,021** | ❌ 差 131；且 README 各 crate 之和 = 24,360 ≠ 它自己的表头 24,491 |
| 8 | INDEX 逐 crate：compositor **15,926** | 实测 16,021 | ❌ 差 95 ⇒ **同一 crate 三个数字**（15,890 / 15,926 / 16,021） |
| 9 | INDEX 逐文件表 42 条 | **11 条错** | ❌ 其中 **4 处 `src/main.rs` 全被写成 `3,857`**（aetherd 实际 636 / init 210 / ops 175）——一次全局替换的误伤 |
| 10 | INDEX L359-368「测试分布」表 | 填的是**行数/文件数**不是测试数 | ❌ 整表已损坏不可用 |
| 11 | INDEX 内部：compositor 逐文件之和 15,879 vs 小计 15,926；`wayland/` 2,194 vs 2,192 | 自相矛盾 | ❌ |
| 12 | 走查图张数：README **10 张** / INDEX **19 张**（同文件另处又写 10） | 实际 20 张文件 / 19 张在门禁清单 | ❌ 三处不一致 |
| 13 | INDEX L49 ISO"约 30MB" | 38.5 MB（同文件 L187 又写 41 MB） | ❌ |
| 14 | CHANGELOG"架构上系统内**不存放上游 API Key**" | `modelcfg.rs` 持久化 `api_key` 到 `/etc/aether/model.json`，且默认 `cloud_base` 就是 GLM 官方端点 | ⚠️ 架构意图与实际能力不符（未强制走用户网关） |
| 15 | README/CHANGELOG"可安装到虚拟磁盘后独立引导" | 工作区 ISO 实测非 isohybrid，安装器前提不成立 | ⚠️ 与 H-6 冲突（发布产物是否 isohybrid 未验证） |
| 16 | `docs/进程间通信协议（IPC-PROTOCOL）-2026-09-05.md` 称 aetherd 走 `/run/aetherd.sock` | 实际 `127.0.0.1:7311` TCP | ❌ |

### 3.3 根因：门禁给了**虚假保证**

`scripts/repo-stats.py --check` 实跑输出：

```
[ OK ] 代码索引（INDEX）-2026-09-11.md: 24,491 行 / 45 文件
[ OK ] README.md: 24,491 行 / 45 文件
[ OK ] docs/路线图与已知缺口（ROADMAP）-2026-09-05.md: 24,491 行 / 45 文件
exit=0
```

门禁 3/3 全绿，但**同一个 compositor 在 README 写 15,890、在 INDEX 写 15,926、实测 16,021**。原因：`DECL_INDEX` / `DECL_README` / `DECL_ROADMAP` 三条正则**只抓合计行**，逐 crate、逐文件的数字完全不受保护。这是一个**机制性缺陷**：门禁只能防住它恰好覆盖的那一个数字。

### 3.4 未跟踪文件：归档搬迁后的残留旧副本

`git status` 长期挂着 5 个 `??`（全在 `docs/` 下）。SHA256 比对显示：4 个与 `docs/归档/` 下同名文件**逐字节相同**，第 5 个 `docs/未实现项清单（UNIMPLEMENTED）-2026-09-27.md` 比 archive 版**少 16 行**——少的正是那段"⚠️ 本文已大量过时"的警示头。

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
| 9 | `docs/报告/生产化计划（PRODUCTION-PLAN）-2026-09-28.md` 声称的"六个硬门禁 6/6 达成" | 未逐条复核 |
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

---

## 附录 H：残余发现的处置（2026-10-03，批次 S-1~S-3）

> 本附录的规矩：**逐条对着代码核**，而不是对着附录 D.2 的结论抄。
> 核对中发现 D.2 有多条已过时 —— 记在下面，避免后来者重复劳动。

### H.1 核对结论：D.2 里已过时的三条（代码已修，报告未更新）

| D.2 编号 | 报告当时写的 | 代码实况（本次核对） |
|---|---|---|
| #14 Unix socket 失败静默降级 TCP | ❌ 未修 | **已修**：`aetherd/src/server.rs` 建 socket 失败即 `exit(EXIT_UNIX_SOCKET_UNAVAILABLE)`；只有显式 `AETHER_ALLOW_TCP_ONLY=1`（开发机调试）才降级，且日志明说"仅提供 TCP 通道" |
| #15 `audit_args` 字段白名单默认放行 | ⚠️ 部分修 | **已修**：`aetherd/src/tools.rs::audit_args` 注释明写"**默认脱敏**，而不是点名脱敏"，并有 `audit_args_redacts_unknown_string_fields_by_default` 用例 |
| #18 仓库无 CI | ❌ 未修 | **已建**：`.github/workflows/ci.yml`（ubuntu + windows 双平台跑 `gates.py --online`，Linux 任务额外跑 `cargo test`）；文件头如实写明"本工作流在本地无法验证" |

### H.2 本轮实际修复（三个提交）

| 批次 | 对应发现 | 提交 | 改了什么 |
|---|---|---|---|
| S-1 | #12 `fsck` fail-open | `230f01d` | 判定抽成纯函数 `fsck_verdict`；**fail-open 只剩"镜像确实没这工具"一种**；"工具在但执行失败/被信号杀死"改为 fail-closed；"跳过检查"措辞改为"**未检查**，非通过" |
| S-2 | M-17 残余（aetherd 侧无 `SO_PEERCRED`） | `d5ae35e` | 判据放进 `aether-ipc`（**在 musl 检查范围内**，于是 Linux 代码真的被编译验证）；aetherd 与 aether-init **统一到同一判据**（原先是 `uid == 0` vs `uid == 0 \|\| uid == getuid()` 两处写法） |
| S-3 | #17 H-6 校验**时机**只能靠人工审查 | `d1f97dd` |
| S-4 | **#13 H-3 身份判据可伪造**（高危） | `0ac2ab3` | 分区身份绑定到**所在磁盘 MBR 的随机签名**（装机时生成并写入偏移 0x1B8，同时记进安装标记）；启动时要求两者一致，**fail-closed**（任一缺失即拒绝）。攻击成本从"拷一个分区"提高到"伪造整盘" |
| S-5 | #17 H-4 `O_NOFOLLOW` 开关只能靠人工审查 | `3d91e3d` | 源码扫描式测试断言 `O_NOFOLLOW` / `symlink_metadata` 预检**实际还在**（这些是 Linux 专属代码，Windows 上行为测试等于没写） | 新增源码扫描式测试：断言三道门（`--yes` 确认 / isohybrid 前提 / 不得自读自写）在源码里出现在 `dd` **之前**；回滚顺序即变红 |

### H.3 S-2 的过程记录：一个只有 musl 检查能发现的坑

S-2 最初用标准库 `UnixStream::peer_cred()` 实现（我以为 1.70 起已稳定）。
Windows 编译不到这段 `cfg(unix)` 代码，**`cargo test` 全绿**；
是 **musl 静态检查**当场报出：

```
error[E0658]: use of unstable library feature `peer_credentials_unix_socket`
```

它在 rustc 1.98 上**仍未稳定**。若没有这条门禁，提交上去的就是一个
"Windows 测试全过、Linux 根本编译不过"的改动 —— 而这恰好就是 D.2 第 18 条
（无 CI ⇒ Linux 断言从不执行）所要防的事。**这是本轮最有价值的一条记录。**

### H.3b §3.2 文档一致性发现的逐条核对（批次 S-6）

D.2 之外，报告 §3.2 另有 16 条"声明与实际不符"。逐条**对着代码/现状**核对后的结论：

| §3.2 # | 报告当时写的 | 本次核对结论 |
|---|---|---|
| #1~#13 | 数字漂移/表损坏（README 与 INDEX 的测试数、逐 crate、逐文件、走查图张数、ISO 体积等） | **已随文档整理与本轮数字同步消除**；且门禁已从"只抓合计行"改为**逐 crate + 逐文件 + 测试分布表的集合相等断言**（附录 D.2 第 8 条），这类漂移现在会被拦下 |
| **#14** | CHANGELOG 称"系统内**不存放**上游 API Key"，而 `modelcfg.rs` 会持久化 | ❌ **报告属实，已修（S-6）**：改为如实陈述 —— 支持"自有网关"部署，也可直连上游；直连时 Key 持久化在 `/etc/aether/model.json`（**0600**，与 `ui.key` 同级），两种方式都**不经本项目中转** |
| **#15** | README/CHANGELOG 称"可安装到虚拟磁盘后独立引导"，与实测非 isohybrid 冲突 | **已修（早于本轮）**：README 现写明"**装机目前不可用**：需 ISO 经 isohybrid 处理 + 构建侧已补步骤自检，重新构建后可用" |
| **#16** | IPC-PROTOCOL 称 aetherd 走 `/run/aetherd.sock`（当时实际是 TCP） | **复核为"文档已对、代码也已补齐"**：`aether-ipc` 定义 `/run/aetherd.sock`，aetherd 建 0600 socket 且**优先**使用、TCP 仅作回退（附录 D.2 #14）。CHANGELOG 同段漏改的"NDJSON over TCP"已由 S-6 一并改为"双通道" |

> 方法论提示（与 H.1 同一条）：§3.2 与附录 C 一样是**当时的快照**。
> #15/#16 已被后续提交修掉，只有 #14 是真正遗留的 —— 逐条对着代码核，比照单全做省得多。

### H.4 仍然未处理（如实列出，不含糊）

| 编号 | 发现 | 为什么没做 |
|---|---|---|
| #13 | ~~H-3 身份判据可伪造~~ | **已由 S-4 处理**（跨介质绑定）。残余：能整盘伪造（含 MBR）的人仍可绕过 —— 需安全启动/TPM，属范围外 |
| #17 残余 | ~~`O_NOFOLLOW` 开关回滚不变红~~ | **已由 S-5 处理**。残余：源码扫描只能证明"代码还在"，不能证明"行为正确"（加错位置仍可能漏），需 Linux 侧运行 |
| #16 | M-13 括号粘贴在 guest 的实际行为 | **必须真机/虚拟机验证**（busybox ash 是否发 DECSET 2004），本机无法做 |
| L-11 | `scripts/` 目录 ACL 过宽 | **宿主环境问题**（Windows ACL），仓库里改不了，需在机器上 `icacls` 收紧 |

### H.5 未实机验证（与附录 B 一致，本轮新增）

* fsck 在真机上失败/工具缺失的实际行为（本机无 ext4 持久分区）
* 陌生 uid 连接被拒的实际行为（本机无 unix socket 对端可造）
* 真机装机流程（S-3 只加测试，不改行为）
* **S-4 的磁盘签名绑定**：真机上 `aether-init` 读磁盘 MBR 与挂载行为（本机无块设备）；
  旧装机缺少 id 会被**拒绝挂载**（这是有意的 fail-closed），需要重装或手工补 id
* S-4 的 `/dev/urandom` 回退路径（拿不到随机源时退回时间戳混合）未在真机触发过

---

## 附录 I：逐任务审计（2026-10-04，会话批次 V-1 ~ AA-1）

规程要求**每完成一个任务执行 1 遍代码审计**。本附录记录对**本会话新增代码**的审计，
重点放在"会写系统文件、会碰路径"的那几块（新写的代码最可能带着我自己的习惯性疏忽）。

### I.1 审计范围与选点理由

| 代码 | 为什么重点看它 |
|---|---|
| `aetherd/src/snapshots.rs`（新，§5.6） | **会写系统文件**（回滚 `/etc/localtime`、`resolv.conf`、策略文件），并且**按从磁盘读来的路径写** |
| `aether-compositor/src/spaces.rs`（增删，§5.4） | 下标运算 + `Vec::remove` —— 越界与"删错格子"最容易藏在这里 |
| `aether-ipc` 新请求 + `aetherd/src/server.rs` 新处理 | 新入口的鉴权（是否每一条都要求 UI 通道、是否都留审计） |
| `src/text.rs` 双表 / `draw.rs` 的 `ui` 缩放 | 索引取值（表长不一致 → 运行期 panic）与全局状态 |

### I.2 发现与修复（三条，均已修 + 加测试）

| # | 等级 | 位置 | 问题 | 修复 |
|---|---|---|---|---|
| I-1 | **中危** | `snapshots.rs::restore` | **回滚会放宽文件权限**：临时文件用默认权限建（umask 022 → 0644），`rename` 覆盖后，本来 0600 的 `policy.json` 变成**所有人可读** —— 回滚自己制造了一次权限降级 | 清单里记录快照那一刻的 `mode`；回滚按它设置；**未知时用 0600（宁紧勿松）** |
| I-2 | 低危 | `snapshots.rs::create` | **同一秒创建两次会静默覆盖**：id 只精确到秒，目录同名 | 目录已存在就加后缀（`-2`、`-3`…） |
| I-3 | 低危 | `snapshots.rs::restore` | 清单里的 `path` **没有限制在白名单内** —— "把从磁盘读来的路径直接当写入目标"是不该有的习惯 | 回滚前逐个校验是否在允许清单内，否则拒绝；且**鉴权检查放在最前面**（不先做校验和，避免"损坏"掩盖"不允许"） |

I-3 的修复里有一个必须说清的取舍：白名单做成了**参数**（`restore_allowed(root, id, allowed)`），
生产入口 `restore()` 传 `FILES`。为什么不让它写死在函数里 —— 那样测试就只能拿 `/etc/...`
当目标，**既不能在 CI 里跑，也就等于这条安全检查测不到**。现在生产与测试走同一条代码路径，
另有一条测试专门盯**生产入口**没有退化。

### I.3 复核方式（怎么找出来的）

* I-1：跟着"回滚时文件是怎么被写下去的"这条线走 —— `fs::write` → `rename`，
  中间**没有任何一步管权限**，而写文件本身是 umask 后的结果。这条不需要工具就能看出来，
  需要的是**逐行走一遍写入路径**；
* I-2：看 id 的生成方式（精确到秒）与目录的取法（`root.join(id)`）——
  两者一合就能推出"同一秒会撞"；
* I-3：问"这个值是从哪来的" —— `e.path` 来自**磁盘上的 JSON**，
  却被当成写入目标，属于典型的"不该信任的来源"；
* `spaces.rs` 的下标运算：逐条对照 `Vec::remove` 前后的 `current` 重编号，
  并用测试固定"删掉当前空间之前的那个，当前下标要跟着挪"。**未发现新问题**。

### I.4 仍然未处理（如实列出）

* `restore_allowed` 的白名单只做**字符串相等**比较，不做路径规范化 ——
  若将来白名单里出现相对路径或带 `..` 的项，等价性会被绕过。
  当前 `FILES` 全是写死的绝对路径，所以**不构成现实风险**，但这条依赖"白名单内容可信"，
  写在这里免得以后放宽它；
* 快照文件（`files/` 下的副本）在 `create` 时**没有 `sync_all`**：
  掉电可能留下零长度副本 —— 回滚时的校验和检查能发现（会拒绝而不是写坏内容），
  所以是可接受的取舍，未改；
* 两条 `#[cfg(unix)]` 权限端到端测试在本机**不编译也不运行**（aetherd 在这台机器上
  连 Linux 目标都编译不了 —— `ring` 需要 musl-gcc）。因此把"用哪个权限"的**决策**
  抽成纯函数 `restore_mode()` 并加了跨平台测试，端到端那两条留给 CI/真机。

---

## 附录 J：逐任务审计之二 —— 应用安装与链接（2026-10-04，批次 AC-1）

审的是 `aetherd/src/apps.rs`（1291 行）：**装应用 = 往系统里放一个可执行文件**，
属于本项目攻击面最大的一块。审计方式：沿着"外部给的字节 → 落到磁盘 → 被执行"这条路
逐段走。

### J.1 先记下**确认无问题**的几处（这些不是废话，是审计结论）

| 检查点 | 结论 |
|---|---|
| id 会不会被拼成逃逸路径 | **不会**。`valid_id` 要求字母数字开头、限长 64、字符集限定；`load_manifest` 每次读清单都**重新校验** |
| 手改 `/var/apps/<x>/app.json` 把 id 改成 `../../usr/bin/evil`，能否让 `link_all` 写到 bindir 外 | **不能**。`list()` / `find()` 都经 `load_manifest()`，非法 id 直接被拒 —— 这是这次审计最值得确认的一条 |
| 包内符号链接 | **拒绝**。`copy_tree` 遇到 symlink 直接失败（注释里写明理由：装完它就等于把逃逸面装进系统） |
| 包内 `entry` 逃逸 | **拒绝**。`resolve_entry` 挡绝对路径与 `..`，并要求是普通文件 |
| 半途失败 | **不留垃圾**。复制失败会 `remove_dir_all`，否则下次会撞"已经装过了" |
| 预检时机 | **在复制之前**。不会"先装进去再说跑不起来" |

### J.2 两条发现（均已修 + 加测试）

| # | 等级 | 位置 | 问题 | 修复 |
|---|---|---|---|---|
| J-1 | 低危 | `apps.rs::link_all` | 清理判据**只看"文件内容里含生成标记"**，而且用 `read_to_string` 读**任意**文件（大二进制会整个读进内存）。用户自己在 bindir 放的文件，只要内容含那串中文就会被删掉 | 判据收紧为三条同时成立：普通文件 + 首行 `#!/bin/sh` + **前 512 字节**内含标记 |
| J-2 | 低危 | `apps.rs::make_executable` | 把 `PermissionsExt::mode()` 的**完整 `st_mode`（含 `S_IFREG` 等类型位）**直接 `set_mode(mode \| 0o755)` —— 等于把类型位传给 `chmod`。Linux 内核恰好会掩掉，所以现在能用 | 抽出纯函数 `exec_mode()`：先 `& 0o7777` 再或 0755 |

J-2 与附录 I 的 `restore_mode()` 是**同一个毛病**：`st_mode` 与"权限位"不是一回事。
两处都改了 —— 这类错误不会报错，只会静静地依赖平台行为。

### J.3 仍未处理（如实列出）

* `wrapper_script` 用 `shell_quote` 逐个引起来（含 `'` 的转义），**本身是对的**；
  但固定参数与用户传入参数之间**没有 `--` 分隔** —— 若某个应用的固定参数以 `-` 开头，
  用户在命令行传的 `-x` 可能被它"吃掉"。当前包清单里的 args 来自我们自己的 mkapp 产物，
  不构成现实风险，写在这里免得以后放宽；
* `link_all` 往 `bindir` 写文件**没有做"目标是不是符号链接"的检查**：
  若 bindir 里已被放了一个同名 symlink（例如指向 `/etc/shadow`），
  `fs::write` 会**跟着链接写过去**。bindir 是系统目录、往里面放东西需要 root，
  所以不构成现实风险；但要修的话应当在写之前 `symlink_metadata` 检查一次。

---

## 附录 K：**高危 · 锁屏旁路**（输入守卫顺序错了，2026-10-04 批次 AD-1）

### K.1 问题

`aether-compositor` 的输入守卫顺序原本是：

```
终端(424)  →  锁屏(435)  →  首启向导(441)  →  命令面板(535)  →  DNS(552)
```

也就是说**终端守卫排在锁屏之前**。于是"锁着屏、但背后恰好有终端焦点"时：

* **按键先落进终端里的 shell** —— 在锁屏状态下**可以执行命令**（锁屏旁路）；
* 顺带把解锁也弄坏了：**解锁密码被终端吃掉**，用户按不出解锁（只有鼠标还能用）；
* 同一类问题还有三处：终端有焦点时，**首启向导、命令面板、DNS 输入框都拿不到键**
  （面板的搜索框打不进字、向导按键被 shell 吃掉）。

代码里那句注释写的是"放在 match 之前、且排在 DNS 守卫之前：**锁着的时候一切输入都归锁屏**"
—— 意图是对的，但**终端守卫在它上面**，注释与实现不一致（这正是"注释不能替代检查"的例子）。

### K.2 影响

* 等级：**高危**（锁屏是"人离开机器"时唯一的安全边界，而它没有拦住键盘输入）；
* 前提：锁屏时背后存在一个有焦点的终端窗口。这不是罕见状态 ——
  用户在终端里干活时锁屏就会命中；
* 不需要任何特殊权限，也**不需要 GUI**（纯按键即可）。

### K.3 修复

把"一个事件交给谁"抽成**纯函数** `key_route()`（`main.rs`），顺序写在一处：

```
Lock（锁屏） > Setup（向导） > Palette（面板） > Dns（编辑态） > Terminal > Global
```

循环里的每个守卫改成 `matches!(route, KeyRoute::X)`，终端块**下移**到这四者之后。

### K.4 验证

| 测试 | 盯住的性质 |
|---|---|
| `key_route_prefers_the_most_restrictive_subsystem` | 六个组合的优先级 |
| `lock_screen_beats_the_terminal` | **回归**：`(locked=true, terminal_focused=true)` 必须得到 `Lock` |
| `the_loop_checks_lock_before_terminal` | **源码探针**：循环里 `KeyRoute::Lock` 的守卫必须出现在 `KeyRoute::Terminal` 之前（函数没被改、但循环里守卫被调换位置的情况） |

第二条是把**出问题的那个状态组合**钉死；第三条补上了"函数对了但调用处顺序错了"这种漏网情况。

### K.5 同类问题的一次性清扫

* 全文件只有**一处**输入 drain（`main.rs:412`），所以修复覆盖所有真实输入路径；
* 鼠标事件在锁屏下同样被吞（`KeyRoute::Lock` 分支对非键盘事件直接 `continue`）；
* `--preview` 模式不构成边界（它是开发用途，能启动它的人已经能执行任意命令），
  但它走的是同一个循环，因此同样受这条顺序约束。

### K.6 仍未处理

* 这条修复**没有真机验证**：需要"锁屏 + 终端有焦点"这个状态组合，
  以及真实键盘输入（见附录 B 的实机清单）；
* `handle_lock_key` 自身的行为（退避、口令校验）不在本批范围内。

---

## 附录 L：**严重 · `passwd -l` 反而让锁屏按回车就进**（2026-10-04 批次 AE-1）

### L.1 问题

`/etc/shadow` 第 2 字段的三种取值对应**三种完全不同的语义**，而
`aether-compositor/src/auth.rs` 把其中两种合并了：

| 取值 | 真实含义 | 原实现 | 应当 |
|---|---|---|---|
| 空 | 未设口令 | `NoPassword` → **放行** | 放行并说明（保留） |
| `!` / `*` / `!$6$…` | **口令被禁用 / 账户被锁定**（`passwd -l` 的产物） | `NoPassword` → **放行** ✗ | **拒绝** |
| 文件读不到 / 无此账户 | 无法判断 | `Missing` → 放行并说明 | 放行并说明（保留） |

后果：管理员执行 `passwd -l user`（或往 shadow 里写 `*`）之后，
**锁屏按回车就能进入该账户** —— 一个**"增加限制"的动作反而取消了限制**。
这是方向性错误，属最严重的一类：安全机制在"被启用"时失效。

它甚至被**写进了测试**（`parse_shadow(SHADOW_LOCKED_HASH) == NoPassword`）。
这条值得单独记下来：**测试通过不等于行为正确 —— 测试会把 bug 固化成契约**。
所以本附录的修复同时改了三处：枚举、判定、以及那条固化了错误的断言。

### L.2 附带发现（中等 · 误拒）

同一函数里 `expired = d > 0`：第 8 字段是"自 1970-01-01 起的**过期日天数**"，
非空且为**未来**日期时账户**并未过期**。原实现把所有非零值都当"已过期" ——
例如 2030 年到期的账户现在就会被拒绝。方向是 fail-closed（不是漏洞），但属误拒，
而且会让"到期策略"这种正常运维手段变成"立刻锁死"。

### L.3 修复

1. 枚举拆开：`NoPassword`（只有**真的空**）/ **`Locked`**（`!`、`*` 前缀）/
   `Hash { hash, expires_day }` / `Missing`；
2. `Locked` → **一律 `Deny`**，且**不计入失败次数**（那不是用户输错，
   否则攻击者反复触发就会把机主自己罚到长时间等待）；
3. 过期日在 `decide()` 里与调用方传入的 `now` 比较：**未来才过期的不算过期**；
   过期同样不计入失败次数（与原来一致）。

### L.4 验证（`auth::` 11 项 + `lock::` 7 项全绿）

| 测试 | 盯住的性质 |
|---|---|
| `locked_password_is_denied_and_never_allowed` | **回归**：`!$6$…` 与 `*` 两种写法、空口令与任意口令，**全都必须拒绝** |
| `future_expiry_date_is_not_expired` | 未来过期日放行、已过期日拒绝（同一哈希两种过期日） |
| `expired_account_is_denied` | 用**正确口令**证明拒绝的原因是过期而非口令错 |
| `shadow_parsing_distinguishes_the_four_states` | 四种状态的解析（把原来那条错误断言改成 `Locked`） |

### L.5 仍未处理 / 未验证

* **未实机验证**：真机 `/etc/shadow`（`$6$`）上的完整解锁链路；本机只验证了纯逻辑；
* `Missing`（读不到 `/etc/shadow`）时**放行并说明**这一设计**保留**：
  能改 `/etc/shadow` 的人已经是 root，而"读不到就把机主永久关在门外"是另一种伤害。
  但这条**依赖一个前提**：放行时界面上必须**明确告诉用户"本次未校验口令"**
  （已有 `AllowWithReason` 的理由文案，且带理由时不会走"校验通过"的路径）——
  写在附录 B 的实机清单里，需要真机确认那句话真的显示出来了；
* 口令比对本身（`sha512crypt::verify_password`）的常数时间性未审计，
  属独立批次（本地锁屏的时序侧信道优先级低于网络认证）。

---

## 附录 M：**中危 · 一次伪造的 SNTP 回复就能把系统时钟跳到任意年代**（2026-10-04 批次 AF-1）

### M.1 问题

`aetherd` 以 **root** 运行，对时走 **UDP**（可伪造、可中间人）。
`parse_response` 把**报文结构**校验得很细（长度不足 48 字节直接拒、LI=3 拒、
mode/version 校验、stratum 0 与 16..255 拒），但**偏移量本身没有合理性检查**：

```rust
// 修前：只看"怎么调"，不看"该不该调"
if offset_ms.abs() < 100 { None } else if … < 1000 { Slew } else { Step }
```

于是无论服务器报的是 3 秒还是 50 年，`Step` 都会照做 ——
**一次伪造回复即可让系统时钟跳到任意年代**。后果不是"时间不准"这么轻：

* TLS 证书有效期判断（`notBefore`/`notAfter`）被打乱；
* 日志与审计的时间线失去顺序，事后无法还原"哪件事先发生"；
* 会话/令牌超时、缓存过期、构建系统的时间戳比较全部失真；
* 反向（把时钟拨回过去）还能让已过期的证书/令牌"重新有效"。

平滑路径同样有问题：给 `adjtime` 一个几天的增量，等于让内核花很久去追，
期间所有时间读数都偏。

### M.2 修复

1. 新增 `MAX_OFFSET_MS = 24 小时` 与纯函数 `offset_is_sane()`；
2. `apply_offset()` **先判合理性**，不合理直接返回新的 `Adjustment::Refused`，
   **一个字节都不碰时钟**（拒绝发生在调用平台接口之前）；
3. `Refused` 是一个**独立结论**，不是"失败"：界面要说"已拒绝调整"，而不是"出错"；
4. 顺带三处一致性修正：
   * 被拒绝时**不更新"上次同步时间"** —— 否则界面会说"刚刚同步过"，而钟其实没动；
   * `ok` 随拒绝为 `false`（界面按"没成功"呈现，理由在 `message` 里）；
   * `adjustment_for` 改用 `unsigned_abs()`：`i64::MIN.abs()` 在 debug 下 panic、
     release 下回绕 —— 一个来自网络的值不该有"看构建类型"的行为。

**为什么是 24 小时**：真正差出一天以上的情况（新装机、主板电池没电、虚拟机挂起很久）
应当由**用户显式**处理 —— 用户能在设置页看到"服务器报了多少偏移"，
那时手动设时间才有依据。"让一次网络回复替用户决定跳到 2030 年"不是合理的默认。

### M.3 验证（`timesync::` 13 项全绿）

| 测试 | 盯住的性质 |
|---|---|
| `offset_sanity_boundary` | ±24h 允许、±(24h+1) 拒绝、`i64::MIN/MAX` 拒绝且**不 panic** |
| `absurd_offsets_are_refused_before_any_clock_change` | 48 小时与 50 年都返回 `Refused`；**跨平台可跑**（拒绝在平台调用之前） |
| `normal_offsets_still_classify_as_before` | 加上限之后平滑/跳变的判断没被改坏 |

### M.4 仍未处理 / 未验证

* **未实机验证**：真机上"服务器报 50 年 → 界面显示拒绝 → `date` 未变"的链路；
* SNTP 回复的**来源校验**没做（不检查源地址/端口，也没有 NTS 之类的认证）——
  本批只挡住"影响范围"，没解决"谁都能回"这个问题。在局域网/虚拟机场景下这是常见取舍，
  但**写下来**：真正需要抗伪造时要上认证（NTS）或至少绑定源地址；
* 反向保护（禁止把时钟拨到过去）只由这条上限覆盖，没有单独的单调性检查 ——
  在 24 小时内仍然可以把时钟往回拨。

---

## 附录 N：逐任务审计之三 —— PID 1（`aether-init`）与回收站（2026-10-04 批次 AG-1）

`aether-init` 是**唯一还没被本会话审计过的特权组件**（它跑在 PID 1，降权、挂载、
关机都由它负责）。本附录先记**正面结论**，再记本轮发现。

### N.1 正面结论：这些加固是**真的在**（逐项确认，不是照抄注释）

| 检查点 | 结论 |
|---|---|
| 降权（`pre_exec` + `setgid`/`setuid`） | **失败会中止 exec** ✓ —— `pre_exec` 返错 → `spawn()` 失败 → 不会以 root 跑起来。这是降权代码最容易错的地方（静默忽略错误 = 权限没降） |
| 附加组 | **先 `setgroups` 清空** ✓ —— 否则从 root 降权后仍是组 0 的成员，"降权"只挡住属主权限。注释写明这是自查发现的 |
| 主组解析 | 先看 `/etc/passwd` 第 4 字段，**不用 uid 兜底** ✓（注释写明"uid 恰好等于某组 gid 会静默成功"） |
| 挂载选项 | `nodev,nosuid,errors=remount-ro` ✓，并**写明为什么不能加 `noexec`**（`dlopen` 的 PROT_EXEC 映射会被一起挡掉 —— 这个理由是对的，很多系统在这里加错） |
| 关机顺序 | 停服务 → **`sync()`** → `reboot(2)` ✓，且 `sync` 步骤出现在可审计的事件流里 |
| 日志目录 | `LOG_DIR` 0700 + **no-follow 改权限** ✓（避免 chmod 跟随符号链接） |

也就是说：**这个组件在之前几轮已经被认真加固过**，本轮没有再找到高危问题。
审计的正面结论同样要写下来 —— 否则下一个人还得重新走一遍。

### N.2 发现（低-中危）：回收站是默认权限 —— **被删文件对所有用户可读**

`aetherd/src/trash.rs` 的 `trash()` 用 `create_dir_all` 建回收站根与每个条目目录，
**不设权限** → umask 022 下是 **0755**。于是任何本机用户都能读回收站里**已经被删掉**的文件。

"删除"给人的预期是"别人看不到了"，而回收站里常常有配置、密钥、日志、聊天记录。
这与本会话另外两处"默认权限"问题属同一类（附录 I-1 的 0600→0644、附录 J-2 的 `st_mode`）。

修法：回收站根 + 每个条目目录都设 **0700**；模式抽成纯函数 `trash_dir_mode()`
（`#[cfg(unix)]` 的真机权限断言在 Windows 上不跑，而"用什么权限"这个**决策**必须到处可测）。

### N.3 顺带写明一个"决定而不是疏忽"

`aether-init` 的 `VAR_DIRS` 用默认权限创建 `/var/log/aether`、`/var/diag`、`/var/tmp`。
其中 **`/var/tmp` 是 0755，而 FHS 惯例是 1777**（sticky + 人人可写）——
也就是**非 root 应用写不进 `/var/tmp`**。

本轮**没有改它**，而是在代码里写明这是本系统的取向（单用户设备上"人人可写"没有收益、
却多一块可写攻击面），并要求"哪天应用需要写 `/var/tmp` 时，连同这条取向一起重新评估，
而不是顺手改成 1777"。审计的价值不只是修 bug，也包括**把隐含选择变成显式决定**。

### N.4 仍未处理 / 未验证

* **未实机验证**：回收站 0700 在真机上对非 root 用户的实际效果（需要第二个账户）；
* `trash()` 设权限用的是 `set_permissions`（**会跟随符号链接**）：若 `/var` 下的回收站
  路径被替换成指向别处的符号链接，chmod 会作用到目标上。往 `/var` 里放符号链接需要 root，
  不构成现实风险；真要收紧应当照 `logtee.rs` 的 no-follow 做法（那里已经这么做了）；
* `/var/tmp` 的取向（N.3）**保留**，但已列入"应用需要时可写"的重新评估清单；
* `aether-init` 的 `unit.rs`（单元文件解析）与 `manager.rs` 的**重启退避**策略未逐行审计，
  属后续批次。

---

## 附录 O：**高影响 · 任何 Super+键都会锁屏**（2026-10-04 批次 AN-1）

### O.1 问题

`aether-compositor/src/main.rs` 的 `UiEvent::Super(c, shift)` 分支**第一句**是
**无条件**的锁屏调用（注释写的是「Super+L 锁屏」，但代码在 `if c == 'l'` 判断之前）：

```rust
input::UiEvent::Super(c, shift) => {
    // §2.3：Super+L 锁屏……
    renderer.lock.lock();            // ← 无条件！
    tty_log("锁屏（Super+L）");

    if c == 'l' && !shift {          // ← 本意只在这里锁
        renderer.lock.lock();
        …
```

于是**每一个 Super 组合键都会锁屏**：

* `Super+1..4`（§5.4 切虚拟桌面）：切过去的同时被锁在外面；
* `Super+Shift+1..4`（把窗口搬到别的桌面）：同样；
* 也就是说这两个快捷键**实际不可用**，而它们在测试与文档里都是"已完成"。

`c == 'l'` 分支里那份**重复调用**是这段代码的指纹 —— 本意的那份在下面，
上面那份是被遗留下来的。

### O.2 为什么会漏掉

事件循环的分支逻辑**没有测试**（只有纯模块有）。这类"无条件调用放在过宽的条件上"
的错误，既不会被类型系统拦住，也不会被纯模块的单测覆盖。
它需要**人工审阅事件循环**，或者用**源码探针**把它钉住。

（同一类错误在本会话里出现过两次：附录 K 的"终端守卫排在锁屏之前"——
都是"判断条件的位置放错了"。）

### O.3 修复

删掉无条件的那份，锁屏只留在 `if c == 'l' && !shift` 分支里；
并在原处留一段注释说明**这里曾经有过什么错**（而不是把痕迹抹掉）。

### O.4 验证：源码探针

```rust
let needle = concat!("renderer.lock.", "lock();");   // 用 concat! 拼，避免匹配到测试自身
assert_eq!(src.matches(needle).count(), 1);
assert!(src.find(needle) > src.find("if c == 'l' && !shift {"));
```

两条断言分别守住：**只有一处锁屏调用**、**它在 Super+L 分支之后**。
多出来一份 `lock()` 就说明又有人在更宽的条件下调用了锁屏 —— 那正是这次 bug 的形态。

（写这条探针时踩了一个坑：第一版把 needle 写成字面量，于是它**匹配到了自己与注释**，
数出来 5 处。用 `concat!` 拼接后才是"只数代码"。）

### O.5 仍未处理

* 事件循环的**其它**无条件调用没有系统性检查 —— 本附录只修了这一处。
  更彻底的做法是把事件分派从循环里抽出来（§4.1 还没做的那一半），
  那样这类错误就能被单元测试覆盖；
* 这条修复**未实机验证**：需要真机上按 `Super+1..4` 确认不再锁屏。

---

## 附录 P：**界面承诺了一个不工作的快捷键**（2026-10-04 批次 AO-1）

### P.1 问题

§5.4 的虚拟桌面在批次 AA-1 之后是**动态**的（默认 4 个、上限 8 个，
`Spaces::count()` 是唯一真相），`switch` / `move_active` 也都改用 `self.count()` 了。
但**事件循环里的键位判断**没有跟着改：

```rust
if d >= 1 && (d as usize) <= spaces::Spaces::<draw::Win>::COUNT {   // COUNT = 默认值 4
```

于是：

* **`Super+5..8` 静默无效** —— 落到「其余 Super 组合键：明确忽略」那一支，
  按下去什么都不发生（既不报错也不提示）；
* 而 AA-1 里加桌面的提示恰好写着「已新增桌面（共 5 个）· **Super+5 可切过去**」
  —— **界面承诺了一个不工作的快捷键**；
* 快捷键总览页也还写着 `Super+1…4` / `Super+Shift+1…4`。

### P.2 这一类错误的名字

**「数据模型改成动态了，判断/文案还写死。」**
它不是逻辑写错，而是**某个数字没有跟着事实走** —— 与附录 K（守卫顺序）、
附录 O（无条件调用）同属一族：**能跑、能编译、测试全绿，但行为与承诺不符**。

### P.3 修复

1. 键位判断改用 `spaces.count()`；
2. 快捷键总览两条改为 `Super+1…8` / `Super+Shift+1…8`，并在描述里写明
   「默认 4 个、可增删到 8」；
3. **源码探针**：`main.rs` 里不许再出现那个默认常量的完整路径
   （该用 `spaces.count()`），并断言切换判断确实基于 `spaces.count()`。

写探针时又踩了同一个坑：第一版把它要查的字面量写进了**探针自己的文档注释**，
于是探针**匹配到自己**、数出 1 处 ✗（AN-1 是匹配到测试正文，这次是注释）。
**教训：探针里出现的任何字面量都要么用 `concat!` 拼、要么不在注释里复述。**

### P.4 仍未处理

* 同一族的检查只覆盖了"桌面数"这一处：**其它"模型是动态的、某处还写死"的地方
  没有系统性排查**（例如上限 `MAX` 与界面文案是否总是一致）；
* **未实机验证**：真机上按 `Super+5..8` 切到第 5…8 个桌面。

---

## 附录 Q：逐任务审计之四 —— 输入分支的其余七个（2026-10-04 批次 AP-1）

**先说结论：这一轮没有找到活跃 bug。** 这条同样要写下来 ——
否则读者会以为"每轮审计都有收获"，而审计的真实产出包括"这一片是干净的"。

### Q.1 审了什么

`aether-compositor/src/main.rs` 输入循环里**尚未审过**的分支：

| 分支 | 结论 |
|---|---|
| `Char` | Ctrl+Space（NUL）切输入法、确认弹窗独占键盘、向导打开时字符不进指令条、IME 拼字选候选 —— 条件链正确 |
| `Backspace` | 确认弹窗优先、IME 拼字优先于删正文 —— 正确 |
| `Escape` | 先关控制中心、再取消 IME、再拒确认（**并回传撤销令牌 + 落审计**，不只是改本地状态）—— 正确 |
| `Ctrl` | `Ctrl+Z`（撤销栈不在 `apply_ctrl` 签名里，所以单独处理）与 `Ctrl+K`（命令面板）都 `continue`，不会继续走 `apply_ctrl`；关窗时清拖拽状态 —— 正确 |
| `Nav` | 交给 `dispatch_nav` 的三级分流 —— 正确 |
| `Enter` | 确认（L3 校验回显）→ 安装向导 → AI 指令条，顺序正确 |
| 每帧段 | 通知开关同步、性能 HUD（读不到就写「不可用」）、空闲息屏 —— 正确 |

前几轮抓到的三类问题（守卫顺序、无条件调用、写死常量）**在这一片里都没有出现**。

### Q.2 只找到两处**脆弱点**（低，均已修）

| # | 问题 | 修法 |
|---|---|---|
| Q-1 | `layout::Layout::ALL[(n - 1).clamp(0, 3)]` —— 那个 `3` 是 `ALL.len() - 1` 的**手抄副本**。当前 `ALL` 恰好 4 个、解码器只产生 1..=4，所以**不触发**；但这是附录 P 那一类的形态 | 抽成纯函数 `layout::for_key(n)`，用 `ALL.len()` 同源 |
| Q-2 | `n - 1` 依赖「解码器保证 n>=1」这个**跨文件不变量**；`UiEvent::LayoutKey(usize)` 是公开变体，新产生者传 0 会在 debug 下**整数下溢 panic** | `saturating_sub(1)`（代价接近零） |

修法顺带带来一个实际收益：**「按键 → 布局」这条映射第一次有了单元测试**
（0 不 panic、1..=4 与 `ALL` 一一对应、越界落到最后一个）。

### Q.3 仍未处理

* `LayoutKey` 只覆盖 Alt+1..4（`ALL` 有几个布局，键位就到几个）——
  这条**耦合**现在由 `for_key` 与测试钉住，但"加第 5 个布局时要不要加 Alt+5"
  仍是需要人决定的事，代码里没有自动化；
* 输入层的解码（`input.rs`）本身没有再逐行审（本附录只审了循环侧）。

---

## 附录 R：把「判断」从「执行」里抽出来（2026-10-04 批次 AQ-1）

### R.1 起因：四轮审计撞在同一层

附录 K / O / P / Q 四轮审计**全部**落在同一个地方 —— `main.rs` 的事件循环，
而且抓到的三个活跃 bug **全在 `Super(c, shift)` 这一个分支里**：

| 附录 | 形态 |
|---|---|
| O | 分支开头**无条件**调用锁屏 → 任何 `Super+键` 都锁屏 |
| P | 键位判断写死默认常量 → `Super+5..8` 静默无效 |
| K | 守卫顺序放错（这条在更上一层的路由里，已由 `KeyRoute` 解决） |

它们的共同点是：**判断写在执行里**。执行代码里可以塞进任何东西 ——
一句无条件调用、一个写死的数字、一个放错的条件 —— 而那一层**没有测试**。

### R.2 做法：`Super` 分支的意图抽成纯函数

```rust
pub enum SuperIntent { Lock, SwitchSpace(usize), MoveToSpace(usize), Ignore }
pub fn super_intent(c: char, shift: bool, space_count: usize) -> SuperIntent
```

循环里只剩 `match input::super_intent(c, shift, spaces.count()) { ... }`。
三件事同时被解决：

1. **桌面数变成参数** —— 附录 P 那类"写死常量"无从下手（没有常量可写）；
2. **执行侧没有条件可塞** —— 附录 O 那类"无条件调用"在结构上不可能出现；
3. **优先级有测试** —— `Super+L` 必须排在数字判断之前，且 `Super+Shift+L` 不锁屏。

新增两条单元测试，把三个历史 bug 的**状态组合**直接钉住：
`Super+其他键 != Lock`、`Super+5` 在 4 个桌面时无效而在 5 个桌面时必须可用、
`Super+L` 恒为 `Lock`。

### R.3 顺带修正的探针

AN-1 的源码探针原本锚在 `if c == 'l' && !shift {` 上 —— 那段文本在重构后**不再存在**。
探针改成锚在执行分支 `input::SuperIntent::Lock =>`。
**这是探针的正常维护**：它守的性质（"只有一处会锁屏"）没变，锚点跟着结构走。

### R.4 仍未处理

* 这只是 §4.1 的**第一个切片**：`Char` / `Ctrl` / `Nav` / `Enter` 等分支
  仍然是"判断写在执行里"（本轮**没有**动它们 —— 那里要碰窗口、IME、终端，
  风险高得多）；
* 事件循环里剩余的源码探针（锁屏只一处、桌面数用动态值、守卫顺序）
  **仍然是探针而不是测试** —— 它们能防复发，但不会发现新问题。
  要让这一层真正可测，得继续按这个方法往外抽。

---

## 附录 S：同一事实写了两遍 —— `Ctrl` 键的分类（2026-10-04 批次 AR-1）

### S.1 问题（低，但属会咬人的那一类）

事件循环里有一句：

```rust
let closed = !shift && c == 'w';        // ← 手抄的「哪个键会关窗」
…
if let Some(msg) = apply_ctrl(&mut desktop, c, shift) {
    if closed { drag = None; snap_zone = layout::Snap::None; }
```

而「哪个键会关窗」的**真正定义**在 `apply_ctrl` 里（当时只有 `('w', false)`）。
两处**现在**恰好一致，但这是**同一事实写了两遍**：谁哪天在 `apply_ctrl` 里再加一个
关窗键（例如 `Ctrl+Q`），循环里的 `closed` 不会跟着变 →
**窗口数组变了而拖拽状态还指着旧下标**，也就是批次 P2-3 那次越界的成因。

这与附录 Q 的 Q-1（手抄的 `clamp(0, 3)`）是同一类：
**手抄的事实副本**。区别是 Q-1 抄的是常量，这次抄的是**规则**。

### S.2 修法

抽出纯函数 `ctrl_intent(c, shift) -> CtrlIntent`：

* `apply_ctrl` **按它匹配** → 「哪个键做什么」只剩一处定义；
* 事件循环用**同一个函数**算 `closed`（`matches!(intent, CtrlIntent::CloseWindow)`）。

再加一条**源码探针**：`apply_ctrl` 里 `close_window(` 只允许出现**一次**，
且必须在 `CtrlIntent::CloseWindow` 分支里 —— 防止有人绕过分类直接在别处关窗。

### S.3 验证

* `ctrl_intent_classifies_the_documented_keys`：`Ctrl+Shift+C/V`、`Ctrl+W`、
  `Ctrl+Shift+W`（**不关窗**，这是最容易写错的组合）、其余 `Unhandled`；
* `only_one_place_in_apply_ctrl_closes_a_window`（源码探针，见上）；
* **快捷键总览的探针又逮住一次**：`Ctrl+W` 那条原本锚在
  `let closed = !shift && c == 'w';` 上，重构后立即变红 →
  锚点改为 `ctrl_intent` 里的 `('w', false) => CtrlIntent::CloseWindow,`。
  这是本项目探针机制第四、五次发挥作用的记录（附录 R 里有前两次）。

### S.4 仍未处理

* `Char` / `Nav` / `Enter` 三个分支仍是「判断写在执行里」（本轮没动，
  那里要碰 IME、终端与 AI 词条，风险更高）；
* 「手抄的事实副本」这一族**没有系统性排查**：目前是"碰上一处、修一处、加一条测试"。

---

## 附录 T：**整体审计第 1 遍**（2026-10-04 · 全库系统审计）

> 规程要求"所有任务完成后整体审计 3 遍"。本附录是**第 1 遍**，
> 侧重**访问控制、注入、敏感信息、不安全默认**四条线。
> 第 2、3 遍将分别侧重"数据流/边界条件"与"复现式验证"。

### T.1 覆盖面与方法（先说方法，再说结论）

| 线 | 方法 |
|---|---|
| 命令注入 | 全库 grep `Command::new` / `.arg("-c")` / `sh -c` / `bash -c`，逐个人工判读 |
| 访问控制 | **脚本解析** `aetherd/src/server.rs` 的 26 个 `Request` 分支，逐个核对"是否有 UI 通道门 + 是否落审计" |
| 不安全默认 | grep TLS 校验放宽、`0o777`/`0777`、硬编码凭据字面量 |
| 敏感信息 | grep 凭据字面量；逐个核对审计日志 / socket / 快照目录 / 模型配置的落地权限 |
| 生产代码卫生 | 脚本按 `#[cfg(test)]` 切分，统计**生产代码**里的 `println!`/`print!` 与 `unwrap()` |

**方法本身的局限（必须写出来）**：

* `#[cfg(test)]` 切分是**近似**（本项目测试都在文件末尾，故可用；但若某文件在中间出现该属性会误判）；
* 依赖库 CVE 无法核查（本环境 `--offline`，跑不了 `cargo audit`）—— **标注为未验证**；
* 没有做运行时/模糊测试；
* 人工判读的部分（`Command::new` 那些）依赖审阅者的经验，不是机械可证的。

### T.2 结论：四线全部干净（附证据）

**① 命令注入：无。** 全库 35 处 `Command::new`，**没有一处**字符串拼接命令、
**没有一处** `sh -c` / `bash -c` / `.arg("-c")`；全部是硬编码可执行文件 + `arg()` 传参
（`/sbin/mkfs.ext4`、`/bin/mount`、`/bin/dd`、`amixer`、`aplay`、`uname`、`df`、`ps` 等）。

两处需要说明（**判为不是问题**，但记录在案）：

* `aether-init/src/main.rs:224` 的 `/bin/sh` —— 是**救援 shell**：PID 1 在启动失败时
  开在 `/dev/console`。无参数、无 `-c`、只有**本地控制台**可达；
* `aether-init/src/unit.rs:102` 的 `cmd /C exit 0` —— 在 `#[cfg(not(target_os = "linux"))]` 里，
  是**开发机兜底**，目标平台永不执行。

**② 访问控制：26/26 处理器全部合规。** 脚本核对结果：**每个"会改系统状态"的
`Request` 分支都有 UI 通道门、且都落了审计**（`audit_call` 或 `audit_or_warn`），
零缺口。只读入口（`Ping`/`SysInfo`）无门，符合设计。

扫描器最初报了两个"缺口"，**都是它自己的模式太窄**，人工核对后确认没有问题：

* `SystemPower`：该分支是**故意拒绝**客户端直发电源请求的
  （`audit_or_warn(... REJECTED_NO_UI)` + 403）—— 它是安全控制，不是缺口；
* `ToolCall`：用的是 `audit_or_warn`，且**逐工具的审计在 `tools::execute(gate, …)` 内部**
  （`Gate::audit_call`）—— 审计更深一层，扫描没看见。

**③ 不安全默认：无。** 无 TLS 校验放宽（无 `danger_accept_invalid_certs` /
`accept_invalid_hostnames` / `verify(false)`）；无 `0777` 权限
（命中的 `0o777` 全是**掩码** `& 0o777`，以及一条"坏权限必须被拒"的测试）。

**④ 敏感信息：落地权限都很紧。**

| 对象 | 权限 | 出处 |
|---|---|---|
| 审计日志 | **0600**（且**只在创建时**设 —— 避免引入"root 可改任意文件权限"的原语） | `perm.rs:304-313` |
| IPC unix socket | **0600** | `server.rs:106` |
| UI 密钥文件 | **0600**，路径在 `/run`（ramfs）→ **每次开机重新生成，不跨重启残留** | `server.rs:50/271` |
| 快照根目录与条目 | **0700** | `snapshots.rs:184-185` |
| 模型配置（可能含 API key） | **0600** | `modelcfg.rs:88` |

硬编码凭据：无真实密钥。命中的 `"sk-test"` / `"test"` / `"ollama"` 分别是测试夹具与
**本地 Ollama 的占位 key**（本地端点不校验该字段）；`unit.rs:296` 的 `passwd` 是
测试夹具文本。**均无真实秘密。**

**⑤ 生产代码卫生：守护进程里 0 处 `println!`、0 处 `unwrap()`。**

| crate | println/print | unwrap |
|---|---|---|
| `aetherd` | **0** | **0** |
| `aether-ipc` | **0** | **0** |
| `aether-init` / `aether-ops` / `aether-install` | 15 / 13 / 10 | 0 |
| `aether-compositor` | 12 | 10 |
| `aether-shell` | 1 | 0 |

守护进程与协议 crate 是**零**（这两个最怕 panic 与脏输出）✓。
CLI 类 crate 的 `println!` **就是它的用户界面**（不是调试残留）。
`aether-compositor` 的 12 处有明确理由（见 T.3）。

### T.3 本遍唯一的发现（低危）：AI 会话内容**未脱敏**地进控制台日志

`aether-compositor/src/main.rs` 里有两处把 **AI 请求的参数**与**回复原文**打进标准输出：

```
main.rs:2465  println!("aether-compositor: 收到 Action {name} {arguments}");
main.rs:2488  println!("aether-compositor: AI 回复: {preview}");
```

* **风险**：`arguments` 可能含用户让它处理的敏感内容（口令、身份证号、银行卡号等），
  而**审计日志那条路是脱敏的**（参数在 `Gate` 内部统一处理）——
  **两条日志路的脱敏策略不一致**：结构化审计脱敏、控制台原文。
* **等级：低**。理由：控制台是本地设备（串口/本机终端），且同样的内容就在用户屏幕上；
  非远程可达。
* **是否有意为**：**是**。旁边注释写着"每一步都打串口/控制台日志
  （QEMU TCG 下截图窗口太窄，日志是可靠诊断通路）" —— 这是**刻意保留的诊断能力**。
* **处置**：本遍**没有改动**它（改掉会削弱真机排障能力，而收益有限）。
  记在这里，供第 2/3 遍复核是否需要"脱敏后仍可诊断"的方案
  （例如只打参数**个数与长度**、或只打工具名）。
  这一条也是"**框架的哪个部分没被同一条策略覆盖**"的典型例子。

### T.4 本遍未覆盖（留给第 2、3 遍）

* **依赖库 CVE**：本环境 `--offline`，跑不了 `cargo audit` —— 未验证；
* **运行时/模糊测试**：未做；
* **数据流与边界条件**：整数溢出、超长输入、并发/竞态（`aetherd` 的共享状态）、
  IPC 报文长度上限 —— 未做；
* **复现式验证**：第 3 遍计划对前两遍的每条结论做**可复现的取证**
  （脚本 + 原始输出一并留档），而不是停留在"我读过代码"。

### T.5 与逐任务审计的关系

附录 K / O / P / Q / R / S 是**逐任务审计**（每完成一个任务 1 遍，规程第一节要求），
它们抓到的都是**行为与承诺不符**类的问题（守卫顺序、无条件调用、写死常量、手抄副本）。
本附录是**整体审计**（规程第九节要求），关注的是安全基线四线 —— 两类审计的
"视力"不同：逐任务审计擅长看**这一处改动**，整体审计擅长看**整个面上的同类漏洞**。

### T.6 附：性能门禁连续飘红的**复现式取证**（这才是"证据"而不是"推断"）

背景：批次 AS-1 / AT-1 与本次整体审计期间，性能门禁**连续三轮**报红
（P99 41~45 ms，阈值 33 ms）。我的初步判断是"宿主负载，不是回归" ——
但那是**推断**（依据是"`--bench` 不读输入、只测 CPU 侧绘制"这一条构造性理由，
加上平均帧从基线 12 ms 涨到 20 ms 这一条现象）。

按本次审计对"复现式验证"的要求，做了**同条件 A/B 对照**（同一台机器、同一门禁脚本、
背靠背执行，中途不改任何代码）：

| 被测版本 | 平均帧 | P99 | 判定 |
|---|---|---|---|
| `3360f2e`（**改动之前**：AS-1/AT-1 尚未提交） | 15.65 / 15.91 / 17.51 ms | **28.21 ms** | 通过（占预算 85%） |
| `main`（**改动之后**，紧接着测） | 16.07 / 14.30 / 16.47 ms | **25.17 ms** | **通过（占预算 76%）** |

**结论：红是宿主负载，不是回归 —— 且改动之后的数字比改动之前更好。**
证据强度：两次测量**背靠背、同条件、仅版本不同**；
若把改动误当回归，这次对照会直接推翻它（它没有）。

同时保留了另外三条旁证：**平均帧在三轮红里从 24.05 → 19.86 → 16.78 ms 单调回落**
（基线 12 ms）—— "整机在变慢"的形态，而不是"某段代码变慢"的形态。

**留给后续的纪律**：门禁飘红时，先做这种**版本对照**（成本约两次构建），
再决定是改代码还是记环境 —— 不要停在"我觉得是环境"。

---

## 附录 U：**整体审计第 2 遍** —— 数据流与边界条件（2026-10-04）

> 第 1 遍（附录 T）审的是四类安全基线；这一遍按计划审**边界与数据流**：
> 超长输入、整数溢出、路径遍历的边角、并发/竞态。

### U.1 结论：**没有发现新问题**（四项检查全部干净）

**① 超长输入：读取层有硬上限。** `aetherd/src/server.rs:301` 有
`read_line_capped(reader, max)`：用 `take(max).read_line(...)`，
且**当读满上限却仍没遇到换行时直接报错**（不是继续读、也不是静默截断）。
所以"客户端发一条永不换行的超长报文撑爆内存"这条 DoS 不成立。
各功能另有更紧的上限：剪贴板 `MAX_BYTES = 64 KiB`（超限**整段拒绝**、不截断）、
应用预检 `MAX_PREFLIGHT_BYTES = 4 MiB`、依赖文件数 `MAX_DEP_FILES = 64`、
DNS 服务器 `MAX_DNS = 3`。

**② 整数溢出：换算用饱和运算。** `removable::sectors_to_bytes` 用
`saturating_mul(512)`；`parse_virtual_size`、`human_bytes`、时区偏移等
在附录 A~N 的历次修复里已做过窄类型与边界处理。

**③ 路径遍历：白名单校验，且**确实被调用**。**
`snapshots::valid_id` 拒绝空串、长度 > 64、前导 `.`、`/`、`\`、`..`，
并要求全部字符属于 `[A-Za-z0-9_-]`（**白名单**，不是黑名单）。
它的调用点核对过：`snapshots.rs:255`（回滚路径）与 `:341`，
以及应用商店自己那份 `apps::valid_id`（`apps.rs:162` / `:320`）——
**每个"把 id 当路径用"的地方前面都调了它**（只写不调的校验等于没写，这里不是）。

快照**内部的**文件名由守护进程自己生成（`format!("{i:02}-{}", file_name)`，
来自它自己的 `FILES` 常量表），客户端无法注入。

**④ 并发：锁的中毒处理是有意的。** 全库 13 处锁用
`unwrap_or_else(|e| e.into_inner())`（中毒后继续用内部值），
另有 5 处 `.lock().unwrap()` —— 逐行核对后确认**全部在 `perm.rs` 的
`#[cfg(test)] mod tests` 内**（该模块从 425 行开到底），属测试代码。

### U.2 一条**方法论更正**（比结论本身更值得记）

第 1 遍（附录 T.1）我写的方法局限是"按 `#[cfg(test)]` 切分是近似，
因为本项目测试都在文件末尾"。这次核对 `perm.rs` 时发现它有**四处** `#[cfg(test)]`
（76 / 85 / 180 / 425 行）—— 前两处是**逐条测试辅助**，最后那处才是 `mod tests`。

于是"0 处生产 `unwrap()`"这个结论**不能只靠扫描器**：
它对 `perm.rs` 恰好仍然成立（那 5 处确实在 425 行之后的模块里），
但**成立的原因是人工核对，不是扫描器**。
教训：**自动扫描的结论必须抽样人工复核**，否则你不知道自己信的是工具还是事实。

### U.3 本遍未覆盖（留给第 3 遍）

* **依赖库 CVE**：仍受 `--offline` 限制，未验证；
* **运行时复现**：这一遍全部是**静态阅读 + 调用点核对**，
  没有构造恶意输入去实际触发（例如真的发一条超长报文看它怎么断）；
  第 3 遍计划对前两遍的关键结论做**可运行的取证**；
* **fuzzing / 并发压测**：未做。

---

## 附录 V：**整体审计第 3 遍** —— 复现式取证（2026-10-04）

> 第 1 遍（附录 T）审四类安全基线，第 2 遍（附录 U）审边界与数据流 ——
> 两遍都是**静态阅读 + 调用点核对**。这一遍的任务是：
> **把每一条结论对应到"能跑出来"的东西**，跑不出来的**如实标注**。

### V.1 取证方式

不写新的"演示脚本"，而是**先找已有的测试**：如果一条安全结论已经被测试覆盖，
那它就是**可复现**的（任何人 `cargo test` 都能重跑），比截图或临时脚本强得多。
命令只有一个：

```
cargo test -p aetherd --offline --locked
```

结果：**215 项全过**（0 失败）。

### V.2 结论 → 测试对照表（逐条取证）

| 结论（出自） | 覆盖它的测试（`cargo test -p aetherd` 内） |
|---|---|
| 读取层有硬上限，超长无换行报文会被拒（U.1①） | `server::ipc_gating_tests::read_line_capped_boundaries` |
| 剪贴板超限**整段拒绝**、不覆盖旧内容（U.1①） | `clipboard::tests::oversized_is_rejected_wholesale`、`server::ipc_gating_tests::clipboard_oversized_is_rejected_without_clobbering` |
| 快照 id 含路径分隔/`..` 被拒（U.1③） | `snapshots::tests::snapshot_ids_that_escape_are_rejected` |
| 应用包条目无法逃出包目录（U.1③） | `apps::tests::entry_cannot_escape_the_package`、`lib_names_with_path_separators_are_rejected` |
| 时区名不能逃逸（U.1③） | `timesync::tests::timezone_names_that_escape_are_rejected` |
| 安装目标盘拒绝路径注入（U.1③） | `tools::tests::install_disk_rejects_path_injection` |
| `read_file` 拒绝白名单之外的路径、拒绝凭据文件（含 UI 密钥）（T.2④） | `tools::tests::read_file_rejects_paths_outside_whitelist_on_host`、`credential_paths_including_ui_key_are_rejected` |
| 文件写入超限整段拒绝（T.2④） | `tools::tests::file_write_rejects_oversized_wholesale` |
| 直接下发的电源请求即使来自 UI 通道也被拒（T.2②的 `SystemPower`） | `server::ipc_gating_tests::direct_power_request_is_rejected_even_from_ui_channel` |
| 确认令牌与参数/工具绑定，改了就失效（T.2②） | `perm::tests::token_rejects_changed_args_or_tool`、`unknown_token_rejected` |
| 审计日志超限会轮转（T.2④） | `perm::tests::audit_rotates_when_oversized` |
| DNS 列表只接受 1~3 个 IPv4、其余给理由（U.1①） | `netset::tests::dns_list_rejects_everything_else_with_a_reason` |
| 时间同步拒绝不可信的响应（AG/AF 的修复） | `timesync::tests::rejects_the_responses_that_must_never_be_trusted`、`query_rejects_a_garbage_response` |
| 未注册通道下的请求必须被 403（T.2②，**行为**取证） | `server::ipc_gating_tests::request_variants_are_gated`（真的调 `handle_line` 断言 403） |

**这张表就是第 3 遍的产出**：第 1、2 遍的每条结论，要么在这里找到对应的可运行测试，
要么在 V.4 里明确写"没做到"。

### V.3 第 3 遍新增的**永久**不变量（比取证更进一步）

取证是"我现在跑给你看"；**不变量是"以后它也得成立"**。本遍新增：

```
server::ipc_gating_tests::every_request_handler_is_gated_and_classified
```

它**自动枚举** `server.rs` 里每个 `Request::X` 分支，要求：

1. 每个分支都要**显式归类**（"改状态"或"只读名单"）—— 新分支默认按"改状态"检查；
2. 改状态的分支必须有 UI 通道门（`is_ui`）或"明确拒绝 + 审计"（`audit_or_warn`）；
3. 改状态的分支还要**落审计**（`audit_call` / `audit_or_warn`）；
4. 豁免名单（`Ping`/`SysInfo`/`RegisterUi`）**必须真实存在** ——
   防止名单过期后"测试看着在跑，其实在空转"。

**与已有测试的关系（互补，不是重复）**：仓库里原本就有
`request_variants_are_gated`，它是**行为**测试（真的调用并断言 403），更硬；
但它的用例是**手写清单** `cases` —— 新增变体没加进清单就不会红。
新测试**自动枚举源码**，补的正是这个缺口；并且它额外检查"是否落审计"。
两条测试各守一半，不是一件事做两遍。

### V.4 **跑不出来**的（如实标注，不冒充已证）

| 项 | 状态 |
|---|---|
| 依赖库 CVE | **未验证**：本环境 `--offline`，`cargo audit` 需要网络与漏洞库 |
| 恶意输入的**模糊测试** | **未做** |
| 并发压测 / 竞态复现 | **未做**（第 2 遍只做了锁的中毒处理核对） |
| 真机行为（evdev / fbdev / 挂载 / 电源） | **未实机验证**（本机无对应设备，历批均如此标注） |
| 性能门禁 | 本轮曾因**宿主负载**飘红；已用附录 T.6 的 **A/B 同条件对照**证明不是回归（改动后 25.17 ms 优于改动前 28.21 ms，均通过） |

### V.5 三遍整体审计的总结

| 遍次 | 侧重 | 结果 |
|---|---|---|
| 第 1 遍（附录 T） | 访问控制 / 注入 / 敏感信息 / 不安全默认 | 四线干净；**26/26 处理器合规**；1 处低危（AI 会话内容未脱敏进控制台日志，**有意为之、未改动**） |
| 第 2 遍（附录 U） | 边界与数据流 | **未发现新问题**（读取有上限、换算用饱和、路径白名单**且真被调用**、锁中毒是有意处理） |
| 第 3 遍（附录 V） | 复现式取证 | 每条结论对应到**可运行测试**；新增 1 条**永久不变量**；明确列出**未验证**项 |

三遍下来**没有发现新的安全漏洞**（第 1 遍那一处低危是"策略不一致"而非可利用漏洞），
但产出了两样比"结论"更重要的东西：**一条会红的门禁**（V.3）与
**两次方法论更正**（U.2 的"扫描器结论必须人工复核"、V.3 的"取证要变成不变量"）。

---

## 附录 W：§4.1 第三个切片 —— `Enter` 的**优先级**（2026-10-04 批次 AU-1）

### W.1 为什么挑 `Enter`

回车是界面上"最重"的键，它的语义取决于**当时开着什么**：
确认卡片（权限）> 安装向导 > AI 指令条。这类**链式优先级**在本项目历史上出过事
（附录 K 的守卫顺序、附录 O 的无条件调用）—— 而且它藏在事件循环里，**没有测试**。

改之前它就长这样：

```rust
if let Some((req, echo)) = confirm.as_ref() {
    if req.echo_ok_input(echo) { …放行… } else { …提示回显不匹配… }
} else if installer.open {
    if begin_install(…) { … }
} else if !ai_input.trim().is_empty() {
    …发给 AI…
}
```

三件事都正确，但**正确性没有保障**：把 `else if` 两行换个位置，行为就变了，
而界面看不出任何异常。另外 `confirm.take().map(…).unwrap()` 在事件循环里留了一个 panic 点。

### W.2 改法

```rust
pub enum EnterIntent { Approve, EchoMismatch, StartInstall, SendAi, Nothing }
pub fn enter_intent(has_confirm: bool, echo_ok: bool, installer_open: bool, ai_empty: bool) -> EnterIntent
```

* **优先级成为被测对象** —— 测试里直接构造"同时开着"的组合：
  卡片开着时，即使向导也开着、指令条里还有字，回车也只与卡片有关；
  回显不匹配时**既不放行、也不把指令条的字顺手发出去**；
* **`EchoMismatch` 是独立结果**，不再藏在 `else` 的落点里 ——
  以前它靠"`else` 恰好接在放行之后"才成立，这不够牢；
* 顺手去掉了那个 `unwrap()`：事件循环里不留 panic 点
  （与附录 T 的"守护进程零 `unwrap()`"是同一条取向，这次是**界面进程**）。

### W.3 验证

* `enter_priority_is_confirm_then_installer_then_ai_bar`（新增，含最容易写错的三种"同时开着"）；
* `cargo test` **722 项全绿**；Windows 与 **musl** 双目标 **0 警告**。

### W.4 §4.1 进度（三个切片之后）

| 切片 | 抽出的纯函数 | 对应历史问题 |
|---|---|---|
| AQ-1 | `input::super_intent` | 附录 O / P（无条件调用、写死常量） |
| AR-1 | `input::ctrl_intent` | 附录 Q（手抄事实副本） |
| **AU-1** | **`input::enter_intent`** | 附录 K 那一族（顺序即语义） |

还剩 `Char`（IME / 向导让位 / 确认卡片独占键盘）与 `Nav`（三级分流）两个分支。
`Char` 那一支牵扯输入法状态机，是这几支里最难抽的 —— 要么整支抽，
要么承认"它已经由 `ime` 自己的单测覆盖"并写明理由（**不能假装已经做完**）。

---

## 附录 X：同一份数据，两个上限（2026-10-04 批次 AV-1）

### X.1 问题

「权限与隐私」页的审计日志展示读 **8 条**：

```rust
let info = crate::audit::read(crate::audit::AUDIT_PATH, 8);
```

而同一页的「导出审计日志」读 **200 条**：

```rust
let info = crate::audit::read(crate::audit::AUDIT_PATH, 200);
```

后果不是崩溃，而是**更难受的那种**：用户导出之后在文件里看到一条记录，
回到界面上却找不到它 —— 界面与导出**对同一份历史给出不同的答案**。
缺口清单里把这一项写成「搜索待做」，其实**真正的问题是页面还不是查看器**：
能搜的东西必须在页面上（搜索只是在 8 条里搜，等于没搜）。

### X.2 这一族错误的名字

它是附录 P（模型动态了、某处还写死）与附录 Q（手抄事实副本）的**第三种形态**：
**同一份数据的两个上限**。三者共同的病根都是「同一个事实在多处各写一份」，
区别只在写的是什么：
* P 写的是**常量**（默认桌面数）；
* Q 写的是**规则**（哪个键会关窗）；
* X 写的是**容量**（同一份日志读多少条）。

### X.3 修法

把上限**变成一处定义**：`audit::PAGE_MAX = 200`，
页面与导出都引用它 —— 于是"界面看得到的"与"导出的"永远一致。

同时把这一页补成**查看器**：读满 200 条、按关键词过滤
（工具名 / 参数 / 结论 / 等级，大小写不敏感；空查询=全命中），
并在有搜索词时如实显示「**命中 M / 已读 N 条**」而不是只报总数
（只报总数会让人以为列表被截断）。

搜索逻辑抽成纯函数 `audit::matches` / `audit::matching` 并带测试 ——
"搜不到"与"搜错"是两种不同的 bug，只有测试能分辨。

### X.4 如实记录的**限制**

* 搜索框**不做 IME 拼字**：中文输入法在搜索框里不生效。
  理由：搜索目标是工具名（`read_file`）、结论（`denied_by_user`）、等级（`L2`），
  都是 ASCII；为它接一套拼字状态机得不偿失。**这是限制，不是"已支持"** ——
  代码注释里也这么写。
* 仍**没有滚动**：页面显示"能放下的那些条"，放不下的靠搜索去找
  （先有搜索、再有滚动，是这一批的取舍）。

### X.5 验证

* `audit_search_covers_the_four_fields_and_is_case_insensitive`（新增）；
* `cargo test` **723 项全绿**；Windows 与 **musl** 双目标 **0 警告**。

---

## 附录 Y：一条「限制」其实是**设计选择** —— 关窗撤销（2026-10-04 批次 AW-1）

### Y.1 原始的「不可撤销」是真的，但不是必然

代码注释与缺口清单都写着：**关窗不可撤销，因为窗口含活的 PTY 会话**。
读代码后确认前半句是**事实**：

```rust
let title = desktop.wins.remove(idx).title;   // 窗口被移出来，然后释放
```

`Win` 被 drop，会话随之结束。但那句话里藏着一次**不经意的选择**：
*关窗时把窗口丢掉*。丢掉**不是必需的** —— 不丢就行：
关窗时把窗口移进一个**有界的「最近关闭」托盘**，撤销时放回原位。
会话一直活着，撤销就是「它从未离开」。

**这不是绕过限制，而是把限制的成因去掉。**

### Y.2 代价必须写出来

托盘里的窗口**还活着**（PTY 继续跑、继续占内存），所以：

* 容量**只有 3**（`CLOSED_CAP`），满了丢**最旧**的；
* 被丢掉的那个，它那一步撤销会**如实报「无法撤销」**，而不是放回一个别的窗口；
* `Win` **没有唯一 id**（只有 `title`），撤销只能用标题校验「托盘里最新的是不是它」——
  对不上就拒绝。这是**能力边界**，写在注释里，不假装没有。

### Y.3 本轮真正的方法收获：**「关窗」有三条路**

改之前我以为只有 `Ctrl+W`。实际是三条：

1. 用户的 `Ctrl+W`（`apply_ctrl` → `CtrlOutcome`）；
2. **红灯按钮**（预览路径里直接调 `close_window`）；
3. **AI 的 `close_active` 工具**（`apply_action`，共用 `undo` 栈）。

只改第一条，就会出现「**用哪种方式关的，决定能不能撤销**」——
一种最难发现的 bug：每条路单独看都对。三条路现在都走同一套记录。

**而且 musl 检查在这一批救了场**（本会话第 7 次）：fbdev 主循环是
`#[cfg(target_os = "linux")]`，Windows 构建**看不到**它 ——
我只改了预览路径时，是 `cargo check --target musl` 报出了
「主循环那一支类型不对」。**最主要的用户路径恰恰是 Windows 看不见的那条。**

### Y.4 旧测试的处理

`closing_is_not_undoable_and_does_not_fall_through` 断言的正是**旧行为**，
改动后它变红 —— **这是对的**，它忠实反映了旧设计。处理方式是
**改写成新行为、保留它真正要守的性质**：`Ctrl+Z` 必须先处理「关窗」这一步，
不能跳过去撤销更早的布局改动。**改的是断言，不是性质。**

### Y.5 过程记录：同一类失误连犯 5 次

批量替换时我**五次**因为「锚点里少写了 4 个空格」而断言失败
（脚本原子，所以每次都没写入、没弄坏文件，但每次都白费一轮）。
最后一次改成**正则连缩进一起捕获、并复用匹配到的缩进**就一次通过。
两条教训：

1. 缩进敏感的文件里做批量替换，**不要手抄缩进**（用正则捕获或整行复制）；
2. 跑脚本时我一度用 `Select-String` 过滤它的输出，于是 **Python 的报错被滤掉、
   我误以为改动生效** —— **脚本自己的输出必须看**。

### Y.6 验证与边界

* `closed_tray_keeps_at_most_cap_and_evicts_the_oldest`（容量 0 / 恰好满 / 超出一个）；
* `closing_is_undoable_and_does_not_fall_through`（改写后，保留「不跳步」性质）；
* `cargo test` **724 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* **未实机验证**：真机上按 `Ctrl+W` / 点红灯 / 让 AI 关窗，再 `Ctrl+Z` 看会话是否真的还在。

---

## 附录 Z：i18n 的第一批页面 —— 三处设计选择（2026-10-04 批次 AX-1）

这一批把「权限与隐私」（审计页）的正文与导出文本双语化。它不是安全修复，
但它碰到的三个问题都属于"**分层与边界**"，值得记下来。

### Z.1 数据层不依赖界面层

相对时间与导出文本原本只有中文，而它们住在 `audit.rs`（**数据层**）。
直白的做法是 `use crate::i18n` —— 那会让数据层依赖界面层，
以后单独测 `audit.rs`、或把这一层搬到别处，都会被拖住。

**做法**：数据层收**布尔**（`export_text(info, now, en)`、`Entry::age_text_en`），
界面层用新增的 `i18n::is_en()` 去转。分层保住了，代价只是多一个 3 行函数。

### Z.2 页面英文时，导出也必须英文

导出是"贴到 issue 的文本"，如果页面是英文而导出是中文，
读 issue 的人会以为系统自己没搞清楚语言。所以**导出跟着界面语言走**，
并且新增的测试**逐项断言英文关键词都在、且整份导出里不许有汉字** ——
"半翻译"（头换了英文、条目行还是中文）是最糟的形态，必须被测试挡住。

### Z.3 `format!` 要字面量模板（项目早就写过，我又踩了一次）

我第一版写成 `format!(pick(lang, "共 {} 条…", "{} entries…"), a, b)`，
编译直接报 `format argument must be a string literal`。
项目在 i18n 的文档里**早就写明**：动态文本要**按语言分支**，不能把模板当变量传。
于是改成 `if is_en() { format!("{} entries…") } else { format!("共 {} 条…") }`。

这是"**文档里写着的坑，自己又踩一遍**"——记下来，因为它说明：
知道了规则不等于会用规则，**编译器的报错才是最后一道门**。

### Z.4 本批改动的风险点（musl 检查覆盖）

`export_text` 是 `#[cfg(any(target_os = "linux", test))]`，它的**生产调用点**
在 `main.rs` 的 Linux-only 导出函数里 —— 改它的签名时，**Windows 构建看不到那个调用点**。
所以这一批照例跑了 `cargo check --target x86_64-unknown-linux-musl`（两次），
确认没有"只在 Linux 上编译失败"的漏改。

### Z.5 验证与边界

* 新增 `english_export_is_fully_english`（英文关键词齐全 + **无汉字** + 中文那份没被改坏 + 空记录分语言）；
* 新增 `english_age_text_handles_singular_and_plural`（`1 minute ago` 不是 `1 minutes ago`；时钟回拨仍是 `just now`）；
* `cargo test` **726 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* **未实机验证**：真机上切到英文看这一页（本机没有 evdev/fbdev）。
* **仍未做**：其余设置页正文、通知正文（后者来自几十个发送方，是 §3.5 剩下的大头）。

---

## 附录 AA：「页面 chrome 双语」不等于「整页双语」（2026-10-04 批次 AY-1）

### AA.1 一个页面上的文字有**两个来源**

设置页「网络」的文字来自两处：

| 来源 | 例子 | 在哪一层 | 本批状态 |
|---|---|---|---|
| 页面 chrome | 分组标题「状态」、行标签「接口/MAC/链路/网关」、按钮「重启网络服务」、编辑提示「回车保存 · Esc 取消」 | `draw.rs`（界面层） | **已双语** |
| **DNS 校验失败的原因** | 「最多 3 个 DNS 服务器，收到了 5 个」「暂不支持 IPv6 DNS（…）」 | `aether-ipc::parse_dns_list`（**协议层**） | **仍是中文** |

所以这一批**不是**"网络页已双语"，而是"网络页的 chrome 已双语"。
把这个区别写下来，是因为**最容易发生的失真就是**：改完看得见的部分，
然后在台账里写成"这一页做完了"。

### AA.2 为什么不能直接在协议层翻译

`parse_dns_list` 现在返回 `Result<Vec<Ipv4Addr>, String>`，句子里带参数
（收到了几个、哪个地址不支持）。要在协议层出中英文，就得：

* 让 `aether-ipc` 依赖 `i18n`（**协议层依赖界面层**，分层坏掉）；或
* 在协议层塞一份中英对照表（**同一个事实两处维护**，将来必然漂移）。

两条都不该选。

### AA.3 正确修法（下一批的计划，写清楚以便执行）

1. 协议层改为**返回枚举**：

   ```rust
   #[derive(Debug, Clone, PartialEq, Eq)]
   pub enum DnsError {
       Empty,
       TooMany { got: usize },
       Ipv6 { got: String },
       NotIpv4 { got: String },
   }
   pub fn parse_dns_list(input: &str) -> Result<Vec<Ipv4Addr>, DnsError>
   ```

2. 文案由**界面层**映射（`draw.rs`/`settings.rs` 里 `pick(lang, zh, en)`）；
3. 现有测试从"断言错误句子里有某几个字"改成**断言枚举变体** ——
   这同时让测试更稳（不再绑定文案措辞）；
4. `aetherd` 侧若也要把原因是回给界面，则它只回**枚举的机器名**（如
   `too_many`）或一个中文兜底句，由界面优先用枚举渲染。

### AA.4 本批的验证与边界

* `cargo test` **726 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有**新增测试：改的都是 `draw.rs` 里的字面量替换，
  而这条路径需要**全局语言状态**（`i18n::set_lang`）才能测 ——
  全局状态下的测试会互相干扰（并行跑时改的是同一个全局）。
  这一点如实写出来：**本批的改动没有被自动化测试覆盖**，
  靠的是编译期检查（`pick` 的签名与参数个数）与人工核对。
* **未实机验证**：真机上切英文看这一页。

---

## 附录 AB：执行附录 AA 的计划 —— 协议层不该决定文案（2026-10-04 批次 AZ-1）

### AB.1 做了什么

`aether-ipc::parse_dns_list` 原本返回 `Result<_, String>`，失败原因是**中文句子**。
批次 AY-1 把它记成「网络页 chrome 已双语、校验原因仍是中文」，并写下计划。
这一批把计划执行完：

```rust
pub enum DnsError {
    Empty,
    TooMany { got: usize },
    Ipv6 { got: String },
    NotIpv4 { got: String },
    Loopback { got: String },
}
pub fn parse_dns_list(input: &str) -> Result<Vec<Ipv4Addr>, DnsError>
```

* **协议层只说"为什么失败"**（连参数一起带上）；
* **文案在界面层**：`settings::dns_error_text(lang, &e)` 是**纯函数**，
  中英文两套文案、五种原因，全部有测试；
* `DnsError` 实现 `Display` 时用**中性英文** —— 那是给 `?`/`anyhow`/日志用的
  机器描述，**不是**给最终用户看的文案。另外给了 `as_str()`（机器名，写日志与回执用）。

### AB.2 穷尽匹配把"漏掉的那一类"逼了出来

我最初只写了四种原因（空 / 太多 / IPv6 / 非法 IPv4）。改到 `dns_error_text` 时，
`match` **必须穷尽** —— 编译器当场问「`Loopback` 怎么办」，
我才回去补上「回环地址不能作为上游 DNS」这一类（它原本在 `parse_dns_list` 里
以 `format!` 的形式存在，很容易在重构时被漏掉）。

这是"**类型系统替你查清单**"的一个具体例子：把句子换成枚举之后，
**失败原因是有限的几种**这件事第一次被写进了类型里。

### AB.3 `aetherd` 侧的兜底是中文（如实说明）

`aetherd` 不掌握界面语言，所以它在拒绝时回一句**中文兜底**
（`DNS 设置被拒（{机器名}）：{Display}`）。正常路径下用户看不到它 ——
界面自己用**同一份** `parse_dns_list` 先校验过（`DnsField::validate`）。
只有"绕过界面直接发 IPC"才会看到这句。**这是有意的不对称**，不是漏改。

### AB.4 测试的变化（这次是"变强"）

* 协议层测试原来断言"错误字符串非空"，现在断言**枚举**：不再绑定文案措辞
  （改一句话不会让测试红，改了**原因类别**才会）；
* 新增 `dns_error_text_covers_all_five_reasons_in_both_languages`：
  五种原因 × 两种语言，且**带参数的原因必须把参数写进文案**（否则用户不知道该改什么）、
  **英文文案不许有汉字**；
* `cargo test` **727 项全绿**（+1）。

### AB.5 过程记录：内联 Python 又改坏了引号

用内联 Python 做替换时，PowerShell 把双引号吃掉，写进 Rust 的成了
`'机器名不能为空'` —— 单引号在 Rust 里是 **char 字面量**，编译器报
"character literal may only contain one codepoint"。

**本会话第三次栽在"内联脚本 + 引号"上**。规矩早就有了（一律写脚本文件），
这次又侥幸图快。记在这里：**内联脚本只用于不含引号的替换**。

### AB.6 边界

* **未实机验证**：真机上切英文，故意填错 DNS，看提示是否英文；
* 「通知正文」与其余设置页正文仍未做（§3.5 剩下的大头）。

---

## 附录 AC：i18n 第三批 —— 顺便修掉一句**过期的自述**（2026-10-04 批次 BA-1）

### AC.1 本批做了什么

「外观」页（19 处）+「应用」页说明 +「应用商店」页说明的双语化。
做法与上两批一致：页面块内把中文字面量包成 `pick(lang, zh, en)`，
带参数的（`{min} 分钟`）按语言分支（`format!` 的模板必须是字面量）。

### AC.2 顺手修掉的**过期自述**（这才是本批里最该记的一条）

「界面语言」那一行原本写着：

> 切换后左栏与页标题立即生效；**正文尚未翻译**（见缺口清单 §3.5）

这句话在写下时是**真的**（当时几乎没有正文被翻译）。但做完审计页、网络页之后
它就成了**假话** —— 界面在对自己说谎，而且用户会照它去理解这个开关的作用。

改成「**部分页面的正文仍在翻译中**」，英文同步。

**为什么值得单独记**：这类"随着工作推进而变成假话的说明文字"不会被任何测试发现
（它只是一句话），也不会让编译失败。它只会让**读它的人**判断错。
本项目对"如实"的要求里，这一条同样适用 —— **代码注释与界面自述也要跟着事实走**，
不能只在书写那一刻是真的。

### AC.3 过程记录：替换必须**限定在页面块内**

`"深色模式"` 这个字面量在文件里出现 **3 次**（外观页、控制中心、向导弹出的选项）。
第一次尝试全局替换时，脚本按设计**断言失败并中止**（原子写入，文件没坏）。

改成"先切出页面块、只在块内替换"就一次通过。又一次印证本轮学到的规矩：
**批量替换前先问"这个锚点在全文件里唯一吗"**；不唯一就**限定范围**，
而不是硬凑一个更长的锚点（更长也可能撞上）。

### AC.4 验证与边界

* `cargo test` **727 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（同 AY-1：这条路径要全局语言状态，
  并行测试下会互相干扰）—— 靠编译期检查（`pick` 参数个数）+ 人工核对；
* **未实机验证**：真机切英文看「外观」页；
* **仍未做**：工具权限/快照/显示输出/可移动设备/时钟/系统更新/关于/输入/AI/快捷键
  这些页面的正文，以及通知正文（几十个发送方）。

---

## 附录 AD：i18n 第四批 + 一次**被工具误导、又用手核对回来**（2026-10-04 批次 BB-1）

### AD.1 本批做了什么

「工具权限」（2 处）、「关于」（9 处：版本/许可证/架构/内核/设置文件，以及
电源三行的标题与说明）、「快捷键」（1 处）双语化。

### AD.2 我的一次方法论失误（值得记）

为了挑"收益最大的页面"，我写了个小脚本统计各页面的中文残留。
它给出「通知 149 行」—— 我据此以为通知页是最大的一块。
**手动看那一段代码才发现：那 149 行里绝大多数是行内注释**（`// …`），
脚本只排除了**整行**注释，没排除**行尾**注释。真实残留只有 12 行左右。

第二次普查（`line.split('//')[0]`）我以为是修正过的，**它仍然不可靠**：
它按"这一行有没有 `tr(`/`pick(`"判断是否已翻译，而**多行 `pick(...)` 的中文参数
自己占一行**，于是把已经翻译好的行又数了一遍。同一件事上，我**连续两次**
被自己的脚本误导。

**能用的核对方式只有一个：逐页人工读。** 本批据此发现「应用」页还有 8 处真残留
（脚本这次是对的），已经补完；其余被 flag 的行经人工确认都是多行 `pick` 的参数行。

所以不再贴"每页残留行数"（那个数字没有意义），只贴**人工核对过的结论**：
已整页双语的页面 = 审计页（含导出）· 网络页（含 DNS 失败原因）· 外观页 · 应用页 ·
应用商店页 · 工具权限页 · 关于页 · 快捷键页。
仍未做 = 输入 · AI · 时钟与时区 · 可移动设备 · 显示输出 · 系统更新 · 快照与回滚 ·
通知，以及通知正文（几十个发送方）。

* `通知`：144 行
* `输入`：10 行
* `权限与隐私`：9 行
* `应用`：9 行
* `AI`：7 行
* `网络`：5 行
* `时钟与时区`：5 行
* `可移动设备`：4 行
* `外观`：4 行
* `显示输出`：3 行
* `系统更新`：2 行
* `快照与回滚`：2 行
* `应用商店`：1 行
* `工具权限`：1 行
* `快捷键`：1 行
* `关于`：1 行

合计 **208 行**，分布在 **16** 个页面；
已整页清空的有：。

**教训**：脚本给的数字要**抽样人工核对**再拿去做决策 ——
这与附录 U.2 记的是同一条（那次是"按 `#[cfg(test)]` 切分"的近似）。
同一个坑在同一个会话里出现两次，说明它值得写在最显眼的地方。

### AD.3 边界

* `cargo test` **727 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批与 AY-1/BA-1 一样**没有新增测试**（路径依赖全局语言状态）——
  靠编译期检查 + 人工核对；
* **未实机验证**：真机切英文看这三页；
* **仍未做**：上表列出的页面正文，以及通知正文（**几十个发送方**：
  通知文案由 `push_toast`/`notifications` 的各个调用点产生，
  要逐处改成按语言取词，是 §3.5 剩下的最大一块）。

---

## 附录 AE：设置页全部双语 —— 以及"页面 arm 之后不是这个页面"（2026-10-04 批次 BC-1）

### AE.1 本批做了什么

五个页面 + 一处漏网：时钟与时区（4）· 输入（含**键盘快捷键表** 6）· AI（7）·
系统更新（1）· 快照与回滚（1）· 通知页 chrome（5）。

至此**16 个设置页全部双语**。核对方式同前几批：逐页人工读（见 AE.3），
外加编译期检查（`pick` 的参数个数与签名）。

### AE.2 意外收获：两页**本来就是双语的**

我上一轮那份"残留统计"把 `可移动设备` 与 `显示输出` 标成有 4 处与 3 处残留。
人工读的时候发现：**它们早就全部用 `tr(...)` 了** ——
被 flag 的是**多行 `tr(...)` 的中文参数行**。
这正是附录 AD 记下的那个陷阱，**第三次**在同一个会话里出现。

### AE.3 "页面 arm 之后不是这个页面"

写下这份附录的直接原因：我先按"从 `"输入" => {` 到下一个页面 arm"圈范围，
结果**漏掉了「输入」页里最后那一块键盘快捷键表**（它在下一个 arm 之前，
但我上一轮的打印范围用了错误的结束行）。

教训：在这种**长 `match` 里按 arm 划范围**时，"下一个 arm"必须真的取到
（我那次把范围写成了固定行号）。更稳的做法是：**按 arm 列表遍历**，
而不是手抄两端的行号 —— 本批的最后一次核对就是这么做的。

### AE.4 核对方式与它的**不可靠之处**（如实写）

最后那次核对打印每个 arm 里的中文字符串行，并**回看两行**判断它是否已经在
`tr(`/`pick(` 里。它仍然留下 6 处 ⚠，**全部是 `if is_en() { … } else { … }`
的 `else` 分支里的中文字面量** —— 也就是**正确的**（那是给中文用的那一支）。
所以这类"行扫描"永远需要人工过一遍；它只用来**缩小要看的地方**。

### AE.5 仍未做（另一条战线，说清楚不是漏掉）

* **窗口类界面**：终端窗口标题与状态行、文件管理器、安装器、确认卡片、
  AI 命令栏提示 —— 这些不是设置页，是**窗口内容**；它们的文案同样要按语言取词，
  但改动面在另一批代码里（`draw.rs` 的窗口绘制段与若干 helper）；
* **通知正文**：由几十个 `push_toast`/`notifications` 调用点产生，
  要逐处改成按语言取词 —— 这是 §3.5 剩下的**最大一块**。

### AE.6 验证

* `cargo test` **727 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（同 AY-1/BA-1/BB-1：这条路径依赖全局语言状态）——
  如实写在这里，不假装它被自动化覆盖了；
* **未实机验证**：真机切英文逐页看。

---

## 附录 AF：窗口类界面（第一批）—— 按**函数**圈范围（2026-10-04 批次 BD-1）

### AF.1 本批做了什么

设置页做完之后，剩下的 i18n 在**窗口内容**里（不是页面分派）。本批做前三个绘制函数：

| 函数 | 做了什么 |
|---|---|
| `draw_term_content` | 「（终端会话未建立）」、「会话已结束（状态 N）· 点 Dock 里的终端可再开一个」 |
| `draw_files_content` | 「← 上级目录」、「无法读取目录：…」、「a–b / N 项」、「N 项」 |
| `draw_preview_content` | 「（无内容）」、「无法预览」、「… 空文件」、两种分页说明（含"未全部载入"） |

### AF.2 方法：按函数圈范围，不按页面 arm

前一批是在 `match title` 的 arm 之间划范围。窗口内容**没有 arm** ——
它们是若干独立绘制函数。所以本批改用"**函数名 → 下一个 `fn` 之前**"圈范围，
并且在最后核对时**按 arm/函数列表遍历**，而不是手抄两端的行号
（上一批就是因为手抄行号漏掉了「输入」页的快捷键表，见附录 AE.3）。

### AF.3 自动核对又双叒把 `else` 支当成残留（第四次）

最后核对打印"没有 `tr(`/`pick(` 上下文的中文字符串行"，6 处 ⚠ **全部**是
`if is_en() { format!("English…") } else { format!("中文…") }` 的 **`else` 支** ——
也就是**正确的**（给中文用的那一支）。逐条回看 4 行即可确认。

这已经是同一个模式在会话里**第四次**出现（AD / AE / 本附录，AE 里还叠了三次）。
**结论写死在这里**：这类行扫描只能用来"缩小要看的地方"，
**"是否真的遗漏"必须人工看上下文**。我不再指望它能直接给出结论。

### AF.4 仍未做（精确到函数，方便下一批直接开工）

* **安装器**（`draw_installer*` 相关绘制）：选盘提示「择目标磁盘并确认。整盘覆盖…」、
  「N MB 可用」、「正在写入磁盘，请勿关机…」、「就绪。也可以选中磁盘后按 Enter 开始。」、
  四个阶段按钮（开始安装（整盘覆盖）/ 安装中… / 完成 · 重启后从磁盘引导 / 重试安装）；
* **确认卡片**：工具名表（`install_disk`/`desktop`/`read_file`/`sys_probe`/`sys_info` 的中文名）、
  等级徽标（「L3 危险」/「敏感写」）、「工具」表头、「L3 危险操作：请原样输入 {target} 以确认」；
* **通知正文**：由几十个 `push_toast`/`notifications` 调用点产生 —— 剩下最大的一块。

### AF.5 验证

* `cargo test` **727 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（同前几批：依赖全局语言状态）—— 如实写，不假装被覆盖；
* **未实机验证**：真机切英文看终端/文件管理器/预览窗口。

---

## 附录 AG：安装器与确认卡片 —— 以及"定位错了函数"（2026-10-04 批次 BE-1）

### AG.1 本批做了什么

| 位置 | 内容 |
|---|---|
| 安装器 | 选盘提示（"整盘覆盖，目标盘上的数据将丢失"）· 「N MB 可用」· 「正在写入磁盘，请勿关机…」· 「就绪…按 Enter 开始」· **四阶段按钮**（开始安装（整盘覆盖）/ 安装中… / 完成 · 重启后从磁盘引导 / 重试安装） |
| 确认卡片 | 工具名表的**中英对照**（`install_disk`/`desktop`/`read_file`/`sys_probe`/`sys_info`）· 等级徽标（`L3 危险` / `敏感写` → `danger` / `sensitive write`）· 「工具」表头 · L3 的「请原样输入 {target} 以确认」 |
| 顶栏 | 通知徽标「通知（N 条未读）」 |

两个做法上的细节：

* `tool_label(tool, en: bool)` —— 收**布尔**而不是内部读全局：
  与 `audit::age_text_en` / `export_text(…, en)` 同一套（数据/绘制工具保持纯）；
* 四阶段按钮先取 `(中文, English)` 再 `pick` —— 比写两遍 `match` 短，
  而且**不会出现"新增一个阶段却只加了一种语言"**。

### AG.2 我犯的错：把 `lang` 加到了**错的函数**里

我按"函数名 → 下一个 `fn`"的规矩插入 `let lang`，但**选的函数是错的**：
我以为安装器在 `draw_resize_cursor` 里（那是上一轮清点时看行号得出的印象），
实际它在**独立的 `draw_installer`**（5712 行）。

后果是一串**很难懂的报错**：

```
error[E0423]: expected value, found built-in attribute `lang`
help: consider importing this function instead
   | use crate::i18n::lang;
```

编译器把 `lang` 解析成了同名**函数**（不是变量），于是说"这不是一个值"。
真正的原因是我在**另一个函数**里加了那个 `let`。

**教训（这是同一个根因的第四个变体）**：
"不要手抄行号/缩进"（AE.3）· "脚本结论必须人工核对"（AD）·
这一次是"**不要凭印象判断某段代码在哪个函数里**" ——
**插入前先按名字确认所属函数**（`grep` 一次的成本，远低于读懂那条报错）。

### AG.3 仍未做

* **预览的 3 条错误消息**（`read_preview` 返回"这是一个目录"/"文件过大…"/"二进制文件…"）：
  它在**数据层**产生中文句子。按 AZ-1 的模式应改成返回**枚举**
  （`PreviewError::{IsDir, TooLarge(usize), Binary}`）由界面渲染 —— 计划写在这里，下一批做；
* **通知正文**：几十个 `push_toast`/`notifications` 调用点，剩下最大的一块。

### AG.4 验证

* `cargo test` **727 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（同前几批：依赖全局语言状态）—— 如实写，不假装被覆盖；
* **未实机验证**：真机切英文看安装器与确认卡片（这两个界面在真机上才会出现，
  安装器更是**本会话禁止触碰**的路径 —— 只做代码级验证）。

---

## 附录 AH：预览错误改成枚举 —— 以及**同一个坑第二次**（2026-10-04 批次 BF-1）

### AH.1 本批做了什么

执行附录 AG 的计划：`read_preview` 不再返回中文句子，而是返回**枚举**：

```rust
pub enum PreviewError { IsDir, TooLarge(String), Binary, Io(String), Message(String) }
pub fn preview_error_text(lang: Lang, e: &PreviewError) -> String   // 纯函数，有测试
```

四种"为什么预览不了" + 两种**原样透传**（`Io` 是系统的文本、`Message` 是界面自己的文案）。
`draw_preview_content` 通过渲染函数显示；新增测试覆盖四种原因 × 两种语言，
并断言**英文文案里不许有汉字**。

### AH.2 一个**意外发现**：通知中心复用了 `PreviewData`

改完之后编译器报 `main.rs` 两处类型不符 —— 那里在构造 `PreviewData` 表达
通知中心的**空态**（"还没有通知" / "通知记录已关闭…"）。
所以 `Message(String)` 这个变体不是"设计上的优雅"，而是**被现实逼出来的**：
那两句是**界面**说的，不是数据层说的；界面按语言取好后放进去，
数据层只负责带着它走（与 `Io` 同等对待）。

顺带把这两句也双语了 —— 它们属于"通知"这条线，是**计划外**的收获。

### AH.3 同一个坑，**第二次**（这次没有借口）

我把 `PreviewError` 插到了 `#[derive(Clone, Debug)]` 与 `pub struct PreviewData` **之间** ——
derive 于是落到了我的枚举上，而枚举自己也带 derive，编译器报
"conflicting implementations of trait `Clone`"。

**这个坑在第 48 轮已经踩过并写进过注释**（当时是 `#[derive]` + `enum Layout`、
以及 `#[cfg(...)]` + `fn spawn`）。同一轮里我还犯了第二次：搬家时**只搬走了枚举**、
把 `preview_error_text` 函数留在原地，于是 derive 又落到了函数上（"derive may only be
applied to structs"）。

**把规矩再写一遍，这次写在附录里**：插入代码时，
**永远不要贴在"属性与它修饰的条目"之间**；搬家时，
**先确认这一块到底包含哪些条目**（我这次只搬了半个块）。
两个错误各花了一轮编译，而且都是**已经写下来过的教训**。

### AH.4 仍未做

* 通知窗口**标题**（`main.rs` 里 `format!("通知（共 {} 条）")`）—— 与它的测试断言绑在一起，
  要一起改（下一批）；
* **通知正文**：几十个 `push_toast`/`notifications` 调用点 —— 剩下最大的一块。

### AH.5 验证

* 新增 `preview_error_text_covers_all_reasons_in_both_languages`（四种原因 × 两语言 +
  英文无汉字 + 带参数者参数必须出现 + `Io`/`Message` 原样透传）；
* `cargo test` **728 项全绿**（+1）；Windows 与 **musl** 双目标 **0 警告**；
* 两处**测试断言**跟着改了：枚举之后不能再把错误当字符串断言
  （改成用 `preview_error_text(Lang::Zh, …)` 取文案再断言）——
  这次**测试是跟着实现走的**，不是放宽断言；
* **未实机验证**：真机切英文点开一个目录/二进制文件看预览提示。

---

## 附录 AI：通知正文第一批 —— 停止"猜出现几次"（2026-10-04 批次 BG-1）

### AI.1 本批做了什么

* 新增助手 `pick_zh_en(zh, en) -> String`：**"这条话必须有两种语言"在代码里变得可见**；
* 通知正文**第一批 18 处**无占位符的提示（"没有可移动的窗口"、"回显不匹配…"、
  "上一步还没完成，请稍候"、"正在创建配置快照…"、"已取消回滚"、"正在对时…"、
  "这一项已安装（或不可安装）"、"外观已恢复默认" 等，共 18 处）；
* **通知窗口标题**（`main.rs` 的「通知（共 N 条）」→ `Notifications (N total)`）。

### AI.2 一个**部分的**"单一来源"（说清楚它做到了哪一步）

「上一步还没完成，请稍候」原本在 **3 处**各写一份。现在它们都调 `pick_zh_en`，
**渲染逻辑**只剩一份；但三个调用点**仍然各自写着那两句中文** ——
真正的"单一来源"应该是给每条常用提示一个具名常量（如 `MSG_BUSY`）。

我没有做到那一步，理由是：这个文件里这类提示有几十条，逐条命名会让调用点更难读，
而收益（避免改语言时漏一处）**已经被"能在某处集中改"部分覆盖**。
**这是权衡，不是完成** —— 写在这里，免得后面的人以为它已经是单一来源。

### AI.3 停止"猜某句话出现几次"（这条是本批最该记的）

我前面**连续三次**因为"估计某句中文字面量出现几次"而让脚本断言失败并中止
（`"中文输入：开/关"` 其实出现 2 次、`"回显不匹配…"` 3 次、`"上一步…"` 3 次）。
每次都白跑一轮。

最后改成**宽容策略**：对每个中文字面量**替换全部出现处**，并**打印实际条数**。
这些字面量全是用户可见文案，**每一处都该改**，所以"全部替换"就是正确行为；
打印条数是让人能核对，而不是盲改。

**教训**：当"全部替换"就是正确语义时，**不要先猜个数** ——
直接全替换 + 打印条数。猜个数只在"必须区别对待"时才有价值。

### AI.4 又一次内联 Python 毁引号（第四次）

我用内联 Python 去改脚本里的计数，PowerShell 又把引号吃掉，
脚本直接语法错误。**这是本会话第四次**栽在"内联脚本 + 引号"上。
规矩早就有（一律写脚本文件），而我仍然在"只改一行"时图快。
**再写一遍**：内联脚本只用于**不含引号**的替换。

### AI.5 仍未做：通知正文剩 19 处（都带占位符）

它们是 `format!("…{}…")` 形式，必须按语言分支（`format!` 的模板要字面量）：

| 行号 | 文案 |
|---|---|
| 595 | 已移到桌面 {} |
| 720 / 1113 | 已拒绝「{}」 |
| 920 | {brief}（共 {} 项） |
| 1023 | Aether 执行 · {msg} |
| 1082 | 已执行 · {} |
| 1172 | 桌面数量已达上限（{}） |
| 1186 | 已移除桌面（还剩 {} 个） |
| 1254 | 正在重启服务 {unit}… |
| 1263 | 请在确认卡片里输入 {a} 以确认 |
| 1374 | 正在校验并暂存 {name}… |
| 1391 | 正在回滚到 {confirmed}… |
| 1443 | 正在写入系统时区：{name} |
| 1510 | 正在请求卸载：{id} |
| 1548 | 正在请求安装：{} |
| 1587 | 正在保存 {tool}… |
| 1620 | 审计日志已导出：{path} |
| 1626 | 导出失败：{e} |
| 1682 | 设置未能写入磁盘：{e} |

（行号为批次提交时快照，下一批按**内容**定位，不按行号 —— 见附录 AG 的教训。）

### AI.6 验证

* `cargo test` **728 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（文案替换，路径依赖全局语言状态）；
* 通知标题的**既有测试**（断言标题以「通知（共 」开头）在**默认中文**下仍然通过 ——
  它依赖"默认语言是中文"这一事实；若将来有测试把语言切成英文，它会受影响（未改，如实记）。
* **未实机验证**：真机切英文看提示条。

---

## 附录 AJ：提示条文案清零 —— 以及 musl 抓到的「按值收参」（2026-10-04 批次 BH-1）

### AJ.1 本批做了什么

* 新增 `pick_zh_en_1(zh, en, arg)`：**带一个占位符**的版本。
  为什么不能把模板当变量传 —— `format!` 要求模板是**字面量**，
  于是改用「模板里写 `{}` + `replacen` 第一个」，让两种语言的模板**并排在调用点**上；
* 通知正文第二批 **18 个模板 / 21 处**（含多处重复出现的：「已拒绝「{}」」3 处、
  「Aether 执行 · {}」2 处、「（共 {} 项）」2 处等）；
* 最后 3 处（多占位符）改成语言分支：切桌面提示（3 个占位符）、新增桌面提示（2 个）、
  挂载/卸载提示（各 1 个 → 用助手）。

清点结果：**`push_toast` 里已无中文**（本批开始时 37 处）。

### AJ.2 musl 检查抓到 Windows 看不见的**真错**（本会话第 8 次）

`pick_zh_en_1` 按**值**收 `impl Display`。于是这一处：

    pick_zh_en_1("正在请求安装：{}", "Requesting install: {}", renderer.store.items()[i].name)

**把 `String` 字段从 `&[CatalogItem]` 切片里移动了出来** ——
Windows 构建看不到（那段在 Linux-only 的输入循环里），
`cargo check --target x86_64-unknown-linux-musl` 报
`E0508: cannot move out of type [CatalogItem], a non-copy slice`。

修法是 `clone()`，并把「**本助手按值收参**」写进它的文档注释 ——
`format!` 是**按引用**取参的，所以从 `format!` 换到助手时**语义悄悄变了**。
这种「换个 API 就改了所有权语义」的坑，只读调用点看不出来。

### AJ.3 我自己造的一个 bug（当场发现并修）

「{brief}（共 {} 项）」有**两个**占位符，而助手只替换**第一个** ——
真跑起来会显示成「网页（共 {} 项）」（字面的大括号）。
发现后改成语言分支，并加了一次程序化核查（`zh 模板里占位符 >1 的：无`）。

**教训**：给「模板 + 占位符」做助手时，**必须检查模板里到底有几个占位符**；
只支持一个，就别让两个的调用点溜进来。

### AJ.4 仍未做（如实计数）

`push_toast` 已清零；剩下的是**其它**用户可见文案，其中绝大多数是
`tty_log(...)` 的**串口诊断日志** —— 那是**有意保留的中文**（开发者排障用，
见附录 T.3 关于控制台日志的说明），不在 §3.5 范围内。
其余疑似项已在清点输出里逐条列出（本轮输出里可见）。

### AJ.5 验证

* `cargo test` **728 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 本批**没有新增测试**（文案替换 + 助手，路径依赖全局语言状态）；
* **未实机验证**：真机切英文看提示条。

---

## 附录 AK：§3.5 的真实范围 —— 一次**自我纠正**（2026-10-04 批次 BI-1）

### AK.1 本批做了什么

* `AudioState::label(en: bool)`（收布尔，与 `tool_label`/`age_text_en` 同一套）+ `pick_label`
  让「音量不可调」这条错误消息也分语言；
* 测试同步加强：三种状态的文案**在两种语言下都互不相同**、英文不得含汉字、
  并且补上了「0 与 2 也不同」（原来那条在循环外，`A≠B 且 B≠C` **推不出** `A≠C`）。

### AK.2 更重要的：**修正 §3.5 的范围**

前几批我一直把 §3.5 写成"大部完成"，依据是**合成器**里的清点。
本批做了一次**全库（7 个 crate）清点**，结论要更正：

| crate | 仍未做（用户可见） |
|---|---|
| `aether-compositor` | 基本完成（页面/窗口/提示条/音频状态/预览错误） |
| `aetherd` | **AI 回复模板**（`intent.rs`，例如「好的，已把窗口排成两列。」）· IPC 错误消息 |
| `aether-init` | 电源/服务回执（例如「已受理 system_power：正在停止服务并断电」） |
| `aether-ops` | **诊断报告正文**（例如「服务「{unit}」异常退出」） |
| `aether-install` | 命令行用法与错误文案（例如「未知参数「{other}」（用法: …）」） |
| `aether-ipc` | 2 条对端校验错误（`SO_PEERCRED`） |

**两处不是遗漏，要说清楚**：

* `aetherd/src/intent.rs` 里的**中文关键词表**（`contains_any(&t, &["两列", "排成两列", …])`）
  是"AI 怎么听懂中文"的**输入匹配**，删掉它中文用户就用不了 —— **它不该被翻译**；
* `tty_log(...)` 的串口诊断日志是有意保留的中文（见附录 T.3）。

**为什么这次更正值得单独写**：我先前那句"§3.5 大部完成"**不是假话，但它给的印象偏了** ——
它让读者以为只剩零头，而实际上还有五个 crate 的文案没动。
清点方式从"我改过的那几个文件"换成"全库按 crate 扫"，才看见全貌。
**教训**：范围陈述必须说清**是对谁而言的**（"合成器侧" vs "全仓"）。

### AK.3 musl 检查又抓到一处（本会话第 9 次）

把 `AudioState::label` 改成收参数之后，**Linux-only** 的那处调用
（`main.rs` 里 `tty_log(&format!("音频：{}（音量 {}%…）", audio.state().label(), …))`）
在 Windows 构建里**根本不存在**，是 `cargo check --target x86_64-unknown-linux-musl`
报出「this method takes 1 argument but 0 arguments were supplied」。

这是本会话第 9 次由 musl 检查拦下"只在 Linux 上编译的代码"的问题。
**结论**：改任何带平台门控的函数签名时，跑 musl 检查是**必须**的一步，不是可选项。

### AK.4 我这次差点犯的另一个错（幸好先查了）

我一度认为 `SoundTheme::label_en()` 是**死代码**（grep 没找到调用点），
按"要么用要么删"的规矩差点删掉它。多看了一眼 `draw.rs:4512` 才发现：
那里**已经在用** `pick(lang, theme.label(), theme.label_en())` —— 我的 grep 结果被截断了。

**教训**："没搜到"不等于"不存在"；按 grep 结果删代码之前，
**要看清楚搜索有没有被截断/限定范围**。

### AK.5 验证

* `cargo test` **728 项全绿**；Windows 与 **musl** 双目标 **0 警告**；
* 审计记入 **附录 AK**；缺口清单 §3.5 已按真实范围改写（并列出五个 crate 的未做项）。

---

## 附录 BK：把语言带进 aetherd —— 以及一次**测量假象差点让我改错历史**（2026-10-04 批次 BJ-1）

### BK.1 本批做了什么

快速意图通道（`aetherd`）的回复文案双语化：

| 位置 | 内容 |
|---|---|
| 协议 | `Request::Chat` 增加 `#[serde(default)] en: bool` |
| `intent.rs` | `try_handle(text, en)` + 6 条回复（两列/三列/独占/自由布局/整理/时间）+ 助手 `reply(en, zh, english)` |
| `main.rs` | `sys_brief(en)`（取不到时的说明、内存、运行时长、服务总览） |
| `server.rs` | `handle_chat(…, en, …)`，从请求里取 |
| 合成器 | 发送 `Chat` 时带上 `crate::i18n::is_en()` |
| 测试 | +2：`fast_intents_reply_in_both_languages`、`chat_request_without_lang_field_still_decodes` |

### BK.2 语言从哪来：三个方案与选择

1. 让 `aetherd` 去读 `/var/lib/aether/settings.json` —— 改动小，但**两个进程读同一份文件**，
   是隐式耦合，且失败时没有明确回退语义；
2. **在协议里加可选字段，由合成器随请求带来** ← **选它**：语言本来就是合成器的状态，
   显式传递；`#[serde(default)]` 让旧客户端照常工作；
3. 让 `aetherd` 自己存一份语言 —— 两份必然漂移，否决。

顺带一个副作用：这也让"AI 回复用什么语言"**可测** —— 见 BK.3 的测试。

### BK.3 我第一版测试写错了，是它自己报的错

为了证明"向后兼容"，我手写了一段 JSON：

    {"Chat":{"session_id":"s1","text":"hi"}}

它**解析失败**：`missing field \`type\``。协议其实是 **`type` 内部标签**的形式，
我凭印象写成了外部标签。

改成**从 `encode` 自己的输出里删掉 `en` 字段**来构造"旧版本的请求" ——
不猜格式，还顺手断言"删得掉"（编码形状若变，这条会提醒测试需要更新）。

**教训**：要证明兼容性，就**用被测代码自己的编码器**造数据，别手写协议样本。

### BK.4 一次**测量假象**差点让我"纠正"本来正确的历史（本批最该记）

我把测试输出求和时得到 **498**，而历史台账一直写 **727/728**。我一度判断
"我以前的数字是**两次运行相加**、虚高"，甚至准备去改历史。

多查了一步才看清：那次命令的输出**被工具截断了** —— 单次 `cargo test --workspace`
其实有 **8 个测试二进制**，合计 **730 通过 / 0 失败**。
历史数字是**对的**；错的是我这一次的**测量方式**。

正确做法（已经写死在这里）：**把测试输出重定向到文件，再统计文件**，
不要依赖终端里那一屏被截断的输出。

这已经是本会话同类问题的第三次：附录 AK 的 `grep` 被 `Select-Object -First` 截断、
`SoundTheme::label_en` 被截断的 grep 判成死代码，这次是**测试输出被截断**。
三次的形状一样：**我拿"看到的片段"当"全部"。**

### BK.5 仍未做

`aetherd` 的其它 IPC 错误消息（数量多，需要分批）· `aether-init` 的电源/服务回执 ·
`aether-ops` 的诊断报告正文 · `aether-install` 的命令行文案 · `aether-ipc` 的 2 条对端校验错误。

### BK.6 验证

* **单次** `cargo test --workspace`：**730 通过 / 0 失败**（8 个测试二进制；输出已落盘再统计）；
* Windows 与 **musl** 双目标 **0 警告**（`aetherd` 因 `ring` 需要 musl-gcc，本机不可查 —— 如实说明）；
* **未实机验证**：真机切英文问一句"排成两列"看回复。

---

## 附录 BL：aether-init 回执与对端校验错误 —— 一条分层规矩（2026-10-04 批次 BK-1）

### BL.1 一条规矩（本批把它写死）

> **会被用户看到的文案 → 跟着界面语言；只进日志/只属协议层的 → 中性英文。**

据此本批的处理是：

| 位置 | 会不会被用户看到 | 怎么改 |
|---|---|---|
| `ServiceAck.message` | **会**（合成器走 `AiEvent::ToolDone` 显示） | 加 `ServiceControl{en}`，一路透传：合成器 → aetherd → init |
| `check_peer` 的两条拒绝原因 | 不会（调用方只打日志） | **中性英文** |
| 超长请求行的 io 错误 | 不会 | **中性英文** |
| "aether-init 只处理这些请求" | 不会（协议误用） | **中性英文** |
| `PowerAck.message` | **不会**（合成器根本没引用 `PowerAck`） | **中性英文** |

`ServiceControl{en}` 实际只翻一条文案（"服务不存在"）——
因为状态本身是 `format!("{:?}", state)`，`Running`/`Stopped` 本来就是英文。
为一条文案加一个协议字段看着不划算，但**它同时也是"语言从哪来"的唯一入口**；
将来 init 若有更多回执文案，不必再改协议。

### BL.2 本轮踩到的两件事（都是"我以为只有一个"）

1. **同一个函数名有两个 `#[cfg]` 变体**：`control_init_service` 在 `aetherd` 里有
   **Linux 实现**和**非 Linux 桩**两份。我只改了报错里指出的那一份，
   第二次编译又报一次 —— 才知道有两份。
2. **精确字符串锚点漏了多行写法**：Linux 实现里的请求构造是

       aether_ipc::encode(&aether_ipc::Request::ServiceControl {
           unit: unit.to_string(),
           action,
       })

   我按 `Request::ServiceControl { unit: unit.to_string(), action }`（单行、
   **且没有 `aether_ipc::` 前缀**）去找，0 命中 —— 差点以为"这里根本不发请求"。

   这是同一个形状的第 N 次：**我以为我知道代码长什么样**。
   而正确做法（本会话已经写过好几次）是：**先读到真实的那几行，再写替换**。

### BL.3 顺带确认的一件事

`intent::try_handle`（离线快速意图通道）**在生产代码里是接上的** ——
我上一轮一度以为它是死代码，是因为我那次 `grep` 加了 `Select-Object -First 10`
把结果截断了（附录 BK.4 记的同一类问题）。这一轮专门确认了 `server.rs` 的调用点。

### BL.4 仍未做

* `aetherd` 自己的中文消息（`main.rs` 里"连不上 aether-init（{e}）—— 服务控制不可用"、
  电源通道那几条等）—— **剩下最大的一块**；
* `aether-ops` 的诊断报告正文；`aether-install` 的命令行用法与错误文案。

### BL.5 验证

* **单次** `cargo test --workspace`：**730 通过 / 0 失败**（输出落盘再统计）；
* Windows 与 **musl**（compositor）双目标 **0 错误 0 警告**；
* **未实机验证**：真机切英文点"重启服务"看回执。

---

## 附录 BM：aetherd 的「模型回答」里的五句 —— 以及**故意不翻**的提示词（2026-10-04 批次 BL-1）

### BM.1 本批做了什么

`aetherd` 的 `agent_run(...)` 收了 `en: bool`（由 `handle_chat` 从 `Chat{en}` 带来），
把**由 aetherd 自己写进回答里**的 5 句双语化：

| 位置 | 文案 |
|---|---|
| 模型返回空内容 | 「（模型未返回有效回答）」 |
| 可信通道拒绝（喂回模型） | 「[工具错误] 「{name}」读写用户环境数据…已拒绝」 |
| 需要确认 | 「「{tool}」需要你确认后才能执行。」 |
| 轮次耗尽 | 「（已达本轮工具调用上限，请拆分任务）」 |

**`SYSTEM_PROMPT` 故意保持中文**，并在代码里加了说明：它是**发给模型**的提示词，
里面明写「回答使用用户的语言」—— 翻成英文反而会让这条指令消失，
用户拿到的回答就不会跟着语言走了。**"要不要翻"取决于"谁读它"**，
这条比"看见中文就翻"重要。

### BM.2 一个 Rust 小规矩（我忘了）

我下意识在**函数参数**上写了 `///` 文档注释：

    trusted_channel: bool,
    /// 界面是否英文（§3.5）…
    en: bool,

编译器直接拒绝：`documentation comments cannot be applied to function parameters`。
改成写进函数自己的文档注释里。

### BM.3 grep 只搜了一个文件（同一类问题的又一次）

我查 `agent_run` 的调用点，只 grep 了 `main.rs`，看到"只有 CLI 自检那一处"。
编译后报 `server.rs:1199` —— **主路径在另一个文件里**。
这是我第 N 次"搜索范围划小了却当成全局"。**让编译器当第二双眼睛**是对的，
但更好的做法是：**grep 时把范围开到你怀疑的全部 crate**。

### BM.4 仍未做（下一批的计划）

`aetherd/server.rs` 里约 20 条 `Response::Error{message}` 拒绝文案（"…必须由已注册的 UI 通道发起"等）。
**计划**：改成**中性英文**。理由：

* 它们是**协议/安全层**的拒绝（同附录 BL 的规矩）；
* **真正的 UI 通道（`is_ui=true`）根本不会碰到它们** —— 收到它们的都是没注册的连接，
  那些客户端不会去渲染中文界面；
* 逐个给请求加 `en` 字段代价大、收益小。

**一处例外要说明**：`RemovableMount` 的**成功**回执（「已只读挂载 {dev} → {mp}」）
是会被显示的；但合成器**自己**已经有一条同义的提示条（BD-1 做的挂载/卸载提示），
所以这条回执改英文不影响用户体验。这一点在改的时候会再核对一次。

### BM.5 验证

* **单次** `cargo test --workspace`：**730 通过 / 0 失败**（输出落盘再统计）；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；
* **未实机验证**：真机切英文让模型返回空内容（或触发一次需确认）看文案。

---

## 附录 BN：拒绝文案改中性英文 —— 以及**为什么有一半我不改**（2026-10-04 批次 BM-1）

### BN.1 本批做了什么

`aetherd/server.rs` 里 33 处**协议/安全层**文案改成中性英文：

* 解析/注册：无法解析的请求 · UI 通道密钥不匹配 · 超长请求行；
* **UI 通道门槛**（22 条）："…必须由已注册的 UI 通道发起"（剪贴板 / 配置重载 / 显示输出 /
  可移动设备 / 更新 / 快照 / 回滚 / 对时 / 时区 / 权限策略 / 系统设置 / 服务控制 / 确认令牌…）；
* 工具错误前缀（`[工具错误]` → `[tool error]`）；
* `handle_chat` 里拿得到 `en` 的两条（"AI 通道不可用…"、"未检测到可用模型…"）→ **正经双语**。

### BN.2 有一半**故意没改**（这是本批最重要的判断）

`server.rs` 里还有约 14 条**会显示的成功/失败回执**：

    已只读挂载 {dev} → {mp} · 已暂存更新 {}（{} 个文件）· 已创建配置快照 {}（{n} 个文件）
    创建快照失败：{e} · 已回滚到配置快照 {id} · 偏移 {offset_ms} ms · {what} · 系统时区已设为 {name}
    {} 已设为「{}」 · 写入失败（设置未生效）：{e} …

它们现在**是中文**，而合成器**确实会把 `message` 显示出来**
（`AiEvent::SnapshotDone { message, .. }` / `UpdateStaged { .. }` / `PolicySaved { .. }`）。
**如果按"协议层 → 英文"一刀切，中文界面里就会冒出英文句子 —— 那是退化。**
一条不能因为"省事"就把现成的中文改成英文。

正解有两条，都属**单独一批**：
1. 给这 8 个请求加可选 `en`（与 `Chat` / `ServiceControl` 同法）；
2. 或让合成器**自己渲染**这些回执（需要把 `dev`/`id`/`n`/`offset` 等结构化字段补进 `Response`）。

我在附录里把选择留给下一批，并写清理由 —— **不做**比**做错**更值得记录。

### BN.3 一个测试断言教我的事

改完超长行的错误文案后，`read_line_capped_boundaries` 失败：它断言
`e.to_string().contains("上限")` —— **断言的是措辞**。
改成断言 **事实**（含上限数字 `16` 且说明这是 cap），
这样措辞再变、行为不变时测试也不会误报。

**教训**：测试断言"行为与数据"，不要断言"措辞"。
（这条与"文档不要写死数字措辞"是同一类。）

### BN.4 又一次内联 Python + 引号（第五次）

我用内联 Python 改那行断言，PowerShell 把引号吃掉，脚本直接报错。
**这是本会话第五次**。规矩写过三遍，仍然在"只改一行"时手滑。
**从这里开始：任何带引号的替换，一律写脚本文件。**

### BN.5 验证

* **单次** `cargo test --workspace`：**730 通过 / 0 失败**；
* Windows 与 **musl**（compositor）双目标 **0 错误 0 警告**；
* **未实机验证**：真机用**未注册**的连接发一次请求（看英文拒绝），
  以及中文界面看那 14 条回执是否仍然自然。

---

## 附录 BO：语言做成**连接级状态**（2026-10-04 批次 BN-1）

### BO.1 一个更好的设计（推翻了我上一轮的计划）

上一轮（附录 BN.4）我计划"给 8~9 个会产生回执的请求**各加**一个 `en` 字段"。
本批动手前重新想了一遍：**语言其实是连接的属性** —— 一个合成器 = 一种语言，
而且它一连上就会 `RegisterUi`。

于是改成：

| 层 | 改动 |
|---|---|
| 协议 | `RegisterUi { key, #[serde(default)] en }` —— **只此一处** |
| aetherd | 连接状态多一个 `ui_en`（与既有的 `is_ui` 并列），注册时记下 |
| 合成器 | 注册时带上 `crate::i18n::is_en()` |
| 回执文案 | `handle_request` 里内联的 **10 条**改成按 `*ui_en` 取词 |

比"给 9 个变体各加字段（还要改 9 处发送点）"小得多，而且**更贴近事实**：
语言跟着"谁在说话"走，而不是跟着"说的是哪句话"走。

### BO.2 为什么上一轮的"一刀切英文"是错的（重复一遍，因为它重要）

那 10 条回执（已只读挂载 … / 已创建配置快照 … / 系统时区已设为 …）
**合成器会原样显示**（`AiEvent::SnapshotDone { message, .. }` 等）。
若按"协议层 → 英文"一刀切，**中文界面里会冒出英文** —— 是退化。
现在它们按连接的语言取词，两边都对。

### BO.3 我又犯了同一个错（第六次）

修改那行测试断言/调用点时，我仍然图快用了**内联 Python**，
PowerShell 把引号吃掉，脚本语法错误。**这是本会话第六次。**
上次（附录 BN.4）我写"从这里开始：任何带引号的替换一律写脚本文件"——
**四轮之内又犯**。

所以这次不写"教训"了，写**做法**：本批收尾的那个脚本里，
**所有带引号的替换都在 `.py` 文件里做**，并且用**正则**匹配而不是手抄带引号的字符串。

### BO.4 仍未做（如实）

* `aetherd` 里由**其它模块**构造的少数回执（例如"已暂存更新…"由 `update` 模块构造、
  对时的几条细节文案）—— 需要把 `en` 继续往下传给那些函数，属下一批；
* `aether-ops` 诊断报告正文；`aether-install` 命令行用法与错误文案。

### BO.5 验证

* **单次** `cargo test --workspace`：通过 / 失败见提交记录（本批新增了连接语言的协议字段，
  旧的 `RegisterUi` 请求靠 `#[serde(default)]` 仍可解析）；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；
* **未实机验证**：真机切英文做一次"创建快照/回滚/对时"看回执语言。

---

## 附录 BP：策略名双语 —— 以及"审计页也会显示这些字段"（2026-10-04 批次 BO-1）

### BP.1 本批做了什么

`ToolPolicy::label(self, en: bool)`（4 个策略名），并把三处调用改成按连接的 `ui_en` 取词：

| 调用点 | 为什么要翻 |
|---|---|
| `SetToolPolicy` 的回执文案（"「{}」已设为「按等级」"） | 合成器会显示这条回执 |
| **审计记录里的 `policy` 字段** | **审计页会把 `args` 显示出来** —— 这条我最初漏了 |
| 同上的日志行 | 与回执保持同一份取值 |

### BP.2 漏掉的那一处是"间接可见"

我一开始只想到"回执文案要翻"，忘了 `policy` 这个名字还被**写进审计记录**
（`serde_json::json!({ "tool": …, "policy": local.label() })`），
而**审计页会把 `args` 原样列出来** —— 于是中文界面之外的界面会看到「按等级」。

**教训**：判断"用户可见"时，不要只看"这个字符串在哪被格式化"，
还要看**它会被存到哪里、那个地方会不会被显示**。
审计/日志类字段是典型的"间接可见"。

### BP.3 测试这次是**加强**而不是放宽

原来的断言是 `ToolPolicy::Ask.label() == "每次询问"`。
改成 `label(false)` 之后顺手加了一组断言：四种策略 × 两种语言、
**英文里不许出现汉字**、中文名与英文名各自互不相同。
（同一件事在附录 BF/AH 里也做过：改签名时把断言补强。）

### BP.4 仍未做（清单已写进台账）

`netset` 的 DNS 失败原因 · `perm` 的 L0–L3 名称 · `policy` 的拒绝理由
（"该工具已被用户明确禁止" —— 它会进**审计的 verdict**）· `llm`/`clipboard`/`modelcfg`
的错误文本与前缀 · `aether-ops` 诊断正文 · `aether-install` 命令行文案。

### BP.5 一次**不是代码问题**的门禁红

跑门禁时 `cargo build --all-targets` 报：

    error: cached cgu eqvvvlu6l3q3745dc85pjobmj should have an object file, but doesn't

这是 **cargo 增量编译缓存损坏**（不是代码错误）：同一个 commit 上 `cargo test` 全绿，
只有 `--all-targets` 的那次构建红。处理办法是清掉该 crate 的产物重编：

    cargo clean -p aetherd      # 只删构建产物，不动源码

之后构建 0 错误 0 警告、门禁 15/15。

**写在这里的理由**：将来若有人看到"门禁红过一次"，能查到它红在**工具链缓存**上，
而不是去翻源码找不存在的 bug。（同类记录见附录 T.2 关于性能门禁与宿主负载的说明。）

### BP.6 验证

* **单次** `cargo test --workspace`：**730 通过 / 0 失败**；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；
* **未实机验证**：真机切英文改一次工具权限，看回执与审计页里的策略名。

---

## 附录 BQ：netset / clipboard 双语 —— 以及一类**不能顺手改**的文案（2026-10-04 批次 BP-1）

### BQ.1 本批做了什么

* `netset::apply(key, value, en)` → `apply_to(…, en)`：DNS 相关 6 条文案双语
  （被拒原因 · 符号链接 · 非普通文件 · 备份失败 · 写入失败 · 成功回执）；
* `clipboard::set(text, en)`：超长拒绝那条；
* 顺带修掉 `netset.rs` 里一句**过期自述** —— 它写着"aetherd 拿不到界面语言，
  所以回一句中文兜底"。有了连接级 `ui_en` 之后这句话不成立，留着会误导后来者
  （与附录 AC 记的"过期自述"同一类）。

### BQ.2 一类**不能顺手翻**的文案：写进审计/日志的那些

清点时发现 `perm::Level` 的中文名（"L2 敏感写"）出现在 `impl fmt::Display` 里，
而 `Display` 被**审计写入**用了（`writeln!(f, "{ts}\t{level}\t{tool}\t{args}\t{verdict}")`）——
这些字是**写日志那一刻**就定死语言的，而审计页之后会把它们显示出来。
`policy` 的拒绝理由（"该工具已被用户明确禁止"）同理：它会进 `verdict`。

这类"存储型"文案不能像界面文案那样"显示时再取词"，**语言必须在写入时确定**。
于是牵连一个设计问题：**审计日志该用哪种语言？**

1. 跟着**当前用户**的语言 → 换语言之后同一份日志里会混着两种语言；
2. 固定**中性英文** → 与"日志给排障的人看"一致，但中文用户读起来别扭；
3. 存**结构化**（`level: 2` / `effect: deny`）+ 显示时取词 → 最干净，但要改审计格式与审计页。

**本批不做**，作为**待决策项**记在这里 —— 顺手选一个会让后面返工。

### BQ.3 另一处已知缺口：工具执行路径

`clipboard::set` 也被**工具执行**调用（`tools::execute` → 剪贴板工具），
那条路径还没有语言参数；本批先固定中文并在代码里注明：
**下一批把 `en` 从 `handle_request` 一路传进 `tools::execute`。**

### BQ.4 验证

* **单次** `cargo test --workspace`：结果见提交记录；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；
* **未实机验证**：真机在英文界面下改一次 DNS（含故意写错），看提示语言。

---

## 附录 BR：「存储型文案」定案 —— 审计日志存 token（2026-10-04 批次 BQ-1）

### BR.1 定案

附录 BQ.2 提出三种选择，本批选**第 3 种**并落地：

> **审计日志里存稳定 token（`L0`…`L3`），人类可读名在显示时按界面语言取词。**

理由：跟用户语言会让同一份日志混两种语言；固定英文会让中文用户别扭；
存结构化最干净 —— 而且**显示层本来就期望 token**（见 BR.2）。

### BR.2 一条决定性证据：合成器的测试早就假设了 token

写代码前我翻到合成器审计页的测试里写着：

    assert_eq!(e.level, "L1");

也就是说：**显示层一直以为日志里存的是 `L1`**，而 `perm.rs` 写进去的是
`Display` 的结果「L1 可逆写」。两边对不上，只是**没人看过真实日志长什么样** ——
UI 直接把那一列原样画出来，所以这个不一致一直没有暴露。

这条证据让选择不再需要犹豫：把写入端改成显示端一直期望的形状即可。

### BR.3 兼容与"只留一处"

* **旧日志**里的值是「L2 敏感写」。显示层对**不认识的值原样透传**，
  所以老日志照样显示、不会变空（有测试断言这一条）；
* 我给 `aetherd` 的 `Level` 加过一个 `label(en)`，随后**删掉了** —— 因为等级名
  一旦有两份（aetherd 一份、显示层一份），改一处就会漏另一处。
  显示层那一份有测试、且要兼容旧格式，**它是该留下的那一份**。
  （本会话里"同一事实多处各写一份"已经出过好几次问题，见附录 Q / X。）

### BR.4 同类还剩一处

`policy` 的**拒绝理由**（"该工具已被用户明确禁止"）会进审计的 `verdict` 列 ——
与 `level` 同类（写入时定死语言）。它比 `level` 麻烦：
`verdict` 里既有**稳定值**（`allowed`/`denied`，本来就是英文）又有**自由文本**。
处理方式要单独想（很可能是"deny 只存原因码 + 参数"），**本批不动**，记为下一项。

### BR.5 验证

* **单次** `cargo test --workspace`：**731 通过 / 0 失败**（+1：`level_text` 的映射与透传测试）；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；`gates.py` 15/15；
* **未实机验证**：真机看审计页的等级列（新日志显示 localised 名，旧日志仍是原文）。

---

## 附录 BS：拒绝理由改码 —— 存储型文案的最后一块（2026-10-04 批次 BR-1）

### BS.1 做了什么

`Verdict::Denied(String)` 里那句话说到底也是**写进审计日志**的（`Gate::audit`），
写进去就定死了语言。改成**码**：

| 场景 | 存进日志的 | 界面显示（中文 / 英文） |
|---|---|---|
| 策略禁止（用户把工具设成"禁止"） | `denied:policy_deny` | 已拒绝：该工具被设为「禁止」/ denied: the tool is set to deny |
| 用户拒绝过（TTL 内不再问） | `denied:user_denied:300` | 已被用户拒绝，300 秒内不再询问 / denied by the user; not asking again for 300s |

两个细节：

* **工具名不再进理由** —— 它在审计的另一列里（`install_disk` 那种重复是多余的）；
* **旧日志原样透传** —— 旧行里是「该工具已被用户明确禁止（install_disk）」，
  显示层不认识就原样显示，不会变空白（有测试）。

### BS.2 顺带修掉一个既有小缺口

审计页以前把 `allowed` / `needs_confirmation` / `failed` 这些 token **原样画在界面上** ——
中文界面里也是英文。现在走 `verdict_text`：显示为「已放行 / 待确认 / 失败」。

这属于"顺手把不一致修掉"，不是本批的目标，但**不修就等于留着一处英文**。

### BS.3 两处测试断言改成断言**码**

`policy_actually_changes_the_verdict` 与 `user_denial_makes_denied_reachable`
原来断言"理由里含'禁止'/'已被用户拒绝'" —— 又是**断言措辞**。
改成断言稳定码（`policy_deny` / `user_denied:` 前缀），措辞再改也不会误报。
（这是附录 BN.3 那条教训的第二次应用。）

### BS.4 仍未做

* `Effect::Ask` 的**原因**（"该工具被设为「每次询问」"）—— 它是**发给界面显示**的
  （确认卡片），不是存盘，所以属"显示型"；要把它翻对，得把 `en` 传进
  `policy::effective`（下一批）；
* 工具执行路径的 clipboard 文案（`tools::execute` 还没有 `en`）；
* `llm` / `modelcfg` 的错误文本；`aether-ops` 诊断正文；`aether-install` 命令行文案。

### BS.5 验证

* **单次** `cargo test --workspace`：**732 通过 / 0 失败**（+1 新测试，并加强了 2 条）；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；`gates.py` 15/15；
* **未实机验证**：真机看审计页的结论列（新日志显示本地化文案，旧日志原样）。

---

## 附录 BT：确认原因改码 + 工具路径拿到语言（2026-10-04 批次 BS-1）

### BT.1 先查证再动手（本批最值的一条）

上一批我写"`Effect::Ask` 的原因要翻，得把 `en` 传进 `policy::effective`"。动手前查了一下
**谁在用这个原因**：

* `perm::judge` 里 `Effect::Ask(_)` 分支 —— **忽略**原因；

也就是说：**它没有任何用户可见路径**。确认卡片显示的是工具自己的 `consequence`
（"会发生什么"），不是策略层的这句话。所以正确做法不是加语言参数，
而是和 `DenyReason` 一样改成**码**（`AskReason`），把它挪出"可能被存/被发"的位置。

**教训**：动手前先问"这条字符串到底会不会被谁看到"。查证花了两次 grep，
省下的是给 `effective` 加参数、改十几个调用点、还要给测试补值的整套工作量。

### BT.2 工具路径的语言：放进 `ToolCtx`，而不是给 `execute` 加参数

`tools::execute` 需要语言（剪贴板超长文案会作为工具结果显示）。
第一版我给 `execute` 加了 `en` 参数 —— 结果要改 `server.rs`、`main.rs` 与 5 处测试调用。

更好的做法：**工具执行体的签名是统一的** `fn(&Value, &mut ToolCtx)`（注册表里那一列 `run`），
给 `execute` 加参数就得改**所有**工具；而 `ctx` 本来就传给每一个工具。
所以 `en` 放进 `ToolCtx`：

    pub struct ToolCtx { pub desktop_actions: Vec<Action>, pub en: bool }

然后只改两处构造点（`server.rs` 的 ToolCall 路径用 `*ui_en`；`main.rs` 的 agent 路径用 `en`）。

**这次差点漏掉的是**：`ToolCtx` 是用 `ToolCtx::default()` 构造的 ——
加了字段之后编译器**不会**提醒"你还没设它"，只会安安静静地用 `false`。
所以加完字段必须**主动去找构造点并赋值**（我专门 grep 了一遍 `ToolCtx` / `ctx = ` 才确认）。

### BT.3 顺手修掉一处我自己写的丑陋代码

上一批我插注释时把 `let bytes = crate::clipboard::set(text, false)` 插成了：

    let bytes = crate:// 缺口（§3.5）：…
        clipboard::set(text, false).map_err(…)?;

它**能编译**（Rust 允许路径跨空白/换行），但注释卡在 `crate::` 与 `clipboard` 之间，
任何人看一眼都会以为坏了。本批把它改成正常的整句 + 注释在上一行。

### BT.4 仍未做

`llm` / `modelcfg` 的错误文本；`aether-ops` 诊断报告正文；`aether-install` 命令行文案。
（合成器侧与 aetherd 的主路径至此都已双语。）

### BT.5 验证

* **单次** `cargo test --workspace`：**732 通过 / 0 失败**；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；`gates.py` 15/15；
* **未实机验证**：真机英文界面下复制一段超长文本（超过上限）看提示语言。

---

## 附录 BU：llm 通道错误双语 —— 以及**两个面明确不做**（2026-10-04 批次 BT-1）

### BU.1 做了什么

`llm::complete(…, en)`：把**我们自己写的前缀**双语化。

| 位置 | 中文 / 英文 |
|---|---|
| 非 2xx（带服务器正文） | LLM 请求失败: … → HTTP …；服务器返回: … / LLM request failed: … |
| 连接层错误 | LLM 请求失败: {url} / LLM request failed: {url} |
| 读响应失败 | 读取 LLM 响应失败: {url} / reading the LLM response failed |
| 非 JSON（可能被网关拦） | LLM 响应不是 JSON（可能被网关拦截）: … / LLM response is not JSON … |
| 缺字段 | LLM 响应缺少 choices[0].message / LLM response is missing … |
| 正文截断 | …（正文已截断）/ … (body truncated) |

**技术载荷保持原样**（url / HTTP 码 / 服务器正文）—— 那是**数据**，
而且排障的人正是靠它定位；把它们"翻译"没有任何意义。

### BU.2 两个面**明确不做**（这是决定，不是遗漏）

| 面 | 读者 | 决定 |
|---|---|---|
| `aether-ops`（巡检守护，输出进串口日志） | 排障者 | **保持单一语言** |
| `aether-install`（安装命令行，`--disk … --yes`） | 操作者 | **保持单一语言** |

理由：它们的读者是**操作者/排障者**，与 daemon 的串口日志同一类。
给它们加语言开关，代价是"同一份日志里可能混两种语言"（换过语言的机器上），
而收益只是"英文操作者读着顺一点" —— 按本会话反复用过的那条规矩
（**会被用户看到 → 跟界面语言；日志/协议层 → 单一语言**），它们属于后者。

**写下来的意义**：将来有人扫到"这两个 crate 里还有中文"，
能查到这是**有意的**，而不是漏掉的（与附录 AK 更正 §3.5 范围时同一件事的两面）。

### BU.3 仍未做

`modelcfg` 的 `(未配置)` / `(默认 http://…)` 等值 —— 它们是**写进配置文件**的
（`to_json()` → 落到磁盘），属"存储型"；按 BQ-1 定案的精神应改成**中性/稳定**的形态
（或由显示层渲染），下一批做。

### BU.4 我这一批踩的两个坑（都是重复的）

1. **函数参数上写 `///`** —— 第 73 轮（附录 BM.2）刚记过这条 Rust 规矩，这次又犯，
   编译器再次拒绝：`documentation comments cannot be applied to function parameters`；
2. **用正则插参数插到了内层括号里** —— `Some(&tools_json(, en))`，
   之后我又手动修多了一个右括号，构建红了两次。

这两件事的共同点：**我在"改一行"时跳过了"先读出真实的那一行"**。
本会话已经把这条写进附录 AG/BL/BM，这批是第三次。

### BU.5 验证

* **单次** `cargo test --workspace`：结果见提交记录；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；`gates.py` 15/15；
* **未实机验证**：真机在英文界面下制造一次 LLM 通道错误（例如把 API Key 改错）看提示文案。

---

## 附录 BV：§3.5 收口 —— 一次**上界扫描**与它的正确读法（2026-10-04 批次 BU-1）

### BV.1 本批做了什么

* `modelcfg` 的 JSON 状态值中性化（`(未配置)`→`(not set)`，`(默认 …)`→`(default …)`）——
  那是**机器可读**的输出（脚本/工具会解析），里面不该夹中文人话；
* 跑了一次**全库 i18n 终扫**，明细落盘到 `docs/报告/i18n终扫（I18N-FINAL-SCAN）-2026-10-04.txt`。

### BV.2 终扫给的是**上界**，不是"残留数"

| crate | 终扫计数 | 读法 |
|---|---|---|
| aether-compositor | 285 | **上界** |
| aetherd | 233 | **上界** |
| aether-init | 51 | 上界（多为救援控制台/logtee 的串口文本） |
| aether-install | 40 | **有意保持单一语言**（命令行） |
| aether-ops | 26 | **有意保持单一语言**（巡检守护） |
| aether-ipc | 1 | `expect(...)` 的 panic 消息（开发者面） |
| aether-shell | 0 | ✓ |

为什么是上界：扫描器按"这一行的 8 行上下文里有没有 `pick(`/`is_en()`"判断是否已翻译，
而**已翻译行的另一语言分支**（`if en { "…" } else { "…" }` 的 else 支、
`(State::X, false) => "中文"`）本身**就是**中文且上下文里没有 `pick(`。
本会话已经四次踩到"行扫描分不出这类"，所以这次**不拿它当结论**。

### BV.3 抽样人工核对（这次的证据）

从每个 crate 的明细里各抽若干条**逐条读**，结论：**没有一条是"漏翻的用户可见文案"**。
抽到的样本全部落在下面几类：

1. **已翻译行的另一语言分支**（`(AudioState::Ready, false) => "就绪"`；
   `if en { "service not found" } else { "服务不存在" }`；`bail_unit(unit, en)` 的 else 支）；
2. **串口/救援日志**（`rescue_console("PID 1 启动失败：{e:#}")`、`logtee` 的拒绝原因）；
3. **有意单一语言的 CLI crate**（`aether-install` / `aether-ops`）；
4. **`expect(...)` / `debug_assert!` 的 panic 消息**（开发者面，不是界面文案）；
5. **页面名 key**（`match title { "外观" => … }` 的匹配键，不是显示文本）。

### BV.4 `§3.5` 的完成声明与边界

**已完成**：合成器全部设置页与窗口类界面、提示条、音频/预览/安装器/确认卡片；
aetherd 的 AI 回复、协议回执、拒绝文案、策略名、DNS、剪贴板、LLM 通道错误；
aether-init 的服务回执；审计两列的结构化存储 + 显示层取词（含旧日志兼容）。

**有意保留中文/单一语言**（不是遗漏）：
`aether-ops` 与 `aether-install` 的 CLI 输出 · `aether-init` 的救援控制台与 `logtee` ·
串口诊断 `tty_log` · `intent.rs` 的**中文关键词表**（AI 听懂中文靠它，翻了就废）·
`SYSTEM_PROMPT`（发给模型的提示词，里面要求"用用户的语言回答"）。

**未验证**：真机切换语言逐页核对 —— 本环境没有 evdev/fbdev，**只能做代码级验证**。

### BV.5 验证

* **单次** `cargo test --workspace`：结果见提交记录；
* Windows 与 **musl** 双目标 **0 错误 0 警告**；`gates.py` 15/15；
* 终扫明细作为**可复查的产物**提交到 `docs/报告/`（不是口头结论）。

---

## 附录 BW：§4.1 注入脚本的**第一半** —— 以及一次"写了脚本没运行"（2026-10-04 批次 BV-1）

### BW.1 本批做了什么

台账 §4.1 写的是"动作类无界面 e2e 可跑；**键鼠注入未做**"。本批落地注入的**格式层**：

* 新模块 `aether-compositor/src/e2e.rs`（`#[cfg(test)]` 接入，零警告）：
  `parse` / `parse_line` —— 纯函数，支持 `nav:*`（9 个导航键，含 `pgup`/`del` 别名）、
  `key:<单字符>`、`wait:<帧数>`，`#` 注释与空行忽略，**报错带行号**；
* 测试：正常解析（含注释/空行）、未知步骤报错带行号、`key:ab` 必须报错而不是默默取第一个、
  `wait:soon` 报错。

### BW.2 为什么**只做一半**（不交付半成品）

完整的"注入"还差两件，它们要一起才成立：

1. **执行器**：本想让 `e2e` 直接调 `dispatch_nav` —— 但它是 `main.rs` 的**私有函数**，
   跨模块要提升可见性（或抽一个更薄的缝）；
2. **帧级点击**：`Renderer::render_frame` 会把可点区域填进 `control_hits`，
   所以"在**内存缓冲**上画一帧 → 按真实坐标点击 → 断言状态"**不需要显示设备**就能做
   —— 但要接进主循环的 `--e2e <脚本>` 模式。

两件事我都没在本批硬塞进去：**脚本格式本身可以独立验证**，先把它做扎实；
执行器与点击一起做，才有"点得到、断得准"的价值。这条判断写进台账 §4.1 的备注里。

### BW.3 一次低级的疏漏（值得记）

本批我先写了创建 `e2e.rs` 的脚本，**却没运行它**，直接去做下一件事 ——
于是编译报 `file not found for module 'e2e'`（因为注册语句已经加上去了）。

教训很小但很典型：**"写了脚本"不等于"跑了脚本"**。
与前面几次（内联 Python 静默失败、grep 被截断、测试输出被截断）是同一族问题：
**我以为某件事发生了，但没有证据**。这次的证据来自**编译器**——它立刻指出了不一致。

### BW.4 验证

* `cargo test -p aether-compositor e2e`：相关 5 项通过（含本批 2 项）；
* **单次** `cargo test --workspace`：见提交记录；Windows 与 **musl** 双目标 **0 错误 0 警告**；
* `gates.py` 15/15（提交前先跑，**绿灯才提交**）。

---

## 附录 BY：§4.1 收口 —— 坐标点击跑通，且**用真实二进制调试**（2026-10-04 批次 BX-1）

### BY.1 本批做了什么

`--e2e` 的 JSON 脚本新增 **`click:x,y`**，并补上让脚本/测试**拿得到真实坐标**的查询：

| 动作 | 说明 |
|---|---|
| `{"action":"click","args":{"x":…,"y":…}}` | 画一帧 → 按坐标找命中控件 → `control_center_click` → 产生的 `UiIntent` 交给 `apply_ui_intent` |
| `{"action":"query","args":{"what":"status_cluster"}}` | 顶栏状态簇的**真实**矩形（x,y,w,h） |
| `{"action":"query","args":{"what":"hits"}}` | 当前**全部**可点控件与坐标（含控件名） |
| `{"action":"query","args":{"what":"dark"}}` / `"control_open"` | 点击效果的断言点 |

关键仍然是**不另写一套**：注入调用的是主循环在用的 `control_center_click` 与 `apply_ui_intent`；
命中表由 `Renderer::render_frame` 在**内存缓冲**上画帧时登记，所以整条链路不需要显示设备。

### BY.2 两次"查询读到上一帧"——是**跑真实二进制**发现的

我先按常识写了 `query: hits`，测试报"展开后应有可点控件，实得（空）"。两次都栽在同一个坑：

* 命中区是**画帧时**登记的；
* 而"点状态簇 → 控制中心展开"这个状态变化发生在**渲染之后**；
* 于是紧接着的查询读到的是**上一帧**的表（空）。

处理：把"画一帧刷新命中表"抽成 `inj_render_once`，`click` 与两个查询都先调它。
**定位它的办法不是猜**：直接

    printf '%s' '{"action":"query","args":{"what":"status_cluster"}}' \
      | cargo run --bin aether-compositor -- --e2e

真实二进制立刻回 `0,0,0,0`（而不是我以为的坐标）—— 于是原因一目了然。

**这条值得单独记**：`--e2e` 之所以有价值，正是因为它**不需要显示设备就能跑真实二进制**；
本批我用它当场定位了两个问题，而不是靠读代码推断。

### BY.3 一条不写死坐标的测试

`e2e_click_reaches_real_control_center` 分三段跑（布局确定，坐标可跨段复用）：

1. 问 `status_cluster` → 算出中心 → 点它 → 断言 `control_open` 为真；
2. 再问 `hits` → 从**真实命中表**里找出 `ToggleDark` → 算中心；
3. 点它 → 断言 `dark` 状态**翻转**。

也就是说：**"点得到"这件事有据可依** —— 坐标来自被测程序自己报出来的表，
而不是我在测试里写死的数字。

### BY.4 验证

* `cargo test -p aether-compositor e2e_click`：通过；
* **单次** `cargo test --workspace`：见提交记录；Windows 与 **musl** 双目标 **0 错误 0 警告**；
* `gates.py` 15/15（**绿灯才提交**）。

---

## 附录 BZ：账户后端（§2.3）—— 三条安全约定与两个"测试抓到的真 bug"（2026-10-04 批次 BY-1）

### BZ.1 范围：为什么**不做增删用户**

台账里项目自己写着「**先做单用户 + 密码；多用户留 M3**」。
所以本批做的是账户的**看**与**改口令**，增删用户**不越界** —— 那是 M3 的立项范围。
把这条写在这里，是为了让后来的人看到"账户页里没有增删"时知道这是**决定**，不是遗漏。

### BZ.2 三条安全约定

1. **口令只走 stdin**。`chpasswd` 的输入是我们直接写进它的标准输入的：
   既不进 argv（`ps` 里对同机用户可见），也不经过 shell（没有注入面）。
   代码里还额外拒绝**含换行/含冒号**的口令 —— 因为 `chpasswd` 的输入格式是
   "每行一个 `用户:口令`"，换行会把一行拆成两行、冒号会被当成分隔符。
2. **口令不进审计**。写审计时只有 `{"user": …, "action": "set_password"}`，
   连字段都没有；而 `tools::audit_args` 本身是**白名单制、默认脱敏**，是第二道保险。
   日志同理：`[aetherd] 口令已更新：alice`，只有用户名。
3. **旧口令的校验不在这里**。合成器有 `sha512crypt`（锁屏在用），aetherd 没有。
   分工是"合成器先证明你是这个人，再让 aetherd 写新口令"。
   **信任模型写清楚**：合成器是**已注册的 UI 通道**（对端 uid 校验 + 密钥），
   而这条请求同样过 UI 门槛 —— 未注册的连接连改口令的入口都摸不到。

### BZ.3 策略错误用"码"，不用句子

`PasswordProblem { Empty, TooShort, TooLong, ContainsNewline, ContainsColon }` +
`code()`；措辞在 `server.rs` 的 `password_problem_text(en, p)` 里按界面语言渲染。
与 `DenyReason` / `PreviewError` / `DnsError` 同一套 —— 这一层不该决定用户读到的句子。

### BZ.4 两个"测试当场抓到的真 bug"（本批最有价值的部分）

1. **`nobody` 被当成人类账户**。我第一版的过滤条件是 `uid >= 1000`，
   而 `nobody` 的约定 uid 是 **65534** —— 它会被列进"账户"页，让人以为可以改它的口令。
   我自己写的样例里恰好有 `nobody` 那一行，于是测试第一次运行就红了。
   修法：`(1000..65534)` 并用 `NOBODY_UID` 常量把"为什么是 65534"写在代码里。
2. **漏登记只读入口**。既有的 `every_request_handler_is_gated_and_classified`
   是"访问控制表"测试：它要求每个 `Request::X` 分支要么**改状态且落审计**，
   要么**显式登记进 `READ_ONLY` 并写明理由**。我加了 `ListUsers` 却忘了登记，
   它直接报"访问控制表出现缺口"，并给出该走哪条路。

**这两个都不是我"看代码看出来的"** —— 是测试逼出来的。它们的价值恰好在于：
一个挡的是"功能错了"（列出 nobody），一个挡的是"安全分类漏了"（只读入口没归类）。

### BZ.5 验证

* `cargo test -p aetherd`：**220 通过 / 0 失败**（含 3 条新测试：账户解析与格式容错、
  口令策略的四种危险形状、用户名白名单）；
* **单次** `cargo test --workspace`：见提交记录；Windows 与 **musl** 双目标 **0 错误 0 警告**；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机上真的改一次口令并用新口令解锁（本环境没有目标系统）。

---

## 附录 CA：口令策略**只有一份** —— 以及一次主动回退（2026-10-04 批次 BZ-1）

### CA.1 本批做了什么

把口令策略从 `aetherd` 移到 **`aether-ipc`**：`MIN/MAX_PASSWORD_CHARS`、
`PasswordProblem` + `code()`、`check_new_password`、`is_valid_user_name`。
`aetherd::accounts` 改成 **re-export**（调用点不动，但"规则从哪来"一目了然）。

### CA.2 为什么必须移（这是我在补自己的一个错）

上一批（附录 BZ）我把策略放在 `aetherd`。这一批要做界面时才发现：
**界面也要用它** —— 而项目自己的先例就摆在那里：

> `DnsField::validate` 的注释：「界面**不重复实现**校验规则：两侧各写一份必然分叉，
> 而分叉的后果是'界面说可以、服务端拒绝'或更糟的反过来。」
> 而 `parse_dns_list` 就住在 `aether-ipc`。

所以照做。**如果我没移，界面上就会出现第二份"口令至少 8 位"** —— 两份迟早分叉。

### CA.3 一次**主动回退**：界面本批没做完，所以撤掉了半成品

本批原本要一并做设置页「账户」。做完"模型"部分（页面定义 + `PassField`/`PassForm` +
`check_form` + 三条测试，全部编译通过）之后我停住了，理由：

* 这个页面**必须**同时具备：`draw.rs` 的页面正文（否则 `_ => {}` 兜底会让它是个**空页**）、
  `PageCtx` 新字段（**7 处构造点**都要改）、`main.rs` 的编辑态与 IPC 接线。
  这三件缺一不可 —— 缺任何一件，页面上要么空白、要么输入框打不进字；
* 而剩下的预算**不足以**把它们做完并验证。

于是我把 `settings.rs` **整体回退**（`git checkout --`），只保留已经完整、已经绿灯的策略迁移。
**理由**：一个"列在设置里、点进去是空页"的功能，比"暂时没有这一页"更糟 ——
前者会被当成坏了。宁可少交，不交半成品。

**这条与附录 BW.2（"只做一半"）的区别**：那次我交付的是**可独立验证**的一半（脚本格式），
而这次那"一半"单独存在时**在界面上是坏的**，所以不能留。

### CA.4 验证

* `cargo build --workspace --all-targets`：**0 错误 0 警告**；
* **单次** `cargo test --workspace`：见提交记录；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机改口令 + 新口令解锁。

---

## 附录 CB：账户界面接通（§2.3）—— 以及"门禁的警告检查比看起来弱"（2026-10-04 批次 BZ-2）

### CB.1 本批做了什么

设置页「账户」真正可用：

* **只读列表**：`ListUsers` → 名字 / uid / shell；
* **改口令**：三个**遮显**输入框（旧 / 新 / 确认）+ 提交按钮；
* **旧口令在合成器侧校验**：读 `/etc/shadow` → `auth::parse_shadow` → `auth::decide`
  （**与锁屏同一套**，没有第二份口令校验逻辑）；
* 新口令交给 aetherd（`SetPassword`），走 `chpasswd` 的 stdin。

几处刻意的取舍：

| 取舍 | 理由 |
|---|---|
| 口令框**从空白开始** | 旧口令的"当前值"在 shadow 里是**哈希**，拿不到明文 —— 事实决定的 |
| 上屏一律**圆点**，按**字符数** | 按字节算会把一个汉字显示成三个圆点，等于泄漏长度 |
| 回车 = "下一个框"，最后一个框才提交 | `UiEvent` 里没有 Tab 变体；这样不用加新事件也能填完 |
| 提交成功后**清空三个框** | 口令不该在界面里多留一秒；失败则保留，让用户改 |
| 目标账户 = 列表里第一个 | 单用户模型（项目决定）；多用户是 M3 |

### CB.2 一个真实发现：**门禁的"0 警告"比看起来弱**

我把本批改完之后 `cargo build` 一直是 0 警告。直到一次 `cargo clean -p aether-compositor`
之后，才冒出两个**既有**警告：

* `AiEvent::ToolDone.origin` 只在 Linux 的 fbdev 循环里被读；
* `AiEvent::DnsSaved` 只在 `set_dns` 里被构造，而它依赖 Linux 专属的 `admin_request`。

也就是说：**Windows 上全量重建时，这两个东西本来就是 dead code**。
之所以一直没看见，是因为 cargo **只对重新编译的 crate 重新报告警告** ——
增量编译把旧警告"藏"住了。

**这条对"门禁"的意义**：`gates.py` 里的构建步骤在增量状态下通过，
并不等于"全量重建也没有警告"。**结论：验证构建时应当周期性 `cargo clean` 一次**，
否则会长期看不见既有警告（本批就是这么发现的）。

处理：如实标注（`#[cfg_attr(not(target_os = "linux"), allow(dead_code))]` + 写明理由），
**没有删功能** —— 这些在目标平台（Linux）上是要用的。

### CB.3 架构上踩的一次坑

我一开始按"通用分发器"的思路，把两个新响应塞进 `fetch_removables` 的 match 里 ——
但这个项目的架构是**每个请求各自一个线程 + 局部 match**（`admin_request` 同步取回执，
再 `tx.send(ev)` 送回主循环）。看懂了之后改成同一形状，才与既有的 `fetch_removables`
/ `toggle_removable` 一致。

教训与附录 AD/AE 同族：**先读懂现有形状，再动手**；我这次是"先按想象写，被编译器纠正"。

### CB.4 验证

* `cargo clean -p aether-compositor` 后**全量重建**：0 错误 0 警告；
* **单次** `cargo test --workspace`：见提交记录；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机改口令 + 用新口令解锁。

---

## 附录 CC：收尾后的**整体审计**（第 4 遍）—— 本会话新增的 §2.3 账户与 §4.1 e2e（2026-10-04）

规程要求"所有任务完成后整体审计 3 遍"（附录 T / U / V 已完成）。
但 §2.3 账户与 §4.1 e2e 是**在那三遍之后**才做的，所以收尾时补这一遍，
范围就是**本会话新增的代码**。

### CC.1 取证结果（每条都给了命令与实测输出）

| # | 检查项 | 方法 | 结果 |
|---|---|---|---|
| 1 | 口令**不进审计/日志** | 在 `accounts.rs`/`server.rs` 里搜 `password` 与 `audit` 的共现 | **无**。写审计时参数只有 `{"user":…,"action":"set_password"}`，连口令字段都没有；`audit_args` 又是白名单制、默认脱敏（第二道保险） |
| 2 | 口令**不进 argv** | 看 `Command::new("chpasswd")` 的全部参数 | **argv 里只有 `chpasswd`**；口令经 `writeln!(stdin, "{user}:{new_password}")` 写入标准输入。不经 shell，无注入面 |
| 3 | 两个新请求**都有 UI 门槛** | 看 `Request::ListUsers` / `Request::SetPassword` 分支首行 | **两处都有** `if !*is_ui { return 403 }`；且既有的永久不变量测试 `every_request_handler_is_gated_and_classified` 会把漏登记直接判红 |
| 4 | 口令策略**只有一份** | 全库搜 `pub fn check_new_password` | **恰好 1 处**（`aether-ipc/src/lib.rs`）；`aetherd` 是 re-export，合成器直接用 —— 界面与服务端不会分叉 |
| 5 | 用户名**不被当成任意文本** | 看 `is_valid_user_name` 与调用点 | 白名单（小写字母/数字/`_`/`-`，首字符限字母或 `_`），且 `set_password` 在 spawn 之前先校验 |
| 6 | 命令拼接 / `sh -c` | 搜 `sh -c`、`Command::new("sh")` | **0 处**（与附录 T 的结论一致） |

### CC.2 发现的问题（1 处，**文档准确性**）

报告 §4.3 原本写「`aetherd` 与 `aether-ipc` 的生产代码里 **0 处 `println!`**」。
实测：`aetherd/src/main.rs` 里有 **21 处** `println!` —— 但都在**命令行子命令**的输出路径上
（`aetherd app list`、`--help` 之类），那是**给人看的 CLI 输出**，本就该走 stdout。

**处置**：把该结论**限定**为"**守护进程运行路径**（`server.rs`/`tools.rs`/`accounts.rs` 等）0 处 `println!`"，
并在收尾报告里写明这 21 处的性质。

这条本身不是安全漏洞，但属于**报告不准确** —— 而"文档不得夸大"是本项目的硬要求，
所以照样记一条并改正。**教训与附录 BV 同族：结论要写清"对谁而言"。**

### CC.3 结论

**本会话新增的代码没有发现新的安全漏洞。** 四遍整体审计（T / U / V / CC）合计，
产出的是：一条会红的门禁（访问控制表不变量）、两次方法论更正、一条流程更正
（增量构建的 0 告警 ≠ 全量 0 告警，附录 CB.2），以及这一条文档准确性更正。

---

## 附录 CD：整体缩放第一步 —— 布局尺寸表（§2.4），以及"截图比对不可用"（2026-10-04 批次 CD-1）

### CD.1 本批做了什么

`theme::ui` 一直**只缩放字号**（它自己的注释写着："本批只缩放字号，不缩放布局尺寸……
真要做到'整体缩放'，得先有一份集中的尺寸表 —— 那是独立批次"）。本批就做这份表：

* 新增 **`theme::size`**：8 个命名尺寸（`CONTENT_PAD` 24 / `CONTROL_INSET` 18 / `ROW_H` 40 /
  `ROW_H_SM` 36 / `GROUP_GAP` 22 / `LINE_GAP` 24 / `HINT_GAP` 26 / `TIGHT` 12）
  + 纯函数 `scale(n, pct)` 与 `px(n)`；
* 把 **239 处**尺寸接入（`x + 24` 85 处、`x + 18` 45 处、`h: 40` 27 处、`ry += 24` 29 处……）；
* `ui::set_scale_pct` 同时调 `size::set_pct` —— **两处共用同一份缩放输入**（各读一遍设置必然漂移）。

### CD.2 为什么是"挑出来的"而不是把 600 多个字面量机械替换

实测 `draw.rs` 生产区里：`ry += N` 119 处（21 个不同值）、`h: N` 160 处（36 个）、
`x + N` 246 处（37 个）、`y + N` 96 处（42 个）。

但其中**只有一类该跟着缩放**：

* **尺寸**（行高/间距/内容边距）→ 走 `px`；
* **像素级细节**（发丝分隔线 `h: 1`、阴影偏移 `x + 1`、文本基线微调）→ **不缩放**。
  1 像素的分隔线在 125% 下还应该是 1 像素，否则会糊成 1.25 像素的边。

所以表是**挑**的，而且**只挑了命名得清楚的那几个**；其余等用到时逐个判断。

### CD.3 一次方法上的更正：**`--shot` 的输出不可复现**，所以"逐像素比对"不成立

我原本计划用"重构前后 10 张 `--shot` 的 SHA256 必须相同"来证明行为不变。
**实测不成立**：

* 固定 `AETHER_FAKE_UTC=1`、同一个构建、连跑两遍 —— **仍有 3 档不同**
  （`layout2`/`layout4`/`menu`；换成只取"看起来稳定"的 7 档后，
  `controlcenter`/`installer`/`confirm` 又出现差异）；
* 原因方向：这些界面会去**探真实运行时状态**（音频探测、AI 状态等），
  而 `--shot` 里并没有把它们固定住。

**这条对项目的意义**：`--shot` 适合"人眼看走查图"，**不适合**当自动化的像素回归判据 ——
要它承担后者，得先给它一个"确定模式"（把所有运行时探针固定成夹具值）。
如实记在这里，**不假装截图比对通过**。

### CD.4 换成的判据（更硬）

1. **单测**：`scale(n, 100) == n`（覆盖正负与常用尺寸）+ 取整规则 + 全局状态跟随；
2. **逐字剥离**：把当前 `draw.rs` 里新增的尺寸表与两行同步代码**挖掉**，
   两边都**剥掉包装**（`theme::size::px(theme::size::CONTENT_PAD)` → `24`），
   再**逐字比较** —— 结果：**6899 行逐字相同** ✓。

第 2 条证明的是"**除了新增的表与那两行，文件其余部分一字未变**"；
配合第 1 条（100% 下换算恒等），语义不变就有了**可复现的证据**，
而不是"看着差不多"。

### CD.5 验证

* `cargo build --workspace --all-targets`：0 错误 0 警告；
* `cargo test --workspace`：见提交记录（新增 3 条 `theme::size` 测试）；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机上 125% 缩放的观感（本环境无显示设备）。

---

## 附录 CE：整体缩放第二步 —— 间距阶梯与不变量（§2.4）（2026-10-04 批次 CE-1）

### CE.1 本批做了什么

在上一批（附录 CD）的尺寸表基础上，把**间距阶梯**也纳入：

| 新常量 | 值 | 含义 | 接入处数 |
|---|---|---|---|
| `GAP_TIGHT` | 8 | 小节内的紧凑间距 | 6 |
| `GAP_STD` | 20 | 控件之间的标准间距 | 4 |
| `GAP_WIDE` | 44 | 大块内容之间的间距 | 6 |
| `GAP_HUGE` | 58 | 页面分段之间的间距 | 4 |
| `ROW_TALL` | 46 | 一行"控件 + 说明"占的高度 | 14 |
| `INSET_SM` | 16 | 次级左缩进 | 13 |

**累计接入 286 处**（上一批 239 + 本批 47）。

命名按**用在哪**而不是**多大** —— 这样以后加页面时能对着位置挑，
而不是"上次那个 44 是用在哪来着"。

**明确不动**的：`h: N` 里剩下的 **44 处 ≤3**（发丝分隔线 `h: 1`、`h: 0` 占位）
以及 `x + 1`/`y + 1`（阴影偏移）—— 1 像素的分隔线在 125% 下还应该是 1 像素。

### CE.2 验证（沿用上一批那条更硬的判据）

把**两侧**的尺寸表模块与两行同步代码都挖掉、剥离包装后逐字比较：
**6936 行逐字相同** ✓ —— 本批改动同样只是"加了一层包装"，100% 下语义不变。

（上一版脚本只挖了"当前"一侧，因为当时尺寸表还不在 HEAD 里；
本批它已经在 HEAD 里了，所以两侧都要挖。**这也说明这条判据是可复用的**，
不是一次性的。）

### CE.3 新增一条**不变量测试**

`every_size_constant_is_actually_used`：用 `include_str!("draw.rs")` 读自己的源码，
断言表里**每个常量至少有一个调用点**（比对时带上右括号，
否则 `ROW_H` 会被 `ROW_H_SM` 的调用点误算成"用过"）。

理由：这张表的意义就是"被页面代码引用"；加了常量却没人用，它就在变成装饰 ——
与 `ui_scale_pct` 曾经"存了不用"同族。手法与 `aetherd` 的
`every_request_handler_is_gated_and_classified` 一致：**把结论变成不变量**。

### CE.4 诚实的边界：这一步**没有做完**

把**剩下**的字面量也纳入需要**逐处判断"该不该跟着缩放"**：

* 判据不是"数值大小"，而是**这一处是什么** —— `y + 13` 可能是文本基线微调（不该缩），
  也可能是控件内边距（该缩）。**只有看屏幕能确认**；
* 而本环境**没有显示设备**，且 `--shot` 已被证明**不可复现**（附录 CD.3），
  所以"改完对不对"我给不出证据。

**所以本批停在这里**：地基（表 + 286 处接入 + 不变量）是**可验证的**，交出去；
剩下的是"需要一个能看到屏幕的环境"才能负责地做的部分。

**若要在有屏幕的环境里接着做，起点是**：
① 先给 `--shot` 加一个"确定模式"（把所有运行时探针换成夹具值），
   让它能承担像素回归判据；② 逐页把剩余字面量按"是不是尺寸"分类；
③ 每改一批就用 100%/125% 两档截图对比 + 人眼确认；④ 补上本批这条不变量。

### CE.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录（新增 1 条不变量测试）；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机上 125%/150% 缩放的观感。

---

## 附录 CF：让 `--shot` 可复现 —— 一个**真 bug**：渲染不是输入的纯函数（2026-10-04 批次 CF-1）

### CF.1 起因

附录 CD.3 记着一条"想做却做不成"：`--shot` 的输出**不可复现**，
所以它当不了"像素回归"的判据 —— 而这正是整体缩放收尾的前置条件。

### CF.2 怎么定位的（不靠读代码猜）

先做了两件事：

1. **固定目录夹具**（`AETHER_SHOT_FIXTURE=1`，与既有 `AETHER_FAKE_UTC` 同风格）：
   走查用的演示桌面不再去读 `$HOME` 的真实内容 —— 从 10 档里 3 档不稳定降到 2 档；
2. **逐字节二分**：把两遍的 BMP 存下来做 diff，直接算出差异落在**图像行 136–155 / 列 520–521**
   —— 一条 **2×20 像素的竖条**。

有了坐标，`draw.rs` 里对号入座：`draw_ai_bar` 的光标正是
`fill_rect(..., Rect { x: caret_x, y: bar.y + 16, w: 2, h: 20 }, ...)` —— **2×20** ✓。

### CF.3 根因：同一个函数里用了**两个**时钟

```rust
// 第 5693 行：焦点环脉动 —— 用的是**传入的** t
0.45 + 0.25 * (t * 3.0).sin()
...
// 第 5748 行：光标闪烁 —— 用的是**真实时钟**
} else if ((t_now() * 2.0) as i32) % 2 == 0 {
```

`t_now()` 返回的是"当前秒内的小数部分"（`subsec_millis()/1000.0`），
于是**同样的输入渲染出不同的画面** —— **渲染不是输入的纯函数**。

这不是"截图工具的小毛病"，是一个**真 bug**：它让"同一状态 → 同一画面"这个基本性质不成立，
走查图、像素回归、以及将来任何"渲染快照"都不可信。

### CF.4 修法

与同函数其它地方一致，用**传入的 `t`**：

```rust
} else if ((t * 2.0) as i32) % 2 == 0 {
```

顺带删掉 `t_now()` —— 它唯一的调用点就是这个 bug 的来源，删掉后构建 0 告警。

### CF.5 验证

* **10 档 `--shot` 连跑两遍，逐字节完全相同** ✓（修复前是 8/10）；
* **对照实验**：关掉目录夹具再跑两遍 —— **仍然 10/10 相同**。
  也就是说**目录夹具并不是可复现性的必要条件**，光标那处修复才是。
  夹具保留下来另有价值：走查图因此**不依赖跑图那台机器的家目录**，跨机器稳定
  （这一点如实写在这里，免得以后有人以为"没有夹具就不可复现"）。

### CF.6 这一条解锁了什么

整体缩放的**收尾**现在有了判据：改完一批尺寸，跑 100%/125% 两档 `--shot` 做**逐字节对比**，
至少能保证"没有意外改动"，剩下的观感判断仍需人眼（在有屏幕的环境里）。

### CF.7 验证命令（可复现）

```bash
AETHER_FAKE_UTC=1 AETHER_SHOT_FIXTURE=1 \
  # 对同一组 --shot 模式连跑两遍，比较 preview.bmp 的 SHA256
```

---

## 附录 CG：整体缩放第三批 —— 控件高度与内边距，**逐像素**验证（2026-10-04 批次 CG-1）

### CG.1 本批做了什么

抽样核对（不是按数值大小猜）之后，把**控件高度阶梯**与**内边距**接入尺寸表：

| 新常量 | 值 | 含义 | 处数 |
|---|---|---|---|
| `CTRL_H` | 44 | 按钮 / 输入框的标准高度 | 9 |
| `CTRL_H_SM` | 30 | 图标框与列表行高度 | 6 |
| `CTRL_H_XS` | 26 | 面板与次级图标高度 | 4 |
| `PAD_SM` | 8 | 面板内容与边框之间的内边距 | 6 |
| `PAD_LG` | 20 | 较大的内边距（卡片/浮层） | 5 |
| `INDENT_SUB` | 36 | 子项相对父项的缩进 | 3 |

`y + 12` 与表里已有的 `TIGHT`(12) 同值 → **复用**，不新增常量（4 处）。

**累计接入 323 处**（239 + 47 + 37）。

### CG.2 验证方式升级：从"逐字剥离"到**逐像素比对**

附录 CF 把 `--shot` 修成可复现之后，这一批就能用**最直接**的判据：

```
改动前：10 档 --shot 的 SHA256  →  存盘
改动后：同样 10 档            →  比对
结果：**10 档全部逐字节相同** ✓
```

这比前两批的"逐字剥离"（证明"改动只是加了一层包装"）更硬 —— 它直接证明
**屏幕上的像素一个都没变**。前两批的判据仍保留在附录 CD.4 / CE.2，作为
"还没有可复现截图时"的替代手段。

### CG.3 把「故意不缩」的清单写进代码

尺寸表的注释里现在有一段**故意不缩**的清单，每条带理由：

* `h: 1` / `h: 0`（发丝分隔线、占位）与 `x + 1` / `y + 1`（阴影偏移）——
  1 像素的线在 125% 下还应该是 1 像素，否则会糊成 1.25 像素的边；
* `x + 4` / `x + 6`：抽样看到**混用**（既有内边距，也有 `cover.x + 6, h: 1` 这种高亮条）；
* `h: 12` / `h: 6`：装饰性短条（均衡器）与状态点（`w: 6, h: 6` 配 `radius 3.0`）——
  后者要缩就得连圆角半径一起缩，属「成套缩放」；
* `y + 8`：混用（`y + 8.0` 是文本基线微调，`list.y + 8` 是内边距）。

**为什么写进代码而不是只写进审计附录**：后来的人读的是 `size` 模块，
不是审计附录；把"为什么不动"放在会被读到的地方，才不会有人当它是漏了。

### CG.4 诚实的边界（没变）

剩下的字面量仍需**逐处判断"该不该缩"**，而其中一部分的判据是**观感** ——
需要能看到屏幕的环境。本批交付的是：**可验证的地基 + 可复用的判据 + 明确的清单**。

### CG.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录（不变量测试已覆盖新常量）；
* **`--shot` 10 档逐字节比对：改动前后完全一致** ✓；
* `gates.py` 15/15（**绿灯才提交**）。
* **未实机验证**：真机上 125%/150% 的观感。

---

## 附录 CH：像素回归**固化成工具** —— 并发现掩码原来是在绕开一个 bug（2026-10-04 批次 CH-1）

### CH.1 一个意外发现：工具早就有了，掩码在绕 bug

准备写"截图回归脚本"时先翻了 `scripts/` —— **`shot-diff.py` 早就存在**：
它做 BMP 像素级对比，并且有一份 `MASK_REGIONS`（"天然非确定区"）：

```python
MASK_REGIONS = [
    (1215, 1280, 0, 32),     # 顶栏时钟
    (515, 530, 595, 630),    # AI 指令条光标   ← 正是附录 CF 修掉的那个 bug
]
```

也就是说：当年遇到"同一状态画出不同画面"时，处理方式是**用掩码把它盖住**，
而不是查为什么。**掩码描述的不是噪声，是一个 bug。**

于是本批不新写脚本，而是**扩展它** —— 并按"同一件事只有一处"的惯例把两件事一起做掉。

### CH.2 升级后的样子

```
python scripts/shot-diff.py save      # 渲染 10 档，把 SHA256 写进 docs/证据/shot-baseline.json
python scripts/shot-diff.py verify    # 重新渲染并比对；有任何不同 → 退出码 1
```

三处关键决定：

1. **脚本自己渲染**（不再要求人工把 BMP 拷进脚本目录）；
2. **掩码空了**：`AETHER_FAKE_UTC=1` 固定时钟 + 附录 CF 修掉光标 —— 输出完全确定，
   `MASK_REGIONS` 现在是空列表（机制保留，将来真出现非确定区时有地方写）；
3. **基线存哈希**：10 档 BMP 约 29 MB，不该进仓库；"改动前后必须逐字节相同"这件事
   一串 SHA256 就够。要人眼看图时加 `--keep` 把 BMP 留下。

旧的 `mask` / `check` 子命令**保留** —— 历史走查图还能用同一套工具比。

### CH.3 负向测试：证明它**真会红**

一个从不失败的门禁等于没有。所以做了负向测试：

* 把 `theme::size::CONTENT_PAD` 从 **24 改成 25**（故意的）；
* `verify` → **8 档报 DIFF，退出码 1** ✓；
* 还原 → 10 档全部 OK ✓。

### CH.4 接进门禁

`gates.py` 增加一步「像素回归（--shot 逐字节 vs 基线）」。它在**没有字体/渲染不出**时
会打印 `[SKIP]` 并返回 0（例如目标平台上没有可用字体）—— **打印原因，不静默跳过**。

### CH.5 验证

* `python scripts/shot-diff.py save` → 基线 10 档；
* `python scripts/shot-diff.py verify` → `[ OK ] 10 档与基线逐字节相同`；
* 负向测试：改 1 个像素 → 8 档 DIFF、退出码 1（已还原）；
* `gates.py` 全绿；构建 0 告警；`cargo test --workspace` 见提交记录。

---

## 附录 CI：让"整体缩放"**可观测** —— 走查加 `--scale`（2026-10-04 批次 CI-1）

### CI.1 为什么这一批不做"继续凑字面量"

上一批结束后抽样看剩余候选，结论是**收益很小**：

* `h: 6` 是状态点（`w: 6, h: 6` 配 `radius 3.0`）—— 要缩就得**连圆角半径一起缩**，属成套缩放；
* `x + 4` / `x + 6` / `y + 8` —— 抽样看到**混用**（既有内边距，也有 `h: 1` 的高亮条、文本基线微调）；
* `ry += 6` / `28` / `50` —— 各只有 3 处，为它们各起一个常量只会让表变肿。

而与此同时，有个**更要紧的问题**：**"整体缩放到底有没有生效"根本没法验证** ——
缩放只能从设置文件读，走查渲染不出 125%。

所以本批换目标：**先把缩放变成可观测的**。

### CI.2 做了什么

1. **`--scale 100|125|150`**（走查用）：直接按指定档位渲染，**不写设置文件** ——
   走查不该改用户配置；非法值由 `ui::set_scale_pct` 吸附到最近合法档
   （合法档位是 `settings::UI_SCALE_STEPS = [100, 125, 150]`）；
2. 接入 `AI_BAR_H = 52`（指令条高度）：`h: 52` 4 处 + 它 y 计算里那个 `52`（3 处）。

### CI.3 证据：缩放真的生效，且 100% 下像素没变

```
--shot 1 --settings  100% vs 125%  → 不同 ✓，差异 x[25,1263] y[6,711]，141,358 个像素
--shot 1             100% vs 125%  → 不同 ✓，差异 91,720 个像素
100% 与既有基线      → 10/10 逐字节相同 ✓
```

两条合起来说明：**路由过的尺寸确实跟着缩放走**（否则 125% 会与 100% 完全相同），
而**100% 下这批重构一个像素都没动**。

### CI.4 为什么不给 125% 也存一份基线

因为**接下来还要继续把尺寸接进表** —— 每接一批，125% 的画面就会变（那正是目的）。
把 125% 也锁进基线，门禁就会对"本来要发生的改动"一直报红，反而让人开始忽略它。

**基线只锁 100%**：它回答的是"这次改动**该不该**改变画面"。
125% 的图用来给人看（一条命令就能生成两档对照）。

### CI.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录（不变量测试已覆盖 `AI_BAR_H`）；
* `python scripts/shot-diff.py verify` → `[ OK ] 10 档与基线逐字节相同`；
* `gates.py` 全绿（含像素回归那一步）。
* **未实机验证**：真机上 125%/150% 的**观感**（本批只证明了"尺寸确实变了"，没证明"变得好看"）。

---

## 附录 CJ：§3.7 蓝牙/打印的**最小切片** —— 只读检测 + 如实报告（2026-10-04 批次 CJ-1）

### CJ.1 为什么这么做（而不是"等有 BlueZ 再说"）

台账原先把 §3.7 记成"蓝牙与打印**仍无** —— 需要各自完整协议栈"。
但项目对**同类问题**已经有既定做法，写在 §3.8 那一行里：

> §3.8 多显示器：**如实报告**已做：检测输出 + 界面说清只绘制主输出、其余只参与息屏。

也就是说：**"做不到的部分"不必让"能如实报告的部分"也一起缺席**。
本批把同一套做法用到 §3.7，让两处的处理方式一致。

### CJ.2 做了什么（**全是只读**）

`aetherd/src/peripherals.rs`：

| 函数 | 性质 | 说明 |
|---|---|---|
| `adapters(dir)` | **纯函数**（目录当参数） | 数 `/sys/class/bluetooth` 里的 `hci*`；地址读不到就是 `None`，**不编一个** |
| `printer_names(conf)` | **纯函数** | 从 CUPS 的 `printers.conf` 取 `<Printer 名字>` 段 |
| `printing(sock, conf)` | **纯函数**（两个路径当参数） | **如实三分**：没有 CUPS / 有但没队列 / 有队列 |
| `list()` | IO 包装 | 用真实系统路径调上面三个 |

协议加 `Request::ListPeripherals` → `Response::PeripheralList`；
`PeripheralWire` 的字段**按种类分开**（`address` / `queues` / `cups`），
不用一个 `detail` 把信息打包成文本 —— 措辞由界面渲染，协议只带事实。

界面：在「可移动设备」页加一节，与列表同一处刷新；**并写明边界**：

> 配对 / 连接 / 提交打印作业都不在本版范围（需 BlueZ / CUPS 一整套）

### CJ.3 测试与门禁

* 4 条单测：只认 `hci*`（`power/`、`uevent` 不算）· 地址缺失时是 `None` 而不是空串 ·
  目录不存在 = "没有"而不是"出错" · `printers.conf` 解析 · 打印状态三分；
* 因为要用临时目录，**自己写了一个 RAII 的 `tempdir`**（仓库没有 `tempfile` 依赖，
  `--offline --locked` 下不该为一个测试加依赖）；
* `ListPeripherals` 是**只读**入口，已登记进 `READ_ONLY`（既有的访问控制表测试会检查）。

### CJ.4 诚实的边界

**仍缺**：配对、连接、音频 profile、提交/查询打印作业 —— 各需 BlueZ / CUPS 一整套。
界面把这句话**印在页面上**，而不是只在文档里说 —— 用户看到的就是"能看到什么、不能做什么"。

### CJ.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：**749 通过 / 0 失败**（含本批 4 条）；
* `gates.py` 全绿（含像素回归那一步）。
* **未实机验证**：真机上插蓝牙适配器/装 CUPS 后的实际读数。

---

## 附录 CK：像素回归覆盖到**每一个设置页** + "不是空白页"的自动检查（2026-10-04 批次 CK-1）

### CK.1 发现：18 个设置页从没进过像素回归

上一批末尾发现：走查**早就有** `--settings-page N`（它的注释写着"走查各页的视觉基线"），
设置页共 **19 个** —— 而像素基线只覆盖了 `--settings`（第 0 页）。

也就是说：**18 个设置页从来没有进过像素回归**，包括前几批新加的
「账户」页（§2.3）与「可移动设备」页里新加的「外设」一节（§3.7）。

### CK.2 做了什么

1. 档位表从 10 档扩到 **29 档**：10 个固定形态 + `--settings-page 0..18`；
2. 新增 **"不是空白页"检查**（不需要人眼）：

   在**内容区**（去掉顶栏与左侧导航栏）数**不同颜色**的个数 ——
   空白页只有面板底色，而真画了内容的页会因为文字抗锯齿产生大量过渡色。

### CK.3 阈值是**实测标定**的，不是拍脑袋

19 个设置页实测的内容区颜色数在 **2997 ~ 3851** 之间（都很均匀）。
阈值取 **24** —— 离实测值差两个数量级，所以：

* 不会误报（正常页面的颜色数远高于它）；
* 但**真空白**的页面（只有底色 + 可能一两条分隔线，个位数颜色）会被抓住。

### CK.4 负向测试：证明这条检查**真会红**

一个从不失败的门禁等于没有。把阈值临时抬到 `100000` → **19 个页面全部报 `[BLANK]`、
退出码 1**；还原为 24 → 29 档全部 OK、退出码 0。

### CK.5 这一批之后，"改动不该改变画面"的守护长什么样

| 层面 | 手段 | 能抓住什么 |
|---|---|---|
| 10 个固定形态 + 19 个设置页 | 与基线**逐字节**比对 | 任何意外改动 |
| 19 个设置页 | 内容区颜色数 > 24 | **页面整个没画出来**（最要命的回归） |
| 缩放 | `--scale` 三档可渲染 | 缩放有没有生效 |

**边界照旧**：这套东西能证明"没变"和"不是空的"，**不能**证明"好看" ——
观感判断仍然需要人（在有屏幕的环境里，一条命令就能生成两档对照图）。

### CK.6 代价

`gates.py` 里那一步现在要渲染 29 档（原来 10 档），耗时增加约两分钟。
值得：它换掉的是"18 个页面无人看守"。

### CK.7 验证

* `python scripts/shot-diff.py save` → 基线 **29 档**；
* `python scripts/shot-diff.py verify` → `[ OK ] 29 档与基线逐字节相同`，退出码 0；
* 负向测试：阈值抬高 → 17 页全 `[BLANK]`、退出码 1（已还原）；
* `gates.py` 全绿；构建 0 告警；`cargo test --workspace` 见提交记录。

---

## 附录 CL：几何断言 —— 当场查出两处溢出，并定位到一个真 bug（2026-10-04 批次 CL-1）

### CL.1 为什么要有它

像素比对能证明「没变」，但证明不了「内容在框里」。而**缩放最容易出的问题就是溢出** ——
那是纯几何问题，用断言查比用人眼看**可靠得多**。

所以先让渲染把「这一帧登记了什么」吐出来：走查加 `--dump-layout`，
输出工作区 / 窗口 / 全部命中区的 JSON；再由 `scripts/layout-check.py` 做断言。

### CL.2 检查项（判据不够硬的不判红）

| 项 | 判红？ | 为什么 |
|---|---|---|
| ① 越出屏幕 | **是** | 任何情况下都不该发生 |
| ② 左右越出设置窗口 | **是** | 不受滚动影响，真正说明问题 |
| ③ 控件矩形重叠 | **只报告** | 实测 24 处多是**行矩形**之间的（行高 44、起点差 42 → 重叠 2 像素），里面的按钮更小，未必真压在一起 —— 判据不够硬就不该判红 |

（试过按「越出窗口」判，得到 **35 处误报** —— 设置正文**可以滚动**，
折叠线以下的控件本来就在窗口矩形之外。所以换成了「左右越界」。）

### CL.3 查出来并**已修好**的两处溢出

| 位置 | 症状 | 根因 | 修法 |
|---|---|---|---|
| 「外观」页 息屏档位 | `IdleBlank(30)` 右边到 x=841，窗口右边是 820（**溢出 21px**） | `x = 起点 + i*84`、`w: 76` —— 4 档共 336，而可用宽度约 310 | **按可用宽度算**：`opt_w = (可用 - 间隙*(n-1)) / n` |
| 「输入」页 鼠标速度 | 150% 下 `MouseSpeed(150)` 右边到 825（**溢出 5px**） | 段控件写死 `w: 300` 且起点在 +24 | 与同页其它行**同起点同宽度**（`row_left + row_w + 12`） |

两处修完后，① 与 ② 都是 **0**（17 页 × 3 档 = 57 个组合）。

**注意这两处都不是缩放引起的** —— 100% 下就已经溢出（第二处只在 150% 露出来，
因为它写死的宽度没跟着缩）。

### CL.4 顺带把「有内容」也验了

同一批几何数据还能确认：**「账户」页（page-9）与「可移动设备」页的「外设」一节**
都登记到了控件 —— 补上了之前只能「声称」的部分。

### CL.5 一个**已定位、尚未修**的真 bug（如实记录）

重叠报告里有一组很显眼：`CycleUiScale`（y 351，高 44）与 `IdleBlank(0)`（y 369，高 34）
**压在一起 26 像素** —— 两个不同控件重叠，不是「行矩形差 2 像素」那种。

读代码后定位到根因：**「外观」页第一组的四行是用 `r1.y + 偏移` 定位的，
而 `ry` 一直停在 `r1.y` 没被推进** —— 于是后面的「壁纸 / 语言 / 息屏」三节
都从 `r1.y + 58` 开始画，**压在第一组那四行身上**。

**修法很直接**：在进入下一节之前把 `ry` 推到第一组下面
（`ry = r1.y + 154 + r1.h`）。**本批没修**：我这一批的调用预算已经用尽，
而改动会**改变「外观」页的画面**（需要同时更新像素基线）——
不该在预算见底时做这种既改行为又动基线的改动。**下一批第一件事就修它。**

### CL.6 验证

* `python scripts/layout-check.py --strict` → `[ OK ] 没有越界`（① ② 都是 0）、退出码 0；
* 两处溢出修复**改变**了「外观」与「输入」两页的画面 → 像素基线已**重新标定**
  （这次变化是**有意的修复**，不是回归）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录。

---

## 附录 CM：几何断言**进门禁**，并当场修掉 4 处真 bug（2026-10-04 批次 CM-1）

### CM.1 修好了上一批定位的那个 bug

上一批（附录 CL.5）用几何断言定位到：「外观」页第一组四行用 `r1.y + 偏移` 定位，
而 `ry` 一直停在 `r1.y` 没推进 —— 后面几节全压在那四行身上。
本批修了（进入下一节前 `ry = r1.y + 154 + r1.h`）。

**效果立竿见影**：重叠从 **24 处降到 6 处**，其中 `CycleUiScale ∩ IdleBlank` 那组直接消失。

### CM.2 判据再拆一次：横向判红、纵向只报告

修完之后冒出 3 处**纵向**越界 —— 而那是**误报**：设置正文**可以滚动**，
内容比可视区高是正常的。所以：

| 项 | 判红？ | 为什么 |
|---|---|---|
| ① 横向越出屏幕 | **是** | 任何情况下都不该发生 |
| ② 左右越出设置窗口 | **是** | 不受滚动影响 |
| ③ 纵向越界 | **否**（只报「这一页需要滚动」） | 正文可滚动 |
| ④ 控件矩形重叠 | **否**（只报告） | 判据不够硬（行矩形之间常有 2px 缝） |

### CM.3 顺着重叠报告又查出并修掉 3 处（同一类 bug）

剩下的重叠都指向同一个模式：**高度会缩放，而步长写死**。

| 位置 | 症状（150%） | 修法 |
|---|---|---|
| 「关于」页 电源三按钮 | `Power(PowerOff) ∩ Power(Reboot) = 3528px²` | `ry += 48` → `ry += px(CTRL_H) + 4` |
| 「网络」页 两行（DNS / 服务重启） | `EditDns ∩ RestartService = 2940px²` | `ry += 50` → `ry += px(CTRL_H) + 6`（**100% 下与 50 完全等价**） |

**重叠 6 → 2 → 1**。剩下那一处是 `page-1@150% TzShift(-30) ∩ TzShift(30) = 180px²`
（很小），留待核对 —— **不硬凑**。

### CM.4 进门禁

`gates.py` 增加一步「布局几何（越界 / 重叠）」，跑 `layout-check.py --strict`
（17 页 × 3 档 = 57 个组合）。它判红的是**横向越界**；纵向与重叠只打印。

**代价**：这一步要渲染 57 帧（约 5 分钟）。与像素回归那一步加起来，门禁从约 5 分钟变成约 10 分钟。
值得：它换掉的是「19 个页面的几何没人看守」。

### CM.5 这一批的账

* **修掉 4 处真 bug**（1 处来自上一批的定位，3 处来自这一批的重叠报告）；
* 重叠 24 → 1；
* 几何断言成为常驻门禁；
* 横向越界与纵向越界**分开对待**（前者判红，后者只报「需要滚动」）。

### CM.6 验证

* `python scripts/layout-check.py --strict` → `[ OK ] 没有横向越界`、退出码 0；
* 四处修复**改变**了「外观 / 关于 / 网络」等页的画面 → 像素基线已**重新标定**
  （变化是**有意的修复**，不是回归）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿（含新那一步）。

---

## 附录 CN：设置页控件重叠**清零**，几何断言扩到窗口类界面（2026-10-04 批次 CN-1）

### CN.1 修掉最后一处设置页重叠

上一批剩下 `page-1@150% TzShift(-30) ∩ TzShift(30) = 180px²`。
实测两个按钮：`x` 分别 525 与 561、宽都是 40 → **步长 36 < 宽度 40**，
所以**横向压了 4 像素**（180 = 4×45）。

修法：第二个按钮的位置**从第一个派生**（`fwd.x = back.x + back.w + GAP_TIGHT`），
宽度与步长同源 —— 任何缩放下都不可能重叠。

**结果：设置页的控件重叠 24 → 0。**

### CN.2 几何断言扩到窗口类界面

`--dump-layout` 原来只吐设置页的命中区。现在把**桌面类**的可点区域也吐出来：

| 组 | 内容 |
|---|---|
| `dock` | Dock 图标 |
| `control` | 控制中心的命中区 |
| `files-sidebar` / `files-crumb` | 文件窗口的侧栏项与面包屑 |
| `window-lights` | 每个窗口的关闭/最小化/最大化三键 |

检查脚本随之覆盖 9 个窗口类形态 × 3 档。结果：**横向越界 0**。

### CN.3 两处判据的收窄（都是"别把正常当异常"）

1. **窗口三键的重叠只在同一窗口内算**。`layout4` 是**堆叠布局**：多个窗口共用同一区域，
   它们的按钮位置本来就重合 —— 初版报了 **106 处**，全是这个原因；
2. **2 像素行缝不算问题**。剩下 79 处同组重叠，看数值就知道是行矩形之间压了 2 像素
   （`512 = 256×2`、`402 = 201×2`）—— 与设置页那批同源，**只报告不判红**。

**判红项仍是 ① ② ⑤（横向越界），全部为 0，退出码 0。**

### CN.4 顺手堵掉一个流程坑

本会话我**多次**因为临时脚本里的引号写错而白跑一轮（第 12 次是上一批）。
查了一下：`gates.py` **已经**对 `scripts/*.py` 跑 `py_compile` ——
但我的临时脚本写在 `%TEMP%`，覆盖不到。

所以改的是**我自己的调用方式**：这一批起，每个临时脚本都先
`python -m py_compile <脚本>` 再运行（本批的几条命令里都能看到这一步）。
门禁管不到的环节，只能靠流程补。

### CN.5 这一批的账

* 设置页控件重叠 **24 → 0**（本会话累计修掉 **5 处**"尺寸缩放而步长写死"类真 bug）；
* 几何断言从设置页扩到**窗口类界面**（9 形态 × 3 档）；
* 判据收窄两处（堆叠窗口、2 像素行缝），避免把正常当异常；
* 临时脚本先编译再运行 —— 补上门禁覆盖不到的那一环。

### CN.6 验证

* `python scripts/layout-check.py --strict` → `[ OK ] 没有横向越界（① ② ⑤ 都是 0）`、退出码 0；
* 时区按钮那处修复**改变**了「时钟与时区」页的画面 → 像素基线已**重新标定**（有意修复）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿（17 项）。

---

## 附录 CO：**从源码里找**同类 bug —— 静态扫描一次挖出 6 处（2026-10-04 批次 CO-1）

### CO.1 动机

前几批修掉的 bug 全是同一类：**矩形高度走 `theme::size::px(...)`（跟着缩放），
而步长是裸数字**。它们在 100% 下恰好不重叠，到 125%/150% 才压住。

几何断言能撞到它们，但那是**碰运气** —— 要正好覆盖到那一页、那一档。
所以这一批改成**从源码里找**。

### CO.2 判据是精确的，不是猜

对每个 `ry += N`（裸数字），往前找最近的、**`y` 就是 `ry`** 的矩形，
看它的高度是不是走了 `size::px`：

* 是 → 记下常量值 `H`；
* 判据：**`N < 1.5 × H` 就一定在 150% 下重叠**（`N < 1.25 × H` 则 125% 下就会）。

常量值直接从 `theme::size` 模块解析出来，不手抄。

### CO.3 结果：6 处真候选，全部修好

| 行 | 原步长 | 高度常量 | 修法（100% 下等值） |
|---|---|---|---|
| 「可移动设备」重新扫描后 | 40 | `ROW_H_SM`=36 | `px(ROW_H_SM) + 4` |
| 「快照」创建后 | 50 | `ROW_H`=40 | `px(ROW_H) + 10` |
| 「外观」恢复默认后 | 52 | `CTRL_H`=44 | `px(CTRL_H) + 8` |
| 「时钟」两个开关行 | 48 / 54 | `CTRL_H`=44 | `px(CTRL_H) + 4` / `+ 10` |
| 「账户」重新读取后 | 40 | `ROW_H_SM`=36 | `px(ROW_H_SM) + 4` |

**关键验证**：改完跑 `shot-diff.py verify` → **29 档与基线逐字节相同**，
**基线一个字都不用改** —— 这直接证明这些修复在 100% 下**完全等价**，
只是把"恰好相等"变成了"结构上不可能重叠"。

### CO.4 扫描器的三类误报（都写进了规则里）

1. **步长前面紧跟着另一个 `ry +=`** → 高度已被那一步消化，这里的 N 是**额外间隙**；
2. **配对距离超过 10 行** → 多半是别处的矩形（中间已经画了文本）；
3. **跨页 arm** → `"某页" => {` 之后 `ry` 是**重新起算**的，
   把上一页的矩形与下一页的第一步配上是没有意义的（本批最后一处误报就是它）。

三类规则加完，扫描结果 **0** —— 于是可以判红了。

### CO.5 进门禁

`gates.py` 增加一步「缩放硬编码扫描（步长 vs 缩放高度）」，跑 `scale-audit.py --strict`。
它是**纯静态**的，瞬时完成（不像像素回归与几何断言要渲染几十帧）。

至此，"尺寸缩放而步长写死"这一类问题有了**三层**防护：

| 层 | 手段 | 何时发现 |
|---|---|---|
| 源码 | `scale-audit.py`（静态） | 写代码时（最快、最便宜） |
| 几何 | `layout-check.py`（17 页 × 3 档 + 窗口类） | 跑门禁时 |
| 像素 | `shot-diff.py`（29 档） | 跑门禁时（抓"不该变的变了"） |

### CO.6 验证

* `python scripts/scale-audit.py --strict` → 0 处、退出码 0；
* `python scripts/shot-diff.py verify` → **29 档逐字节相同**（基线未改）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿。

---

## 附录 CP：静态扫描扩到横向 —— 以及它**查出 0 处**这件事本身（2026-10-04 批次 CO-2）

### CP.1 做了什么

`scale-audit.py` 增加**横向**判据：矩形里同时有 `w: size::px(W)`（宽度跟着缩放）
与 `x: … + … * STEP`（写死的步长）→ `STEP < 1.5×W` 时相邻两个会重叠。

同时把上一批踩出来的**误报规则**都写进脚本：① 步长前紧跟另一个 `ry +=`；
② 配对距离超 10 行；③ 跨页 arm；④ 宽度是字面量（不缩放）。

### CP.2 结果

```
① 纵向「ry += N vs 缩放高度」：0 处，其中会重叠 0
② 横向「x 的 STEP vs 缩放宽度」：0 处，其中会重叠 0
[ OK ] 纵向与横向都没有「步长小于缩放后尺寸」
```

### CP.3 但 ② 的 0 处**不是**"没问题"，而是"这一类不长这样"

本会话修过的两处**横向** bug 是：

* 「外观」页 息屏档位：`x = 起点 + i*84`、`w: 76` → 4 档共 336，**整行超出容器**（溢出 21px）；
* 「输入」页 鼠标速度：段控件 `w: 300` → **整行超出容器**（溢出 5px）。

两处的相邻按钮之间**并不重叠**（84 > 76、300/3 段各自独立），
所以横向静态判据抓不到 —— **只有 `layout-check.py` 的横向越界断言能抓**（它确实抓到了）。

**所以 ② 留着是给"相邻重叠"这个子类当守卫**（将来有人写 `w: size::px(X)` 配写死步长时会命中），
而不是"已经扫干净了"。**这条如实写在脚本的文档串里**，免得后来的人误以为 0 就是全清。

### CP.4 三层防护的定位（各自抓什么）

| 层 | 手段 | 抓什么 | 何时发现 |
|---|---|---|---|
| 源码 | `scale-audit.py`（瞬时） | **相邻**步长/宽度不足 | 写代码时 |
| 几何 | `layout-check.py`（渲染 84 帧） | **越界**与重叠（含整行超出容器） | 跑门禁时 |
| 像素 | `shot-diff.py`（渲染 29 帧） | 「不该变的变了」与「页面空白」 | 跑门禁时 |

**三者互补，不是替代** —— 本批正好证明了这一点：横向那两处**只有几何层能抓**。

### CP.5 验证

* `python scripts/scale-audit.py --strict` → 0 处、退出码 0；
* `python scripts/shot-diff.py verify` → 29 档逐字节相同；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿（18 项）。

---

## 附录 CQ：收尾后的**整体审计（第 5 遍）** —— 覆盖本会话全部新增代码（2026-10-04）

规程要求「所有任务完成后整体审计 3 遍」。附录 T / U / V 是最早那三遍；
但此后又做了 §2.3 账户、§3.5 i18n、§4.1 e2e、§3.7 外设切片、§2.4 缩放与三层防护 ——
所以又补了 **CC**（§2.3/§4.1）与这一遍 **CQ**（全部新增代码 + 新增脚本）。

### CQ.1 取证结果（每条都有命令与实测输出）

| # | 检查项 | 结果 |
|---|---|---|
| 1 | 新 Rust 代码（`peripherals.rs` / `accounts.rs`）生产路径里的 `unwrap()` / `expect()` | **0 处** |
| 2 | `peripherals.rs` 里的 `println!` / `eprintln!` | **0 处**（守护进程路径不往 stdout 写） |
| 3 | 字符串拼接命令、`sh -c` | **0 处**（全库） |
| 4 | 口令相关（§2.3）：进审计 / 进 argv / 进日志 | 全部**否**（只经 `chpasswd` 的 stdin；审计只记 `user`/`action`） |
| 5 | 新增的 3 个脚本是否**会静默失败** | 都有 `[SKIP]` / `[FAIL]` 且 `--strict` 下退出码非 0 |
| 6 | 新增请求（`ListUsers` / `ListPeripherals` / `SetPassword`）的访问控制 | 都过 UI 门槛；只读的已登记进 `READ_ONLY`（由不变量测试强制） |
| 7 | 新增文件权限 | 未引入任何新文件写入路径；快照 0700 / 审计 0600 / IPC 0600 未变 |

### CQ.2 本会话新增的**三层防护**（这本身也是一种审计能力）

| 层 | 手段 | 抓什么 |
|---|---|---|
| 源码 | `scripts/scale-audit.py` | 「尺寸缩放而步长写死」的**相邻**不足（写代码时就能发现） |
| 几何 | `scripts/layout-check.py` | **横向越界**与控件重叠（19 设置页 × 3 档 + 9 窗口形态 × 3 档） |
| 像素 | `scripts/shot-diff.py` | 「不该变的变了」（29 档逐字节）与「页面是空白的」 |

它们**不是替代关系**：本会话的横向 bug（整行超出容器）**只有几何层能抓**（附录 CP.3）。

### CQ.3 结论

**本会话新增的代码没有发现新的安全漏洞。**
五遍整体审计（T / U / V / CC / CQ）合计产出：

* 一条**会红的门禁**（访问控制表不变量：新请求不分类就报错）；
* **两次方法论更正**（扫描器结论必须人工复核；取证要变成不变量）；
* **一条流程更正**（增量构建的 0 告警 ≠ 全量 0 告警，附录 CB.2）；
* **两条文档准确性更正**（`println!` 的适用范围，附录 CC.2；静态扫描 0 处 ≠ 全清，附录 CP.3）；
* **三层可复现的防护**，且它们各自抓到过真实问题。

### CQ.4 仍然**未验证**的（照旧如实列出）

* 真机行为 10 项（附录 CF 之后的收尾报告 §5）；
* 依赖库 CVE（本环境 `--offline`，跑不了 `cargo audit`）；
* 125%/150% 的**观感**（本会话只证明了"尺寸确实变了、不越界、不重叠"，没证明"好看"）。

---

## 附录 CS：150% 下**文字真的压住了** —— 实拍发现、根因、修复与剩余（2026-10-05 批次 CS-1）

### CS.1 怎么发现的

用户问"能不能在虚拟机里用"、"要看最新代码"。在**没有构建机密码**的情况下，
改用 `--shot` 直接渲染当前源码（1280×760 BMP → PNG），**人眼看图**：

* 100% 正常；
* **150% 破图** —— 侧栏分组标题压在条目上、外观页每行的标题与说明叠在一起、
  终端里 `AetherOS 0.1.0` 被渲染成 `Aet her OS 0. 1. 0`。

**这是本项目第一次对"缩放后的观感"做实际检查。** 之前所有"125%/150% 没问题"的说法，
依据都是**命中区矩形**的几何断言 —— 而**文字不在命中区里**。

### CS.2 文档要更正的措辞

以前写的「125%/150% **不越界、不重叠**」**只对命中区成立**。
这句话本身没错（命中区确实不重叠），但读者会理解成"看着正常" —— 这是**过度声明**。
已在本附录与台账里写明限定语。

### CS.3 根因（只有一类）

「外观」页第一组的四行用**写死偏移**定位：

```rust
let r1      = Rect { x: …, y: ry, … };          // 基准行
let r_hc    = Rect { x: r1.x, y: r1.y + 56,  … };  // ✗ 56 不缩放
let r_lang  = Rect { x: r1.x, y: r1.y + 112, … };  // ✗
let r_scale = Rect { x: r1.x, y: r1.y + 154, … };  // ✗
```

行高 `ROW_H` 走 `size::px` 会缩放（40 → 60），而**步长 56 不缩放** → 行与行压住。

**这一类静态扫描以前看不到**：扫描器只盯 `ry += N`，而这种写法是 `y: r1.y + 56`。

### CS.4 修了什么（共 9 处）

| 位置 | 原写法 | 修法（100% 等值） |
|---|---|---|
| 外观页 r_hc / r_lang / r_scale | `r1.y + 56 / +112 / +154` | 走 `theme::size::px` |
| 面板列表项步长 | `panel.y + 4 + i * 30` | `px(4) + i * px(30)` |
| 音乐面板 | `panel.y + 30` | `px(30)` |
| AI 指令条圆点 | `bar.y + 23` | `px(23)` |
| AI 指令条文字左缘 | `bar.x + 58` | `px(58)` |
| 窗口标题栏分隔线 | `win.y + 58` | `px(58)`（**线粗仍是 1px** —— 位置缩放、粗细不缩） |
| 状态圆点位置 | `content.x + 22` | `px(22)`（**圆点尺寸 6px 故意不缩**，见 `size` 模块注释） |

### CS.5 验证（两条，都是硬的）

1. **100% 下 29 档像素基线逐字节未变** —— `shot-diff.py verify` 报
   `[ OK ] 29 档与基线逐字节相同`。这直接证明这 9 处改动在 100% 下**完全等价**，
   只是把"恰好相等"变成"结构上跟着缩放"。
2. **150% 实拍前后对比**（`docs/证据/最新构建-外观页-150%.png` 与
   `…-150%-修复后.png`）：外观页四行的标题与说明**从压在一起变成分开**。

### CS.6 还剩什么没修（如实列出）

150% 的修复后图里，**下面三处仍然挤**，它们是**另外三条绘制路径**，本轮没动：

* **文件窗口侧栏**（`上一级目录 / 目录 / 文档 / 终端 / 图片 / 设置` 互相压）；
* **终端文字**（`AetherOS 0.1.0` 被渲染成 `Aet her OS 0. 1. 0` —— 字距被拆散，
  这更像**字体步进**问题，不是行高问题）；
* **音乐窗口列表**。

**没有把它们说成"已修"** —— 下一步该逐个查这三条路径。

### CS.7 静态扫描加了第三类判据

`scripts/scale-audit.py` 现在查三类：① `ry += N` vs 缩放高度；② 横向 `x` 步长 vs 缩放宽度；
③ **`矩形.y/x + 裸数字`（≥20）**。

**阈值 20 是刻意选的**：`+1`（细线）、`+3`（圆角内衬）、`+4/+6`（与高光条混用）是
`theme::size` 注释里写明的**故意不缩**项 —— 把它们也算进来只会制造噪音。

### CS.8 验证

* `python scripts/scale-audit.py --strict` → 三类都是 0、退出码 0；
* `python scripts/shot-diff.py verify` → 29 档逐字节相同（基线未改）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿。

---

## 附录 CT：把「循环/累积步长」也接进缩放（2026-10-05 批次 CT-1）

### CT.1 动机：扫描器先看不见，人眼看图才发现

上一批（附录 CS）修好「外观」页那三行后，150% 实拍图里**仍然**有三处挤。
顺手扫全库"循环/累积步长"发现：`y += 20/22/26`（**文字行距**）与
`ry += 28…78`（**累积步长**）共 **20 处**写死 —— 与外观页是**同一类**，
只是写法从 `y: r1.y + N` 变成 `i * N` / `y += N`，所以旧的判据扫不到。

### CT.2 本批修了什么（22 处）

| 类别 | 处数 | 例子 |
|---|---|---|
| 文件窗口侧栏 | 2 | `base += 30`、`i as i32 * 30` |
| **文字行距** | 11 | `y += 20 / 22 / 26 / 28 / 32 / 34`、`ty += 22.0` |
| **累积步长** | 9 | `ry += 28 / 36 / 42 / 50 / 52 / 78` |

### CT.3 验证：**100% 下 29 档像素基线逐字节未变**

这一批一次改了 20 处，风险看起来不小 —— 但 `theme::size::px(n)` 在 100% 下**等于 n**，
所以理论上 100% 必须**一个像素都不变**。实测：

```
[ OK ] 29 档与基线逐字节相同
```

**这就是这套像素基线的价值**：它把"批量机械改动"从"需要逐处复查"变成
"一条命令就能证明 100% 无副作用"，于是可以放心地一次改 20 处。

### CT.4 效果与**仍未修**的部分（实拍对比）

150% 实拍（`docs/证据/最新构建-150%-第二批修复后.png`）：

* **变好** ✓：设置窗口侧栏（个性化 / 系统 / 设备 / 输入 / 快捷键）**分开了**；
  外观页各行与说明之间的距离也松开了；
* **仍挤** ✗：**文件窗口侧栏的分组标题**、**终端字距**、**音乐列表**。

其中**终端字距**（`AetherOS 0.1.0` → `Aet her OS 0. 1. 0`）是**另一个根因**：
不是行距，而是**字体步进**（每个字的推进量按未缩放的度量算）。本批**没有**动它，
**不把它说成已修**。

### CT.5 扫描器现在有四类判据

| # | 判据 | 抓什么 |
|---|---|---|
| ① | `ry += N` vs 前面那个缩放高度 | 相邻行压住 |
| ② | 横向 `x` 步长 vs 缩放宽度 | 相邻控件压住 |
| ③ | `矩形.y/x + 裸数字`（≥20） | 写死偏移（外观页那类） |
| ④ | 循环/累积步长（`i * N`、`y += N`，≥20） | 本批这类 |

阈值 20 是刻意的：`+1`/`+3`/`+4`/`+6` 是 `theme::size` 注释里写明的**故意不缩**项。
④ 另加一条排除：`items.len() * N` 是**数量乘**（算面板高度），不是步长 —— 实测误报。

### CT.6 一个流程上的自省

本批中途我又用了一次内联 `python -c "…"`，**第 13 次**死在引号上（脚本没跑成）。
上一批我自己立的规矩是"**写脚本文件 → 先 `py_compile` → 再运行**" ——
门禁管不到我写在 `%TEMP%` 的临时脚本，只能靠这个流程。这次偷懒立刻付了代价，
改回脚本文件后一次通过。**规矩不是装饰。**

### CT.7 验证

* `python scripts/scale-audit.py --strict` → 四类都是 0、退出码 0；
* `python scripts/shot-diff.py verify` → 29 档逐字节相同（基线未改）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` 全绿。

---

## 附录 CU：把「手抄数字」变成「门禁会红」（2026-10-05 批次 CU-1）

### CU.1 起因：一个**不是我做**的提交，指出的是我的错

`0ef4324`（2026-10-04 21:55，标题「文案核对与更正：收尾报告 **5 处数字失实/自相矛盾**」）
修的全是我的问题：

| 我写的 | 实际 |
|---|---|
| P99 **25.17 ms**（当成固定值） | 单次实测、每次波动（复测 28.62 ms） |
| 本会话批次提交 **89 条** | 86 条，且该口径**随时间滑动** |
| 审计附录 **68 篇** | 69 篇 |
| 某处仍写「**741 项 / 15 项门禁**」 | 749 / 18（**自相矛盾**） |
| `gates.py` 注释「15 项门禁」 | 18 项 |

共同毛病：**数字靠手抄，不靠来源**。而我当时甚至在几轮里看着它从 93 → 89 → 86 变，
却用"含 amend"搪塞过去，没有去查。

**而且它必然继续失实**：本批实测已经是**提交数 124**、**审计附录 71**（报告里写 86 / 69）。

### CU.2 本批做了什么

1. **新增 `scripts/doc-numbers-check.py`**：从来源**推导**三个数字并比对 ——

   | 数字 | 推导方式 |
   |---|---|
   | 测试数 | `cargo test -- --list` 里以 `: test` 结尾的行数（实测与「N passed」求和**完全一致**：749） |
   | 门禁数 | 读 `docs/证据/gate-count.txt` |
   | 审计附录数 | 审计文档里 `^## 附录` 的行数 |

2. **`gates.py` 在跑之前把本次步骤数写进 `docs/证据/gate-count.txt`** ——
   这样**同一次运行内自洽**：谁改了门禁步骤数，这一步当场红，逼着改文档；
3. **报告里删掉"提交数"那个数字**：它随时间滑动，写死必然失实 —— 只留命令；
4. 报告里的**审计附录 69 → 71**。

### CU.3 负向测试：证明它**真会红**

把报告里的测试数故意改成 **748** → 这一步报

```
① 测试数：文档 748 / 实测 749
[FAIL] 文档里的数字与来源不一致（这就是「手抄必然失实」的现场）：
        测试数：文档 748，实测 749
```

**退出码 1**。然后改回 749。一个从不失败的门禁等于没有 —— 这条规矩本批照做。

### CU.4 当场就抓到一次

本批给 `gates.py` 加了 1 步，门禁数从 **18 → 19** ——
报告里那个 18 **当场变成错的**，正是这道门禁要抓的东西。已在同一批里改成 19，
并复跑全量门禁：`全部 19 项门禁通过`。

### CU.5 仍然**不检查**的两类数字（说明理由）

* **提交数**：随时间滑动，**不该写进文档**（所以不是"检查它"，而是"不许写它"）；
* **性能 P99**：单次实测值，每次运行波动 —— 文档里保留的是**带日期的样本**
  （「2026-10-04 复测 28.62 ms」），并写明"以当次输出为准"。

### CU.6 这条门禁属于哪一层

项目已经有三层**界面**防护（源码静态扫描 / 几何断言 / 像素回归）与"文档规模 + 集合相等"门禁。
本批补的是**文档数字**这一层：以前只有"文件数/行数"是机器核对的（`repo-stats.py --check`），
其余全靠我抄 —— 现在测试数、门禁数、附录数也进了门禁。

**教训写在这里**：手抄的数字不是"可能"失实，是**必然**失实；能推导的就别抄。

### CU.7 验证

* `python scripts/doc-numbers-check.py` → ① 749/749 ② 19/19 ③ 71/71、退出码 0；
* 负向测试：改成 748 → 退出码 1（已还原）；
* `python scripts/gates.py` → **全部 19 项门禁通过**；
* 构建 0 错误 0 警告；`cargo test --workspace`：749 通过 / 0 失败。

---

## 附录 CV：终端字距 —— 根因、一次**可证明失败**的尝试，与正确的修法（2026-10-05 批次 CV-1）

### CV.1 根因（代码证据，不是猜）

| 环节 | 事实 |
|---|---|
| 终端排版 | `draw_term_content` 用**等宽网格**：`tr.mono_cell()` 取格宽，按 `col * cell_w` 定位字符；列数/行数/选择范围都依赖统一格宽 |
| 格宽怎么算 | `mono_cell()` 里 `w = measure("M", font::MONO)` —— **拿最宽字母之一当基准** |
| 实际字体 | 正文体是 **Noto Sans SC（比例字体）**，拉丁字母宽窄不一 |
| 结果 | 窄字母（t / l / i）在自己格子里**右边留空** → 看起来就是「字距被撑开」。100% 只有 1~2px 几乎看不出，**150% 放大 1.5 倍就很明显** |

### CV.2 第一次尝试：**修好了一半，另一半更糟** —— 而且我证明了自己没修对

给终端单独加载等宽字体（`MONO_FONTS` + `AETHER_FONT_MONO` + 缓存键加 `mono` 位）。
实拍结果：

* ✓ **拉丁间距正确了**：`aether@localhost:~$`、`AetherOS 0.1.0 aether-kernel`（此前是 `Aet her OS 0. 1. 0`）；
* ✗ **中文变成豆腐块** □ —— Consolas 不含中日韩字形。

我加了"缺字形就退回正文体"，判断条件写成 `width == 0 || height == 0`。
**实测无效** —— 证据是**渲染出来的图与改前逐字节相同（同一 sha256）**：
Consolas 对缺字形返回的是 `.notdef` **豆腐块字形，它是有宽度的**。

**这次实验没有提交**：带着"终端中文变豆腐块"这种已知回归留在工作区是不负责任的，
所以 `git checkout` 回滚，回到已提交、已绿色的状态。**回滚本身也是本批的结论之一。**

### CV.3 第二次（正确）：先读库源码，再改

按本会话反复得到的教训 —— **别猜 API，去读它**。
在 `~/.cargo/registry/.../fontdue-0.9.4/src/font.rs:630` 读到：

```rust
pub fn has_glyph(&self, character: char) -> bool {
    self.lookup_glyph_index(character) != 0
}
```

`lookup_glyph_index` 对**字体里没有的字符返回 0** —— 这才是"缺字形"的正确判据。
于是改成**逐字回退**：

```rust
let use_mono = mono && self.mono.has_glyph(ch);
let font = if use_mono { &self.mono } else if bold { &self.bold } else { &self.regular };
```

（缓存键仍用**请求**的 `mono`，不用回退后的选择：同一请求的结果是确定的。）

### CV.4 验证：两档都实拍看过

* **100%**：`aether@localhost:~$ uname -srv` / `AetherOS 0.1.0 aether-kernel` / `docs  src  run.sh  core.log`
  —— 等宽、整齐；中文 `宽字符对齐：中文 与 ASCII 混排` 正常；
* **150%**：同样正确（这正是本轮要修的那一档）；
* 证据：`docs/证据/CV-100%-等宽字体.png`、`docs/证据/CV-150%-等宽字体.png`。

### CV.5 基线重新标定 —— 以及我**为什么**敢说变化是"只有终端"

换字体**有意改变**了 100% 的画面，所以重新 `save` 基线。但"重新标定"不能变成
"看到红的就抹掉"，所以先查了两件事：

1. **`font::MONO` 只被终端使用** —— 全库 2 个调用点（`draw.rs:5238`、`draw.rs:5289`），
   都在 `draw_term_content` 里；
2. **每一档都能看到终端窗口**（设置窗口浮在它上面），**只有 `desktop`（`--desktop-only`）
   不画窗口**。

预测：`desktop` 应当是**唯一没变**的档。实测：**28 档变化、`desktop` 未变** ✓ —— 与预测一致，
所以这次重新标定是有依据的，不是掩盖。

### CV.6 诚实的边界：**目标系统上这一步还不生效**

`MONO_FONTS` 里给目标系统列的是 `NotoSansMonoCJKsc-Regular.otf` 等路径，
但 **overlay 目前只子集化了 Noto Sans SC（比例字体）**，没有等宽字体。
也就是说：**在没有等宽字体的机器上，`load_font` 会退回正文体，终端退化成今天的样子**
（间距略松，但不缺字形、不崩）。要让修法在真机生效，需要
`platform/build-iso.sh` 再子集化一份等宽字体放进 overlay —— **构建侧待补**。
本批只改合成器，**没有**动构建链。

### CV.7 顺手记一个流程失误

本批我忘了跑 `sync-doc-numbers.py` 与 `gen-site.py --refresh`，
于是**门禁当场红了 2 项**（文档规模 + 站点：`text.rs` 从 695 行长到 740 行）。
这正是门禁该做的事 —— 它没让我把过期数字提交进去。

### CV.8 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：**749 通过 / 0 失败**；
* `shot-diff.py verify` → **29 档与基线逐字节相同**（基线已按 CV.5 重新标定并说明理由）；
* `gates.py` → **19 项全过**；
* 实拍：100% 与 150% 两档终端中英混排都正确。

---

## 附录 CW：音乐列表在 150% 下的重叠 —— 以及**一处我自己搞错的判断**（2026-10-05 批次 CW-1）

### CW.1 先纠正上一批的一个错判

附录 CV 的收尾里我写过「**文件侧栏的分组标题仍然挤**」。**那是错的。**
这一批我用数据核了一遍 —— 把 `--shot 1` 在 100% 与 150% 两档的命中区矩形打出来：

| 档 | 侧栏条目 y | 步长 | 行高 | 缝 |
|---|---|---|---|---|
| 100% | 121 / 151 / 181 / 211 / 241 | **30** | 26 | 4px |
| 150% | 142 / 187 / 232 / 277 / 322 | **45** | 39 | 6px |

**步长与行高都跟着缩放，没有重叠。** 我先前是从一张 1280×760 的缩略图上"看"出挤的 ——
**低分辨率缩略图不能当证据**。这与本会话反复得到的教训是同一条：
**别用眼睛在缩略图上判，去取数据。**

（顺带说明：侧栏条目**没有**"分组标题"这个东西，我把文件名当成了标题。）

### CW.2 音乐列表的真因（读代码得到）

`draw_music_content` 里有**局部常量**，代码**裸用**：

```rust
const ROW_H: i32 = 46;   // 行距：y += ROW_H —— 不缩放
const ART:   i32 = 30;   // 封面尺寸 —— 不缩放
```

而字号**会**缩放（BODY 14→21、LABEL 11→16.5）→ 150% 下行距不变、字变大，必然压住。
行内两行文字更直接：

```rust
tr.vcenter(row.y + 2.0,  20.0, font::BODY)    // 标题：起点 +2、带高 20
tr.vcenter(row.y + 22.0, 16.0, font::LABEL)   // 歌手：起点 +22、带高 16
```

**全是裸数字** —— 字号一大就叠。

### CW.3 修了 10 处

行距 `y += ROW_H` · 越界判断 · 行矩形高度 · 播放指示条（位置与长度）· 封面尺寸与位置 ·
文字起始 x · 标题带高与起点 · 歌手带高与起点 · 时长带高 · 顶部留白。

**没有改** `PANEL_W` / `PANEL_MIN_W`：它们是**宽度决策**（还要与窗口宽度比较来决定
"要不要显示右侧面板"），缩放它们会改变那个判断 —— 本轮不碰，如实记在这里。

### CW.4 验证：100% 下**基线一个字都没动**

`shot-diff.py verify` → **`[ OK ] 29 档与基线逐字节相同`**。

这是本批最干净的一条证据：10 处改动**全部**是 `px(n)` 形式，
在 100% 下 `px(n) == n`，所以**理论上**必须一个像素都不变 —— 实测一致。
对比附录 CV（换字体那批）需要重新标定基线，这一批**不需要**，因为它没有改变 100% 的意图。

150% 实拍（`docs/证据/CW-150%-音乐列表修复后.png`）：曲名与歌手**彻底分开**，
行距与封面都跟着缩放。

### CW.5 顺带发现一个**覆盖盲区**

音乐列表**没有命中区向量**（`Renderer` 里只有 `settings_hits` / `control_hits` /
`sidebar_hits` / `crumb_hits` / `window_lights` / `dock_icons`），
所以它**不在几何断言的覆盖范围内**；而静态扫描的第三类判据只认
`矩形.y + 整数`，**认不出** `vcenter(..., 20.0)` 这种**浮点带高**。

也就是说：**"行内文字排版"这一类，目前两层自动检查都看不见** ——
本轮是靠"读代码 + 实拍"发现的。**下一步该给扫描器加一类浮点判据**（记在这里，不假装已做）。

### CW.6 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：**749 通过 / 0 失败**；
* `shot-diff.py verify` → **29 档与基线逐字节相同**（基线**未改**）；
* `scale-audit.py --strict` → 四类判据都是 0；
* `gates.py` → **19 项全过**。

---

## 附录 CX：静态扫描加第 ⑤ 类 —— 补上「行内文字排版」这个盲区（2026-10-05 批次 CX-1）

### CX.1 盲区是怎么来的

附录 CW 发现：音乐列表那种"**行内两行文字**"的重叠，**两层自动检查都看不见** ——

* 几何断言：音乐列表**没有命中区向量**（只有可点区域才登记），所以不在覆盖范围；
* 静态扫描第三类：只认 `矩形.y + 整数`，而那里的写法是 `vcenter(row.y + 22.0, 16.0, …)`，
  **是浮点**。

于是本轮给扫描器加第 ⑤ 类：**`vcenter(..., <浮点带高>)` 与 `<rect>.y + <浮点偏移>`**。

### CX.2 先报告，再判红

按本项目一贯做法，新判据**先只报告**，人工过一遍再判红。首次运行报出 **9 处**候选，其中确有会溢出的：

| 位置 | band | 字号（150% 后） | 判断 |
|---|---|---|---|
| `vcenter(y, 20.0, CAPTION)` | 20 | 12 → 18 | 放得下 |
| `vcenter(ry, 42.0, CAPTION)` | 42 | 12 → 18 | 放得下 |
| `vcenter(panel.y + 6.0, 18.0, LABEL)` | 18 | 11 → 16.5 | 勉强放得下 |
| **`vcenter(label_y, 14.0, LABEL)`** | **14** | **11 → 16.5** | **溢出** |
| **`vcenter(y, 20.0, BODY)`（3 处）** | **20** | **14 → 21** | **溢出** |

### CX.3 修了 11 处

把所有 `vcenter(<y>, <band>, <font>)` 里的 **band** 与 **y 里的浮点偏移** 接进缩放
（`theme::size::px(N) as f32`）。**不动第 3 个参数**：那是字号，有的地方写的是字面量
（如 `17.0`），属于**另一类问题**，不混进来。

### CX.4 验证：100% 下基线**逐字节不变**

`shot-diff.py verify` → **`[ OK ] 29 档与基线逐字节相同`**。

11 处改动全部是 `px(n)` 形式，在 100% 下等于 n，所以**理论上**必须一个像素都不变 —— 实测一致。
（这一批与附录 CV 那批形成对照：**换字体**必须重新标定基线；**把裸数字接进缩放**不需要。）

改完再跑扫描器：**⑤ 类归零**，于是把它**从"只报告"改成"判红"**，纳入门禁。

### CX.5 现在这套静态扫描有五类判据

| # | 判据 | 抓什么 | 状态 |
|---|---|---|---|
| ① | `ry += N` vs 前面那个缩放高度 | 相邻行压住 | 判红 |
| ② | 横向 `x` 步长 vs 缩放宽度 | 相邻控件压住 | 判红 |
| ③ | `矩形.y/x + 裸数字`（≥20） | 写死偏移 | 判红 |
| ④ | 循环/累积步长（`i * N`、`y += N`，≥20） | 写死行距/累积 | 判红 |
| ⑤ | `vcenter(..., 浮点 band)` / `+ 浮点偏移` | **行内文字排版** | 判红（本批） |

### CX.6 仍然看不见的（如实记）

* **音乐列表没有命中区向量** —— 它的**几何**（行矩形是否重叠）依然没人检查，
  只有"文字带高写死"这一条被 ⑤ 覆盖。要补得先给它加命中区（它是可点的）；
* **字体本身的度量**（比如某个字体的 advance 异常）—— 静态扫描管不到，
  只能靠实拍（附录 CV 就是这么发现的）。

### CX.7 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：**749 通过 / 0 失败**；
* `shot-diff.py verify` → 29 档与基线逐字节相同（基线**未改**）；
* `scale-audit.py --strict` → **五类都是 0**、退出码 0；
* `gates.py` → **19 项全过**；
* 实拍：`docs/证据/CX-150%-带高缩放.png`。

---

## 附录 CY：给音乐列表加命中区向量 —— 关掉一个记了两批的盲区（2026-10-05 批次 CY-1）

### CY.1 盲区是什么

附录 CW 与 CX 都记过同一件事：**音乐列表没有命中区向量**
（`Renderer` 里只有 `settings_hits` / `control_hits` / `sidebar_hits` / `crumb_hits` /
`window_lights` / `dock_icons`），所以 `layout-check.py` 的几何断言
**看不到它的行矩形** —— "行与行是否重叠"没人查。

### CY.2 做了什么

照 `sidebar_hits` 的现成模式，把 `music_hits: Vec<(Rect, usize)>` 接进 8 个地方：
渲染器字段 · `Renderer::new` 初始化 · 每帧清空 · 传给 `draw_window` ·
`draw_window` 形参 · 分发到 `draw_music_content` · 该函数形参 · 循环里登记行矩形；
再在 `--dump-layout` 里加一组 `music-rows`。

**刻意不做的事**：**不接点击行为**。加"点某行就播放"是另一个特性（需要播放器状态），
混进来会让这一批从"补取证"变成"加功能"。本批只让几何断言看得见它。

### CY.3 验证：100% 下基线**逐字节不变**

`shot-diff.py verify` → **`[ OK ] 29 档与基线逐字节相同`**。

这条证据正好说明它**是纯取证**：只往向量里登记矩形，不参与渲染，
所以 100% 的画面必须一个像素都不变 —— 实测一致。

### CY.4 现在看得见了（数据）

`--dump-layout` 的 `music-rows` 组：

| 档 | 行数 | 行高 | 相邻缝 |
|---|---|---|---|
| 100% | 11 | 42 | **4px** |
| 150% | 11 | 63 | 6px |

几何断言随之覆盖它（`check_desktop` 遍历所有组，自动包含 `music-rows`）。
顺带看到：同组重叠报告从 **79 处降到 15 处** —— 这期间的排版修复也清掉了不少。

### CY.5 这一批踩的坑（同一个错三次，值得记）

给一个**多参数函数**加形参时，**签名与调用点必须对齐**。我连着错了三次：

1. 字符串锚点 `sidebar_hits: &mut Vec<(Rect, usize)>,` **不唯一**
   （`draw_window` 与 `draw_files_content` 各有一处）；
2. 换成"两行锚点"仍然撞车（两个函数都同时收 `sidebar_hits` 与 `crumb_hits`）；
3. 改成按行索引插入后**插错了位置**（插在 `lights_out` 之后，而调用点把它放在
   `crumb_hits` 之后）→ 参数错位，`sidebar_hits` 落到 `crumb_hits` 上 →
   **E0308**（类型不同，编译器当场抓住）。

**教训**：改多参数签名时，先去**调用点**看实参顺序，再决定形参插在哪里 ——
顺序错了，同类型的参数还会**静默错位**（这次是运气好：相邻两个类型不同才报错）。

另外顺带记一条：这一批我又用了一次 ASCII 引号写 Python 字符串（**第 14 次**），
脚本当场语法错。规矩一直没变：**中文引号一律用「」**。

### CY.6 仍然没覆盖的

* 音乐行的**点击**行为（本批刻意不加）；
* **字体本身度量**异常（只能靠实拍，附录 CV 就是这么发现的）。

### CY.7 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：**749 通过 / 0 失败**；
* `shot-diff.py verify` → 29 档与基线逐字节相同（基线**未改**）；
* `layout-check.py --strict` → ① ② ⑤ 都是 0、退出码 0，且**已包含 `music-rows`**；
* `gates.py` → **19 项全过**。

---

## 附录 CZ：实拍 150% 各界面 —— 控制中心的溢出（2026-10-05 批次 CZ-1）

### CZ.1 换了个角度找问题

前面几批都是"静态扫描报什么就修什么"。这一批换成**实拍**：
把 150% 下的账户页 / 可移动设备页 / 控制中心 / 开始菜单 / 安装器 / 确认卡片都渲染出来看。

**为什么现在能这么做**：前几批建立了"渲染 → 转 PNG → 亲眼看"的能力（附录 CV/CW）。
台账里那句"剩余字面量需**观感判断** → 需有屏幕的环境"**已经不成立** ——
有屏幕的判断，我现在可以自己做。

### CZ.2 实拍发现（控制中心）

150% 下：**面板高度不够** ——

* 「注销」被屏幕**切掉**；
* 「Esc 或点击别处关闭」**压在「注销」上**；
* 「未检测到音频（预览模式）」与「虚拟桌面（4）」**叠在一起**。

### CZ.3 真因（三类裸数字）

| 类别 | 例子 |
|---|---|
| **面板尺寸** | `PANEL_H` 常量、`PANEL_W`、面板 x 位置 |
| **行内元素** | `row` 闭包的 `x/w/h = 4/8/34`；`knob` 的 `54/7/46/20`；文字偏移 `+32/+120`；分隔线 `-24`；圆点 `+6` |
| **步长常量** | `y += control_layout::ROW_H_ACTION`（32）· `ROW_H_SWITCH`（36）· `HEAD` · `SECTION` · `FOOTER` |

而行的**内容**会缩放（字号、开关）→ 150% 下内容长大、容器与步长不动 → 压住、溢出。

### CZ.4 修了什么

1. 行内元素与步长常量**全部接进缩放**（`theme::size::px(...)`）；
2. 面板高度**缩放后夹在屏幕可用高度内**（`min(px(PANEL_H), 屏幕高 - 顶部 - 8)`），
   保证**面板本身不再出屏**。

### CZ.5 **没解决**的（如实说）

就算面板夹在屏内，150% 下**内容总量仍然超过可用高度** ——
实拍可见底部「关机 / 重启 / 注销」与页脚「Esc 或点击别处关闭」**仍然挤在一起**。

**要真正装下，需要滚动或两栏布局** —— 那是**设计改动**，本轮**不做**。
所以本轮的效果是"**面板不出屏了、中段不再叠字**"，**不是**"150% 控制中心完全正常"。

### CZ.6 两个坑

1. **正则误伤常量声明**：`const PANEL_H: i32 = px(control_layout::PANEL_H);` →
   `px()` 不是 `const fn`，常量上下文里不能调 → **E0015**。回退那一行，
   缩放只放在**使用处**；
2. **又一次栽在缩进上**：4 处锚点因为我读到的是"去掉前导空格"的文本而落空 →
   改成**按行索引**替换（先按 strip 后内容找行号，再整行替换、保留原缩进），一次通过。

### CZ.7 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → **29 档与基线逐字节相同**（全部改动都是 `px(n)`，100% 下等价）；
* 实拍：`docs/证据/CZ-150%-控制中心-步长修复后.png`；
* `gates.py` → **19 项全过**。

---

## 附录 DA：实拍继续 —— 开始菜单与确认卡片；以及**我自己的过滤规则藏住了 bug**（2026-10-05 批次 DA-1）

### DA.1 实拍发现两处

| 位置 | 现象 | 真因 |
|---|---|---|
| **开始菜单**（150%） | 4 个条目里第 4 个「自由布局」**露在面板背景之外** | `h: items.len() as i32 * 30 + 8` —— 面板高度按**数量 × 裸数字**算，不随缩放 |
| **确认卡片**（150%） | 窗口高度由 `104 + args.len() * 22 + 54 + echo_block + 68` 算出，**四个字面量都没缩放** | 同上：数量乘 + 固定块 |

（顺带实拍确认：**安装器**与**确认卡片**的其余部分在 150% 下排版正常 —— 不是每处都有问题。）

### DA.2 两处都修好了

`items.len() as i32 * theme::size::px(30) + theme::size::px(8)`、
`theme::size::px(104) + args.len() as i32 * theme::size::px(22) + theme::size::px(54) + echo_block + theme::size::px(68)`。

`shot-diff.py verify` → **29 档与基线逐字节相同**（改动都是 `px(n)`，100% 下等价）。

### DA.3 **最重要的一条：我的过滤规则把真 bug 藏住了**

开始菜单那处，静态扫描**其实报过** —— 在附录 CL 里，它报的是
`let panel = Rect { … h: items.len() as i32 * 30 … }`。

**我当时把它当"数量乘、不是步长"的误报，加了一条规则整体跳过含 `.len()` 的行。**

结果：**那条规则把这一类真 bug 一起藏住了**，直到这一批实拍才发现。
本批把它改成**真判据**：

> `数量 * N`：**N 是裸数字** → 报（容器随数量线性变大却不随缩放变大）；
> N 已经走 `theme::size::px(...)` → 放过。

改完立刻又抓出一处：确认卡片的 `win_h`。**五类判据现在都是 0。**

### DA.4 这是同一个错误第二次出现

附录里已经记过一次同类事故：`shot-diff.py` 里那个 **`MASK_REGIONS`** ——
当年为了压掉截图噪声加了个遮罩区，结果**遮住的正是 caret 的不确定性 bug**。
（那次的结论是"遮罩在掩盖 bug，不是噪声"。）

**这次的形状完全一样**：为了让门禁不红而加的白名单/过滤，
**必须先证明被过滤掉的每一类都不可能是真问题**，否则它就是在藏东西。
两次都是"人眼看图"才发现的 —— 这条也说明**实拍不能省**。

### DA.5 仍然没解决的

150% 下**控制中心的底部**仍然挤（内容总量超过屏幕高度）—— 需要滚动或两栏，
是设计改动，见附录 CZ.5。

### DA.6 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → **29 档与基线逐字节相同**；
* `scale-audit.py --strict` → **五类都是 0**（含新判据）；
* 实拍：`docs/证据/DA-150%-开始菜单修复后.png`、`docs/证据/DA-150%-确认卡片.png`；
* `gates.py` → **19 项全过**。

---

## 附录 DB：把「150% 逐界面实拍」变成**可复查的清单**（2026-10-05 批次 DB-1）

### DB.1 问题：观感检查不可复查

前几批用实拍发现了控制中心与开始菜单的溢出（附录 CZ / DA）。但那件事的**覆盖面**
一直靠"我记着看过哪些"—— 这既不可复查，也不该是审计的做法。

### DB.2 两个改动

**① `shot-diff.py` 加子命令 `render-scale <pct> [outdir]`**
把**全部 29 档**（10 个固定形态 + 19 个设置页）在指定缩放下渲染成 PNG。
于是"看过哪些"变成一份**文件清单**，而不是记忆。

渲染产物是**工作产物**（29 张 ≈ 4 MB），放进 `docs/证据/_scale/` 并**加进 .gitignore**；
**清单与结论**（文字）提交进仓库 —— 图片本身没必要进版本库。

**② 空白检查并进 `layout-check.py`**
「这一档是不是空的」原先只在 `shot-diff.py verify` 里做，而那只跑 **100%**。
但 `layout-check.py` **本来就在** 17 页 × 3 档 + 9 个窗口形态 × 3 档上各渲染一次 ——
所以顺带读 `preview.bmp` 数内容区颜色，**零额外开销**，覆盖从 1 档扩到 **3 档**。

（对比：用 `render-scale` 再跑一遍要多花 2.5 分钟。能顺带做的就别重跑。）

### DB.3 这一批的实拍记录（如实：只有一部分是**人眼**看的）

**人眼在 150% 下逐张看过的**（9 个界面）：桌面/四宫格布局、外观页、账户页、
可移动设备页、控制中心、开始菜单、安装器、确认卡片。

**发现的真问题**（都在前两批修掉了）：控制中心面板溢出 · 开始菜单第 4 项露在面板外 ·
确认卡片窗口高度不缩放。

**其余 20 档**（layout2/3/4、desktop、page-1~8/10~14/16~18）**没有人眼逐张看过** ——
它们靠的是**机器判据**：几何断言（命中区越界/重叠）+ 空白检查 + 100% 像素基线。
**这条界限写清楚**：机器能保证"不越界、不是空的、100% 没变"，**不能**保证"150% 下好看"。

### DB.4 本批的两个失误

1. `db2.py` 插入的 helper **没有留在文件里**（原因未查明），而接线脚本已经用上了它 →
   运行时报 `NameError: dump_with_blank`。**补救**：写了一个**幂等**的补丁脚本
   （先查 `def content_colors(` 是否存在再插），并加自检断言三个函数都在；
2. 这是本会话第 N 次"改脚本改出问题"——**结论不变**：脚本改动要有**自检**，
   否则错误会留到下一次运行时才炸。

### DB.5 验证

* `python scripts/shot-diff.py render-scale 150` → **29/29 档**，内容色范围
  **782（desktop）~ 4001（controlcenter）**，无空白档；
* `python scripts/layout-check.py --strict` → ① ② ⑤ **0**、**⑦ 空白档 0**、退出码 0；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**。

---

## 附录 DC：给控制中心加**滚轮滚动** —— 以及一个我自己发现的裁剪缺口（2026-10-05 批次 DC-1）

### DC.1 背景

附录 CZ.5 记过：150% 下控制中心**内容总量超过屏幕高度**，底部「关机/重启/注销」与页脚挤在一起，
当时的结论是"要真正装下需要**滚动或两栏**，属设计改动，本轮不做"。

这一批把它做了 —— 因为查下来**机制是现成的**：

* `input.rs` 已把 `REL_WHEEL` 翻译成 `UiEvent::Wheel { dy }`；
* `main.rs` 已有 `widgets::scroll_apply` 在驱动 `desktop.scroll`（原先只服务文件网格）；
* `control_open` / `control_panel` 都在 `Renderer` 里，而事件循环中 `renderer` 与 `desktop` 同作用域。

### DC.2 做法

1. `Renderer` 加 `control_scroll: i32`（像素偏移）与 `control_content_h: i32`（内容高度）；
2. 绘制时内容从 `panel.y + TIGHT - control_scroll` 起，画完把**实际内容高度**记下来
   （页脚是**钉在面板底**的，不参与滚动）；
3. 滚轮：**控制中心开着时优先滚它**（它是覆盖层），夹在 `0 ..= max(0, 内容高 - 面板高)`；
   否则走原来的文件网格；
4. 关掉时滚动量归零（下次打开不会停在上次位置）。

### DC.3 可观测性：加了 `--control-scroll N`

滚轮事件在 `--shot` 里**发不出来** —— 没有这个走查参数，我就**没法把滚动后的状态渲出来看**。
它与既有的 `--scale` 是同一类做法：只为可观测，**不写设置**。

### DC.4 验证

* `shot-diff.py verify` → **29 档与基线逐字节相同**（100% 下内容本来就装得下 → 滚动量恒为 0）；
* 实拍 `--control-scroll 400`（150%）→ 滚到底后「关机 / 重启 / 注销」**都完整可见**，
  页脚干净 —— 附录 CZ.5 那个缺口解决了；
* 证据：`docs/证据/DC-150%-控制中心-滚到底.png`。

### DC.5 ⚠️ 我自己发现的**新缺口**（如实记，本批没修）

实拍里能看到：滚动后**顶部内容没有被裁掉** —— 第一项「通知」画到了面板**外面**、
压到菜单栏上。

原因：面板背景在内容**之前**画，而 `tr.draw(...)` 不裁剪；滚动让内容超出面板上边界后
就画到外面去了。**要修就得按面板裁剪内容**（逐行可见性判断，或给文本绘制加裁剪区）。

**本批没做**：预算用尽，而且这属于"新加的滚动功能自身还不完整"，
比"补一个已有功能的边角"更该单独一批做完、单独验证。

### DC.6 另一个坑

`--control-scroll` 的插入锚点（`if let Some(pos) = args.iter().position(|a| a == "--settings-page")`）
因为我按 12 空格缩进去匹配而落空（实际是 8 空格）→ 改**按行索引**插入，一次通过。
**这是本会话第 N 次栽在"我读到的是去缩进文本"上** —— 已经形成条件反射：**锚点落空就立刻换行索引**。

### DC.7 门禁当场抓到 host 看不见的 bug（本批最有说服力的一条）

第一次提交前跑门禁：**1/19 失败** —— `cargo check --target musl（6/7 crate）`。

原因：滚轮处理器是 **linux 专属**（`#[cfg(target_os = "linux")]` 一带），
**Windows 上那段代码根本不编译** —— 所以 host 构建 0 错误，
而 musl 检查报 `cannot multiply i32 by usize`（`dy` 是 i32、`WHEEL_LINES` 是 usize）。

修法：`widgets::WHEEL_LINES as i32`。修完 6/6 crate 全过。

**这正是"musl 检查"这一步存在的理由**：它看得到 host 看不到的代码。
如果只有"本机 cargo build 绿"这一条，这个 bug 会一路进到真机。
（门禁挡住了提交 —— 这是本会话第 N 次"不绿不提交"救回一次。）

### DC.8 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → **29 档与基线逐字节相同**；
* `layout-check.py --strict` / `scale-audit.py --strict` → 全 0；
* `gates.py` → **19 项全过**。

---

## 附录 DD：控制中心滚动后的**裁剪缺口** —— 两次**可证明无效**的尝试，第三次才对（2026-10-05 批次 DD-1）

### DD.1 缺口（上一批自己发现的）

附录 DC.5 记过：滚动后内容越过面板上边界，第一项画到了菜单栏上。
原因：面板背景在内容**之前**画，而 `tr.draw(...)` 只按**屏幕**边界检查。

### DD.2 尝试一：重画菜单栏 —— **实测无效**

想法：面板从 `MENUBAR_H + 6` 开始，溢出的内容正好落在菜单栏那一条里，
`draw_menubar` 是幂等的，滚过之后再画一遍就能盖住。

**结果：渲染图与改前逐字节相同（同一 sha256）。**
原因看实拍就明白：**菜单栏没有不透明背景** —— 壁纸直接透出来，
所以"重画菜单栏"只是把同样的半透明内容又画一遍，**盖不住任何东西**。

### DD.3 尝试二：`row` 闭包把不可见的行挪到屏幕外 —— **同样实测无效**

想法：让 `row` 闭包对完全在面板之外的行返回 `y = -1000`，
渲染器本来就会跳过屏外像素，于是文字不画、命中区也失效（顺带修掉"看不见的行还能点"）。

**结果：渲染图**又一次**与改前逐字节相同。**
原因：溢出的那一项「通知」是**分组标题**，它直接 `tr.draw(...)`，**根本不走那个闭包**。

（这一版的"命中区失效"本身是对的、值得留着 —— 但它解决不了标题。）

### DD.4 尝试三（对）：把裁剪做在 `TextRenderer::render` 里

**所有文字都经过 `render()`** —— 所以裁剪做在这一处就够，一处管全部：

* `TextRenderer` 加 `clip_y: Cell<Option<(i32, i32)>>`（绘制入口拿的是 `&TextRenderer`，所以用 `Cell`）；
* 像素循环里加一句：超出区间就 `continue`；
* 控制中心绘制期间设上 `(panel.y, panel.y + panel.h)`，画完清掉。

**实拍确认：**「通知」从面板上方**消失**，内容干净地裁在面板内。

### DD.5 两次失败都是**当场被证明**的，不是"感觉没用"

这两次的价值在于：**渲染图的 sha256 与改前完全相同** —— 这不是"看起来差不多"，
而是"一个像素都没变"，所以"无效"是**结论**而不是猜测。
第三次的效果同样是 sha256 **变了**才认账。

这条做法值得写下来：**改渲染效果时，先比 sha256，再看图**。

### DD.6 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → **29 档与基线逐字节相同**（裁剪只在控制中心内容绘制期间生效，
  100% 下内容本来就装得下 → 画面不变）；
* musl 逐 crate 6/6 通过；
* 实拍：`docs/证据/DD-150%-控制中心-裁剪后.png`；
* `gates.py` → **19 项全过**。

---

## 附录 DE：把"只报告"的重叠逐条查清 —— 14 → 0，并把 ⑥ 升级为判红（2026-10-05 批次 DE-1）

### DE.1 动机

几何断言里 ⑥「窗口类界面同组重叠」一直**只报告不判红**（附录 CL 定的：判据不够硬）。
但附录 CM 的经验是：**那批"2px 细缝"里就藏着真 bug**（TzShift 步长 36 < 宽度 40）。
所以这一批把 14 条逐条查清。

### DE.2 查到什么：14 条全是同一个真因

报出来的是 `OpenSettings ∩ OpenTerminal = 512px²`、`PowerOff ∩ Reboot`、`Reboot ∩ Logout` 等，
面积 **512 = 256 × 2**（125% 下 762 ≈ ×3）→ 就是"行矩形之间压了 2~3 像素"。

真因：控制中心的**行闭包写死 `h = 34`**，而**动作行的步长**是
`control_layout::ROW_H_ACTION = 32` → **步长 < 行高** → 相邻行必然重叠（100% 下就重叠）。
开关行步长是 `ROW_H_SWITCH = 36` > 34，所以没问题 —— 这解释了为什么报出来的**全是动作行**。

**修法**：行高改用**步长常量本身**（`ROW_H_ACTION` = 32）→
"步长 ≥ 行高"对所有行成立（动作行 32 = 32，开关行 36 > 32 留 4px 缝）。
**结果：14 → 1。**

### DE.3 剩下那 1 条，是**我自己的裁剪手法**造成的误报

`controlcenter@150% Reboot ∩ Logout = 12096px²` —— 面积大得反常。取数据一看：
两者**都在 `y = -1000`** —— 那是附录 DD 的裁剪手法（把完全在面板之外的行挪到屏幕外），
**多行共用同一个屏幕外坐标**，于是被判成"重叠 48px"。

**这不是布局重叠**：屏幕外的矩形根本不会被画出来。修判据：**跳过屏幕外矩形**。
**结果：⑥ = 0**，于是按本项目惯例（先报告、确认无误再判红）把它**升级为判红**。

### DE.4 预测与验证

改行高时我预测"**只有 controlcenter 一档会变**"（修复只动控制中心）。
实测：`1 档与基线不同：['controlcenter']` ✓ —— 与预测一致，所以重新标定基线是有依据的。

### DE.5 ⚠️ 本批最重要的教训：**我自己的脚本静默失败了两次**

为了改 `layout-check.py`，我写了两版补丁脚本，**两版都打印了"✓ 已修改"，而文件其实没变**。
原因：我用的是 `t.replace(old, new)` —— **没有断言**，
而"✓"是**无条件打印**的。于是"没匹配上"和"改成功了"在输出里长得一模一样。

（附录 DB 里 `db2.py` 的 helper 神秘消失，是**同一个毛病**的另一次发作。）

**第三版改成：每处替换都 `assert`，失败就在写盘前中止，并把改后的行打印出来。** 一次通过。

**规则**：脚本里的"改文件"动作必须**断言命中**，否则它就是一个会撒谎的脚本。

### DE.6 现在这套几何断言的判红项

① 横向越出屏幕 · ② 左右越出设置窗口 · ⑤ 窗口类界面横向越界 · ⑥ **同组重叠（本批升级）** ·
⑦ **空白档**（附录 DB 加的）。全部为 0。

### DE.7 验证

* `layout-check.py --strict` → ① ② ⑤ ⑥ ⑦ **全 0**、退出码 0；
* `shot-diff.py verify` → 基线已重新标定，**29 档逐字节相同**；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**。

---

## 附录 DF：实拍补看"定制布局"；以及一处**设计问题**（不是我该擅自改的）（2026-10-05 批次 DF-2）

### DF.1 为什么补看这几个

设置页用的是**共享的** `widgets::row` / `tr.vcenter` 助手（本会话已修过），
而控制中心与开始菜单的真 bug 都出在**定制排版**里。
所以收口前补看**定制排版**的界面：平铺布局 2/3/4 与纯桌面。

### DF.2 我实际看了什么（如实）

* **布局 4（独占/堆叠）150% 与 100%** —— 逐张看过；
* **布局 2（两列）100%** —— 看过（但**没**看 150% 版）；
* 布局 3、纯桌面 —— **只渲染了，没有人眼看**。

### DF.3 看到一处：最大化窗口里**桌面图标透出来**

布局 4（音乐窗口最大化）里，左侧的桌面图标（文件/终端/设置）**透过窗口可见**。

**关键判断：这不是缩放引起的。** 100% 与 150% 两张图**一模一样**地有这个现象。
真因是**窗口背景是半透明的**（`surface_2` 0.5 alpha）+ 桌面图标画在窗口**下面**（层叠关系正确）。

**我没有改它**，理由：改 alpha 或改层叠会动到**整套视觉风格**，
而且"半透明窗口下图标透出"是**设计取舍**（毛玻璃观感 vs 图标该被遮住），不是实现缺陷。
**这是要你定的事**，不是我自己拍板的 —— 如实记在这里。

### DF.4 这一批没有改任何代码

本附录只是**记录实拍结论**与**一个待你决定的设计问题**。
（代码改动是上一批 DF-1：§2.4 台账更新 + 报告方法学一节。）

### DF.5 实拍覆盖率的现状（收口前）

| 类别 | 人眼逐张看过 | 机器覆盖 |
|---|---|---|
| 设置页（19 档） | 外观 · 账户 · 可移动设备（3/19） | 几何 + 空白 × 3 档、像素基线 |
| 窗口类形态（9 档） | 桌面(含窗口) · 控制中心 · 开始菜单 · 安装器 · 确认卡片 · 布局2 · 布局4（7/9） | 同上 |
| 缩放特例 | 150% 的上述界面 | `render-scale` 可一键重渲全部 29 档 |

**没看过的**：16 个设置页、布局3、纯桌面 —— 它们由机器判据覆盖
（不越界、不重叠、不空白、100% 像素不变），**但没有经过人眼**。这条界限写清楚。

---

## 附录 DG：设置页实拍续 —— 找到一处**已定位但故意没修**的挤压（2026-10-05 批次 DG-1）

### DG.1 补看内容最密的设置页

按附录 DF.5 的清单，收口前补看内容密的设置页（150%）。
本轮渲了第 6（网络）· 10（权限与隐私）· 12（快照与回滚）· 13（系统更新）四页，
**人眼看了第 6 页**（其余三张只渲了，没看 —— 如实记）。

### DG.2 看到的：网络页状态行与第一行挤在一起

150% 下「**未发现网络接口**」（状态行）与紧随其后的「**接口 （无）**」几乎贴在一起。

### DG.3 定位（精确到行）

* `aether-compositor/src/draw.rs:4370` 画状态行，随后 **4371：`ry += row_h;`**；
* 而 `row_h` 的定义在 **3462**：
  `let row_h = (((r.h - chrome).max(0)) / PAGES.len().max(1) as i32).clamp(20, 30);`
  —— 那是**设置侧栏**（左侧页列表）的行高，**上限 30 且不随缩放变化**；
* 于是 150% 下：状态行文字变大了，**行距没变** → 挤在一起。

**性质**：与附录 CS/CT 修掉的那批是同一类（步长不随缩放），
但这次是**"借用了侧栏的变量"**，静态扫描看不见（它只认裸数字，不认变量里的值）——
**这是静态判据的一个已知盲区**。

### DG.4 **我没有修它**，以及为什么

要改就得先回答"**这个 `row_h` 到底该是内容行高还是侧栏行高**"——
它在同一函数里被侧栏和内容**共用**。改错会波及所有用到它的页面，
而本轮预算已经用在定位上。

**按本项目的规矩：宁可如实记下"定位了、没修"，也不要猜一个值改下去。**
下一批的第一件事：查清 `row_h` 的全部使用点，再决定是"内容行高单独走 `px()`"还是"整页统一"。

### DG.5 本轮其他

* 实拍覆盖率：人眼看过的界面 **13 → 14**（新增网络页）；未看的设置页 **16 → 15**（另三张只渲未看）；
* 四张证据：`docs/证据/DG-150%-页6/10/12/13.png`。

### DG.6 验证

* 构建 0 错误 0 警告（本批**没有改代码**）；`shot-diff.py verify` → 29 档逐字节相同；
* `gates.py` → **19 项全过**。

---

## 附录 DH：修掉网络页那处挤压；并把"静态判据的盲区"变成判据（含**判据自身的验证**）（2026-10-05 批次 DH-1）

### DH.1 先查证，再动手

附录 DG 只做到"定位了、没修"。本批第一步是查清 `row_h` 在
`draw_settings_content`（3394..5096）里的**全部使用点**：

| 行 | 用途 | 判定 |
|---|---|---|
| 3462 | 定义：`(((r.h - chrome)/PAGES.len()).clamp(20, 30))` | 侧栏按高度摊出来的行高 |
| 3475 / 3478 | 侧栏行矩形 | 侧栏自用，正确 |
| 3750 / 4147 / 4216 / 4291 | `row_hint(...)` —— **方法名**，不是这个变量 | 无关 |
| **4371** | **`ry += row_h;`（网络页内容）** | **唯一误用** |

**结论：内容侧只有一处** → 修复是局部的、安全的。

### DH.2 修法：与下面各行同一个节奏

网络页的内容行（4416）用的是 `ry += theme::size::px(theme::size::LINE_GAP);`。
把状态行那一步也改成同一个常量，整页节奏一致。

**验证（预测 → 实测）**：改前我预测"**只有 page-6 一档会变**"。
实测 `1 档与基线不同：['page-6']` ✓ —— 与预测一致，所以重新标定基线有依据。
150% 实拍：状态行与「接口（无）」等各行**全部分开**。

（侧栏那个 `clamp(20, 30)` **保持不缩放是对的** —— 19 个页面要装进窗口高度，
它是"按空间摊"的，缩放反而会溢出。这条写进注释。）

### DH.3 把盲区变成判据：⑥ 类

静态扫描原先只认**裸数字**，看不见"变量里装的不缩放值" —— 网络页就是这么漏过去的。
新增 ⑥ 类：对每条 `y += <标识符>;`，回退到**所在顶层函数开头**，
在函数范围内找 `let <标识符> = ...`，若定义里**有 `clamp(` 且没有 `theme::size::px(`** → 报。

### DH.4 ⚠️ 这条判据的第一版**从不触发**，我把它验证出来了

第一版我限制了 **120 行**的搜索窗口 —— 而 `row_h` 的定义在 **909 行之外**。
于是它对**修复前**的代码也报 **0 处**。

**我是靠"拿修复前的版本跑同一逻辑"发现这一点的**：
如果只看"当前代码报 0 处"，会误以为"代码很干净"，
而真相是**判据根本没在工作**。

**一个从不触发的判据等于没有判据。** 改成按函数作用域搜索后复验：

* 修复前版本：命中 **1 处**（行 4371 `+= row_h`，定义在 3462）✓
* 当前版本：**0 处** ✓

### DH.5 验证

* `shot-diff.py verify` → 基线已重新标定（只有 `page-6` 变），**29 档逐字节相同**；
* `scale-audit.py --strict` → 原五类 **0**；⑥ 类 **0**（且已验证它对旧代码会报 1）；
* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**；
* 实拍：`docs/证据/DH-150%-网络页修复后.png`。

---

## 附录 DI：长句横穿窗口 —— 以及我在这条路上翻的四次车（2026-10-05 批次 DI-1）

### DI.1 现象（实拍发现）

150% 权限与隐私页：最后一行「等级：L0 只读 / …（下一步）。」**一直画到音乐窗口上**，横穿出设置窗口。
几何断言 ② 是 0 —— 因为它查**命中区矩形**，看不见文字（与附录 DG 同一个盲区）。

### DI.2 修复分三步

1. **`TextRenderer::fit()`**：按 `measure()` 量出的宽度截断并补 `…`。
   （项目原有的"截 N 个字符"写法**不随缩放** —— 150% 下照样溢出。）
2. 截断之后信息丢了（只剩「等级：L0 只读 / L1 需确认 / L…」），所以升级成
   **`TextRenderer::wrap()`**：按宽度**折行**（末行才截断）。该行是页面最后一行，折行不会顶掉别的内容。
3. 折行的第三行仍跑到窗口下沿之外 —— 因为**设置正文本来就没被裁剪**。
   用附录 DD 加的 `clip_y` 机制把正文**裁到窗口矩形**，看不见的部分由页面滚动揭示。

### DI.3 四次翻车（都记下来）

| # | 翻车 | 怎么发现的 | 教训 |
|---|---|---|---|
| 1 | 裁剪区间设成 `content`（可滚动页面的 `content` 是**完整内容高度**）→ **空操作** | 渲染图与改前 **sha256 完全相同** | **先比 sha256 再看图**：图没变 = 改动无效（附录 DD 的方法学） |
| 2 | 补丁脚本打印了「✓ 已设裁剪」，而**文件根本没变** | 后续 `grep set_clip_y` 找不到它 | 脚本把多处修改 + 一次 `write_text` 放一起，**第二处崩了第一处也没写**。**改文件的脚本要么单点、要么写完立刻自检**（附录 DE 同族） |
| 3 | 裁剪**泄漏**：菜单栏 / AI 气泡 / AI 输入框 / Dock 全被裁掉 | **看图** | 裁剪必须在正文画完**立刻**清掉；每帧开头那次清除只能兜底 |
| 4 | 把清除插在 **match 表达式内部** → 语法错；改到 match 之后又忘了外层 `tr` 是 `Option` | 编译器 | 位置类改动要看**语法结构**，不能只看行号 |

### DI.4 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify`：**所有设置页都变了**（裁剪对每页生效，属有意修复）→ 已重新标定基线；
* 实拍：`docs/证据/DI-150%-权限页裁剪后.png` —— 折行文字收在窗口内，
  且菜单栏 / AI 气泡 / AI 输入框 / Dock **都完好**；
* `gates.py` → **19 项全过**。

---

## 附录 DJ：跨帧状态专项审计（上一批翻车 4 次之后的复盘）+ 把"裁剪配对"变成判据（2026-10-05 批次 DJ-1）

### DJ.1 为什么要审这个

上一批（附录 DI）在同一个改动里翻了 **4 次车**，其中两次属于同一类：
**给渲染器设了一个跨帧存活的状态，忘了清**（裁剪区间泄漏，把菜单栏/Dock 一起裁掉）。
所以本批专门审这一类。

### DJ.2 审计方法

1. 列出 `Renderer` 的**全部 48 个字段**；
2. 找出 `render_frame` 里**每帧被重置**的：只有 5 个
   （`bg_dirty` / `bg_row` / `control_scroll` / `frames_since_bg` / `shadow_key`）；
3. 对剩下的**逐个判断**：它是"帧内临时状态"（必须每帧清）还是"用户状态"（必须跨帧存活）。

### DJ.3 结论：**没有泄漏**（逐项）

| 字段 | 判定 | 依据 |
|---|---|---|
| `settings_hits` / `setup_hits` / `palette_hits` | ✅ 每帧 `clear()` | draw.rs 的 2523 / 2585 / 2600 |
| `control_scroll` | ✅ 关闭归零 + 绘制时夹取 | 两处都在，且只在 `control_open` 时被读 |
| `control_content_h` | ✅ 每帧重测 | 只在绘制控制中心时写；只在 `control_open` 时读 |
| `control_panel` / `status_cluster` | ✅ 绘制时重算 | 只在 `control_open` 时被读，陈旧值不会被用到 |
| `audit_query` / `audit_editing` / `space_count` / `removable_*` / `notifs` / `settings` / `dns` / `pass_form` | ✅ **应当**跨帧存活 | 那是用户状态，不是帧内状态 |

**也就是说：这一类问题在现有代码里没有第二处**（除了我上一批自己引入的那一处，已修）。

### DJ.4 把"配对"变成判据（⑦ 类）

**"靠人记住每处 set 都要配 clear"正是我翻车的原因**，所以加进 `scale-audit.py`：
文件内 `set_clip_y(Some(...))` 的处数**不得多于** `set_clip_y(None)` 的处数。当前 **0 个文件报警**。

**判据的强度（如实）**：它是**文件级计数** ——
能抓"新加了一处 set 却完全没有 clear"，**不能**证明每处 set 都在正确位置被清掉。
后半句只能靠实拍/看图（上一批正是看图才发现泄漏的）。

### DJ.5 判据自身的验证（延续附录 DH 的规矩）

一个从不触发的判据等于没有判据。用合成输入验证：

| 输入 | set / clear | 结果 |
|---|---|---|
| 配对正确 | 1 / 1 | 通过 ✓ |
| 漏清（应报） | 2 / 1 | **报警** ✓ |
| 完全没清（应报） | 1 / 0 | **报警** ✓ |
| 只有清没有设 | 0 / 1 | 通过 ✓ |

### DJ.6 沉淀成规则

> **给渲染器设一个跨帧状态，必须在它"作用域结束"处清掉；能放在同一函数里就别跨函数。**

（附录 DI 那次泄漏就是"在函数里设、指望别处清"。）

### DJ.7 验证

* `scale-audit.py --strict` → 原五类 **0**；⑥ 类 **0**；⑦ 类 **0 个文件**（且逻辑已用合成输入验证）；
* 构建 0 错误 0 警告（本批**没有改产品代码**）；`shot-diff.py verify` → 29 档逐字节相同；
* `gates.py` → **19 项全过**。

---

## 附录 DK：把纵向裁剪升级为**矩形裁剪** —— 一处管住所有文字外溢（2026-10-05 批次 DK-1）

### DK.1 怎么发现的：**改完影响所有页面的东西，回头复看一个之前看过的页面**

附录 DI 给设置正文加了裁剪，但那是**只管纵向**的 `clip_y`。
本批复查账户页（裁剪前看过）时，立刻看到：
「旧口令先在本机校验（证明是你本人），新口令再交给 aetherd 写入」**横向穿出窗口**，
一直画到音乐窗口上。

**根因**：我在权限页那一行用了 `fit()`/`wrap()`，但**其余长句照旧**。
裁剪又只挡纵向，所以横向外溢一处都没被挡住。

### DK.2 为什么不逐行加截断

长句有几十处，而且**以后新写的句子还会忘**。
所有文字都经过 `TextRenderer::render()` —— 所以把裁剪做成**矩形**，一处管住全部：

* `clip_y: Cell<Option<(i32, i32)>>` → `clip: Cell<Option<(i32, i32, i32, i32)>>`（x0,y0,x1,y1）；
* `set_clip_y` → `set_clip`；像素循环里同时判 x 与 y；
* 设值点两处：控制中心（面板矩形）、设置正文（**窗口**矩形）。

### DK.3 验证

* 构建 0 错误 0 警告；
* `scale-audit.py` ⑦ 类（裁剪配对）仍为 **0 个文件** —— 改名后 set/clear 仍然配得上；
* `shot-diff.py verify`：设置页变了（裁剪范围变大，属有意修复）→ 已重新标定基线；
* 实拍：`docs/证据/DK-150%-账户页矩形裁剪.png` —— 长句**切在窗口右沿**，
  且菜单栏 / AI 气泡 / AI 输入框 / Dock **都完好**。

### DK.4 沉淀成两条规则

1. **改"影响所有页面"的东西（裁剪 / 字体 / 主题 / 缩放）之后，必须回头复看至少一个之前看过的页面。**
   本批正是这么做才立刻发现横向外溢 —— 如果只看新页面，会以为已经修好了。
2. **能在"所有绘制都经过的那一处"解决问题，就不要逐个调用点打补丁。**
   （裁剪做在 `render()` 里，比给几十处长句加截断更稳，也不会被以后的新代码绕过。）

---

## 附录 DL：开一条**从没查过的轴** —— 英文界面实拍（2026-10-05 批次 DL-1）

### DL.1 为什么想到这条轴

界面支持中英双语（所有文案走 `pick(lang, zh, en)`），而**英文文案宽度不同**。
更关键的是：**现有的机器判据全是中文跑出来的** —— 几何断言、空白检查、像素基线，
没有一条覆盖英文。**英文专属的布局/翻译问题对它们完全不可见。**

### DL.2 加了一个走查参数 `--lang en|zh`

与 `--scale` 同类：**只为可观测，不写设置文件**。
验证：加完之后中文 29 档**逐字节不变**（说明它不影响默认路径）。

（已知小瑕疵：外观页的语言开关标签读的是 `settings.locale`，所以走查英文时它仍显示「中文」。
产品路径下语言由设置驱动，不受影响。）

### DL.3 实拍发现：**英文界面只翻译了一半**

同一张图里同时出现英文与中文：

| 位置 | 英文模式下显示 |
|---|---|
| 菜单栏 | `File / Edit / View / Help` ✅ |
| 文件窗口标题 | `aether` ✅ |
| 文件侧栏**第一项** | `Parent folder` ✅ |
| 文件侧栏**第二项** | **「主目录」** ❌（同一栏里中英混着） |
| 文件窗口状态条 | `7 items` ✅ |
| 终端窗口标题 | **「终端」** ❌ |
| 终端底部提示 | **「开发机预览：PTY 仅在 Linux 上可用」** ❌ |
| 音乐窗口标题 / 面板 | **「音乐」** / **「正在播放」** ❌ |
| 右上角状态 | **「本地模型在线」** ❌ |
| Dock 标签 | **「文件」「终端」「设置」** ❌ |
| AI 气泡 / 输入占位 | **「好的，已把窗口排成两列。」** / **「把窗口排成两列」** ❌ |

### DL.4 修掉其中一处（最典型的一处）

`AiStatus` **早就写好了 `label_en()`**（draw.rs:1484，注释写着「§3.5 i18n」），
**但调用处（2816）只用了 `label()`** —— 也就是"翻译写好了没人用"。
改成 `i18n::pick(lang, label(), label_en())`：一行改动，不新增翻译。

**验证**：英文实拍右上角变成 `Local` ✅；中文 29 档**逐字节不变** ✅
（说明改动只影响英文路径 —— 一个干净的局部修复）。

### DL.5 ⚠️ 一条**失败的量法**（如实记）

我想用启发式量规模：「含中文、且不在 `pick(` 行上的字面量」→ 得到 draw.rs **234 处**、
main.rs **495 处**。**这个数字是错的** —— 它把断言消息、`tty_log` 日志、注释、颜色注释全算进去了。

**结论：这个问题的可靠证据只有实拍。** 所以本附录给的是**渲染图 + 逐项清单**，
不是那个数字。（这也再次说明：启发式判据要验证，不然会给出假的安全感。）

### DL.6 下一步（清单已备好）

按 DL.3 的表逐项走 `pick()`：Dock 标签 · 窗口标题（终端/音乐）· 音乐面板「正在播放」·
终端底部提示 · 文件侧栏「主目录」· AI 输入占位。**AI 气泡那句来自 shot 夹具的预置回复**，
不是 UI 文案（真机回复由 aetherd 决定），修的时候要分开对待。

### DL.7 验证

* 构建 0 错误 0 警告；`shot-diff.py verify` → **中文 29 档逐字节相同**；
* `cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**；
* 实拍：`docs/证据/DL-EN-桌面.png`（修前）、`docs/证据/DL-EN-状态栏已修.png`（修后）。

---

## 附录 DM：英文清单逐项修完（窗口标题 / Dock / 侧栏 / 终端 / 音乐）（2026-10-05 批次 DM-1）

### DM.1 沿用模块自己的范式

`text::strings` 里 **早就有**这个写法：`MENUS` / `MENUS_EN` 两张表 + `menu_label()`
——「**取不到英文就退回中文**（宁可显示中文，也不要空白或 panic）」。
窗口标题这一组（`WIN_FILES` / `WIN_TERM` / `WIN_BROWSER` / `WIN_MUSIC` / `WIN_SETTINGS` / `INSTALLER`）
**只有中文**，所以照抄一份：加 `*_EN` 常量 + `win_label(zh)` 访问函数。

### DM.2 先查出一个**陷阱**，才动手

`draw.rs:6288` 用 `*name == strings::INSTALLER` **比较** Dock 列表里的名字。
如果我把**列表本身**翻译了，这个比较就永远不成立（安装图标的琥珀色/悬停判定会失效）。
所以：**列表保持中文，翻译只发生在"显示"这一步** ——
只改**标签查找**（draw.rs 6219-6221）与**窗口标题**（main.rs 2162-2164）。

### DM.3 两批共修 8 处

| 位置 | 修前 | 修后 |
|---|---|---|
| Dock 标签 ×3 | 文件 / 终端 / 设置 | `Files` / `Terminal` / `Settings` |
| 窗口标题 ×3 | 终端 / 音乐 / 文件 | `Terminal` / `Music` / `Files` |
| 文件侧栏 | 主目录（同栏第一项却是 `Parent folder`） | `Home` |
| 终端底部提示 | 开发机预览：PTY 仅在 Linux 上可用 | `Dev preview: PTY is only available on Linux` |
| 音乐面板 | 正在播放 | `Now playing` |

### DM.4 一处容易只改一半的地方

`NOW_PLAYING` 有**两个**使用点：一个**画**（5218），一个**量宽度**（5213，
`tr.measure(...)`，用于居中）。只改画的那处，会让**标签宽度与实际文字不一致** → 居中偏移。
两处一起改才对。

### DM.5 验证

* 构建 0 错误 0 警告；`shot-diff.py verify` → **中文 29 档逐字节不变**（两批都是纯英文路径改动）；
* `cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**；
* 实拍：`docs/证据/DM-EN-标题与Dock.png`、`docs/证据/DM-EN-第二批.png`。

### DM.6 剩下的**不是 UI 文案**（如实区分）

* AI 气泡那句「好的，已把窗口排成两列。」与输入框里的「把窗口排成两列」——
  来自 **shot 夹具的预演内容**（真实回复由 aetherd 决定），不是界面文案；
* 文件名（文档 / 图片 / 预算.csv）与曲名（以太漂移…）是**演示数据**；
* **未查**：设置页在英文下的表现（本批只覆盖桌面这一屏）—— 留给下一批。

---

## 附录 DN：英文轴覆盖到设置页 —— 又踩了一次**自己刚识别出的陷阱**（2026-10-05 批次 DN-1）

### DN.1 实拍发现：标题没翻译，侧栏翻译了

英文模式下的外观页：**侧栏**显示 `Appearance` ✅，而**页面大标题**仍是「外观」❌ —— 同一屏中英不一致。

真因是"翻译写好了没人用"的又一处：

* 侧栏（draw.rs:3503）用 `p.title_in(lang)` ✅；
* 大标题（3525）用 `p.title` ❌（只有中文）。

### DN.2 ⚠️ 我第一版把它改坏了 —— 而且踩的是**上一批自己识别出的陷阱**

我直接把 `title` 换成 `title_in(lang)`。结果：**整页内容消失**（标题成了 `Appearance`，下面一行都没有）。

原因：`title` 有**两个**用途 —— ① 画大标题；② 下面的 `match title { "权限与隐私" => … }`
**分发页面内容**。翻译了它，match 永远不命中。

**这正是 DM-1 里我在 `*name == strings::INSTALLER` 上识别过的陷阱**：
「被比较/被匹配的字符串不能翻译」。**我识别了它，然后在下一批自己踩了进去。**

→ 教训要落到**动作**上，而不只是记下来：
> **翻译一个变量之前，先列出它的每一个使用点（包括 match / == / 反查）。**

修法：`title_zh`（中文，供 match 分发）+ `title`（当前语言，仅供显示）。

### DN.3 顺带修掉设置窗口标题栏

英文实拍里还看到：**设置窗口的标题栏**仍是「设置」。
来源是 `open_app` 里的 `title: APP_TITLES[icon].to_string()`。

**这次先查了使用点**：`APP_TITLES` 在 3311 行被用于**反查图标索引**（`*t == app`）——
所以**列表本身保持中文**，只翻译**显示**那一处。
（写守卫时我第一版守卫写得太严，把"列表被比较"当成"不能改显示"而拦住了自己；
守卫应该检查的是"**有没有改到列表本身**"。）

### DN.4 英文专属的新发现（本批没修，留给下一批）

英文文案更长 → 设置页**多行被窗口右沿裁掉**：`Privacy & Permis…`、`Writes to /var/log/aether/ and sho…`、
`Search: tool / args / verdict (d…`。

**这与中文下的情况不同**：中文大多放得下。裁掉的部分**纵向滚动也看不到**（裁的是横向），
用户只能**把窗口拉宽**才能看到 —— 属于"可恢复但不理想"。

**下一步**：给设置正文的长句接上 `wrap()`（DI 已经写好了这个函数），
或者让内容区支持横向滚动。**本轮不改**（改动面大，且当前 100% 中文基线不能动）。

### DN.5 验证

* 构建 0 错误 0 警告；`shot-diff.py verify` → **中文 29 档逐字节不变**（全部是英文路径改动）；
* `cargo test --workspace`：见提交记录；`gates.py` → **19 项全过**；
* 实拍：`docs/证据/DN-EN-页0-标题已修.png`（外观页）、`docs/证据/DN-EN-页10-全英文.png`（权限页）。

---

## 附录 DO：**把自动折行实现出来，然后否掉它**（2026-10-05 批次 DO-1）

### DO.1 要修什么

附录 DN.4：英文文案更长，设置页**多行被窗口右沿裁掉**（`Privacy & Permis…`、
`Writes to /var/log/aether/ and sho…`）—— 裁掉的部分纵向滚动也看不到。

### DO.2 诱人的做法（按 DK 那条规则）

DK.4 沉淀过一条规则：「**能在"所有绘制都经过的那一处"解决问题，就不要逐个调用点打补丁**」。
照这条，最自然的做法是：在 `TextRenderer::draw` / `draw_bold` 这一层做**超宽自动折行** ——
一处改动，所有页面受益。

### DO.3 我没有直接采用它，而是**先把它实现出来验证**

理由：设置正文是按 `ry += 固定步长` 顺序画的，**步长与"这段文字实际占几行"无关**。
所以渲染器偷偷折行 → 多出来的行**会压住下一行**。

**实测（本批真的实现了）**：`wrap_w` 字段 + `set_wrap_width()` + 两个入口拦截 + 设置正文启用。
构建通过、渲染出来一看 —— **假设成立**：折行产生的续行**确实压在了下一行上**
（「导出审计日志」「审计日志」「搜索：…」下面都能看到鬼影文字）。

### DO.4 于是**回退**，并记下这个结论

回退后 `shot-diff.py verify` → **29 档与基线逐字节相同**（这也是"回退干净"的证明）。

**结论（写下来，免得以后有人再走一遍）**：

> **渲染层不能"偷偷折行"。** 折行会改变文字的**纵向占用**，
> 而布局是按固定步长推进的 —— 两者不一致就会重叠。
> 正确做法只有两条：① **在调用点折行**（调用点自己把多占的高度算进 `ry`）；
> ② 或者让窗口更宽 / 内容区支持横向滚动。

### DO.5 这是本会话第三次"可证明的无效/有害尝试"

* 附录 DD：两次修控制中心裁剪，渲染图 **sha256 与改前完全相同** → 无效；
* 附录 DO（本批）：自动折行 → 渲染图**出现重叠** → 有害，回退。

三次都不是"感觉不行"，而是**渲出来看/比 sha256** 得到的结论。
**代价是几次构建与渲染，收益是不把一个会制造重叠的改动留在仓库里。**

### DO.6 遗留（如实）

英文长句在设置页仍会被窗口右沿裁掉 —— **未修**。
修法已在 DO.4 写清（调用点折行 或 更宽的窗口/横向滚动），**属于下一批的活**。

### DO.7 验证

* 本批**没有留下任何产品代码改动**（实验已回退，`git status` 干净）；
* 构建 0 错误 0 警告；`shot-diff.py verify` → **29 档与基线逐字节相同**；
* `gates.py` → **19 项全过**。

---

## 附录 DP：把"哪几行超宽"变成**可复现的清单**，并按 DO.4 的第①条修（2026-10-05 批次 DP-1）

### DP.1 先要一份清单，否则就是盲改

附录 DN/DO 的结论是：**修法必须在调用点折行**（渲染层偷偷折行会重叠）。
但设置页的 `tr.draw` 有几十处 —— 得先知道**具体是哪几处**。

于是加了一个**只读报表** `--report-overflow`：

* `TextRenderer` 加 `over_w`（当前允许宽度）与 `over_hits`（记录超宽文字）；
* `render()` 开头记一笔（**不改变任何绘制结果**）；
* 设置正文绘制期间设上内容区宽度；`--report-overflow` 渲染完打印。

**清单（可复现）**：

| 语言 | 超宽条数 |
|---|---|
| 中文 | **3** |
| 英文 | **12** |

英文的 12 条分布在页 0 / 4 / 7 / 8 / 11 / 13 / 14 / 16-18（16-18 是同一句）。

### DP.2 修法：一个把"折行"和"补高度"绑在一起的助手

```rust
fn draw_wrapped(tr, buf, w, h, x, ry: &mut i32, text, px, rgb, alpha, max_w) {
    let step = theme::size::px(20);
    let segs = tr.wrap(text, px, max_w, 3);
    for (i, seg) in segs.iter().enumerate() {
        tr.draw(buf, w, h, x, (*ry + i as i32 * step) as f32, seg, px, rgb, alpha);
    }
    *ry += (segs.len() as i32 - 1) * step;   // ← 关键：把多占的高度补进 ry
}
```

**为什么必须绑在一起**：附录 DO 实测过"折行但不补高度 → 压住下一行"。
把两件事放进同一个函数，就不会只改一半。

### DP.3 本批修了页 11 与页 12（中英同处，一处修两种语言都好）

* **验证 1（同一个报表复跑）**：页 11 / 页 12 **超宽 0 条** —— 用**发现它的工具**验证修复，形成闭环；
* **验证 2（基线）**：`shot-diff.py verify` → **只有 `page-11` 与 `page-12` 变了**（与预测一致）；
* **验证 3（实拍）**：长句折成 3 行，**且下面的「正在读取…」「重新读取」正确下移、没有重叠**。

### DP.4 遗留清单（下一批照着做）

* 中文还剩 **1** 条（页 0 的「切换后左栏与页标题立即生效；部分页面的正文仍在翻译中…」）；
* 英文还剩 **10** 条（页 0 三条 · 4 · 7 · 8 · 13 · 14 · 16-18 各一条）。

改法已经固定：把该处 `tr.draw(...)` 换成 `draw_wrapped(...)`，宽度传内容区宽度。

### DP.5 ⚠️ 本批的一个失误

我用脚本**机械替换多行 `tr.draw(...)` 调用**时，把内部的文字表达式**按行 strip 后拼成一行**，
结果生成 `),;` → **编译失败**。

**教训**：**多行调用表达式不能用"拼字符串"的方式重排** ——
要么整段按模板替换并把原文表达式**逐字保留**，要么**手工改**。本批是手工对照 git 原文修好的。

### DP.6 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → 基线已重新标定（只有 page-11 / page-12 变，属有意修复）；
* 报表复跑：页 11 / 12 超宽 **0 条**；`gates.py` → **19 项全过**。

---

## 附录 DQ/DR：照清单修完 4 处超宽；以及**英文断在词中间**的修正（2026-10-05 批次 DQ-1 / DR-1）

### DQ.1 安全改法：**只动行级位置，不碰中间的文字表达式**

上一批（DP.5）我"按行 strip 后拼成一行"重排多行调用 → 拼出 `),;` → 编译失败。
这次的规矩是：**只做三处行级改动**，中间的文字表达式**一个字都不碰**：

1. `tr.draw(` → `draw_wrapped(` + 新行 `tr,`；
2. 含 `ry as f32` 的行 → `&mut ry`；
3. 在 `);` 之前插入内容区宽度。

### DQ.2 ⚠️ 第一次仍然失败：我把 `),` 也当成了调用结尾

站点探测允许结尾是 `);` **或** `),` —— 而 match 分支里的 `crate::i18n::tr(...),` 正好以 `),` 结尾，
于是宽度被插进了**调用中间** → 3 个编译错误（参数个数不对）。
**回退**（`git checkout`）后**先把站点真实结构打出来看**，确认结尾必须是 `);`，并加了一条自检
「**中间不允许再出现 `);`**」—— 第二次通过，构建 0 错误 0 警告。

### DQ.3 结果

* 修了 4 个站点：页 8（应用商店）· 页 13（系统更新）· 页 14（显示输出）· 页 16-18 共用的通知中心提示；
* `--report-overflow` 复跑：页 8 / 13 / 14 / 16 的**中英两语都是 0 条**；
* `shot-diff.py verify`：只有 `page-8` / `page-13` / `page-14` 变 —— **页 16 没变**（说明那处文字
  当前要么没被画出、要么已放得下）。**如实记**，不硬说成"修好了三页"。

### DR.1 实拍又发现一处：英文断在**单词中间**

折行后英文出现 `confir|mation`、`depend|encies` —— 因为 `wrap()` 是**按字符**切的。
中文没有空格、按字符断是对的；英文应当**优先回退到最后一个空格**。

修法（`text.rs::wrap`）：断行点若本行存在空格、且空格位置超过本行 **60%**，就在空格处断；
否则仍按字符断。**中文文本里没有空格 → 走原路径 → 中文渲染不变。**

### DR.2 结果

* 实拍：英文变成 `Click a row to install (a` / `confirmation card appears;` /
  `dependencies are pre-check…`（第三行是 3 行上限 + `fit()` 的截断）—— **不再断在词中间**，且不重叠；
* `shot-diff.py verify`：6 档变化（页 8 / 10 / 11 / 12 / 13 / 14）—— 都是带折行的页，符合预期。

### DQ/DR.3 遗留清单（下一批）

`--report-overflow` 里还剩 **5 条**，它们**不是 `tr.draw` 站点**：
页 0 三条（语言提示 / 高对比度 / 性能 HUD）· 页 4（AI 密钥）· 页 7（应用卸载）。
它们由**共享的 `widgets::row(...)` 助手**画的**行内副标题** ——
要在那里折行，就得让**行高跟着长**（布局改动），属于另一类改法，留给下一批。

### DQ/DR.4 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → 基线已重新标定（页 8/10/11/12/13/14 变化，均为折行相关，属有意修复）；
* 报表复跑：已修的站点**超宽 0 条**；`gates.py` → **19 项全过**。

---

## 附录 DS：最后 5 条超宽里的 2 条；以及"报表 0 ≠ 画面变了"（2026-10-05 批次 DS-1）

### DS.1 5 条分成两类

| 类别 | 条目 | 性质 |
|---|---|---|
| **普通 `tr.draw`** | 页 4（AI 密钥）· 页 7（应用卸载） | 可以直接用 `draw_wrapped` |
| **`toggle_row` 的行内副标题** | 页 0 三条（语言提示 / 高对比度 / 性能 HUD） | 行高由**布局**固定，折行必须让行变高 → **布局改动** |

本批修了前两条（用 DQ 的**安全手法**：只动行级位置，中间的文字表达式一个字不碰）。

### DS.2 ⚠️ 又一个只差一个逗号的错误

末行原本是 `..., color::text_dim(), 0.75);` —— `0.75` 是**最后一个参数、后面没有逗号**。
我直接把宽度表达式拼上去 → `0.75(content.w…)` → `expected function, found {float}`。
补上逗号后通过。**回退再重做**（先 `git checkout`，改完立刻构建）。

### DS.3 结果：报表两页都是 0，但**只有页 4 的画面变了**

* `--report-overflow`：页 4 / 页 7 的英文都是 **0 条** ✅
* `shot-diff.py verify`：**只有 `page-4` 变了** —— **页 7 没变**。

**原因（实拍确认）**：页 7 是「应用」页，我折行的那两行在**窗口可视区下方**（本来就被裁掉）。
折行之后每段都够窄 → 报表满意；但它们仍然**看不见** → 像素一模一样。

**这暴露了那条报表的局限**：它量的是**绘制调用**的宽度，**不是可见性**。
所以本批**如实写**：页 7 的修复是"**惰性但正确**"（页面滚动到那里、或窗口拉高时它就生效），
**不是**"这页的显示问题解决了"。

### DS.4 剩下 3 条（下一批，属**布局改动**）

页 0 的三条是 `toggle_row(...)` 的**行内副标题**，画在行内 `y + h*0.56` 处。
要在那里折行，必须**让行变高**，而行的 y/h 由调用点的布局算出（`r1.y + px(112)` 之类）。
**不能改共享的 `widgets::row`** —— 它有几十个调用点，改了它而没同步加高，
就会**压住下一行**（附录 DO 的实测结论）。

**所以下一批的做法**：给设置正文加一个"带长提示的开关行"助手，
它**先量出折行行数**、把行高加够、再画 —— 调用点从返回值拿高度。

### DS.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → 基线已重新标定（只有 `page-4` 变）；
* 报表复跑：页 4 / 页 7 英文 **0 条**；`gates.py` → **19 项全过**。

---

## 附录 DT：**同一块代码连翻三次车** —— 于是整块回退，如实记录（2026-10-05 批次 DT-1）

### DT.1 目标

`--report-overflow` 里最后 3 条（页 0 的语言提示 / 高对比度 / 性能 HUD）是
`toggle_row(...)` 的**行内副标题**。要在那里折行，必须**让行变高**；
而外观页那四行是**固定偏移**定位的（`r1.y + 0/56/112/154`）——
所以必须**先量出折行行数、再定位后面的行**（附录 DO 的教训）。

### DT.2 三次尝试，三次都改坏了

| 次数 | 做法 | 结果 |
|---|---|---|
| 1 | 整块重写那一段 | 我**凭印象**写了 `s.dark`（原文是别的字段）→ 编译错 |
| 2 | 从 git 取回原文那两行 | 行号错位，把**续行**替换掉了 → 重复行 + 括号不配对 |
| 3 | 外科式插入（只加行/改行） | 前半段都对（构建通过），最后去 `Some(...)` 包裹时，
用「行尾是不是 `"))`」这种**启发式**判断括号 → 字符串**跨行**，切错 → 括号不配对 |

**最终：整块 `git checkout` 回退，工作区 0 改动。** 本批**没有留下任何产品代码改动**。

### DT.3 真正的教训（比"要小心"更具体）

1. **我连着两次违反自己刚立的规矩**：DM/DQ 里我写过「**逐字保留原文表达式，不要凭印象重写**」，
   结果 DT 的第 1、2 次都栽在这上面；
2. **"行尾是不是 `)`"这类启发式判断括号配对是不可靠的** ——
   字符串字面量可以跨行，行尾的括号未必是配对的结束。
   **要判断结构就去看结构**（把整段打出来），不要用行尾特征猜；
3. **一块代码改三次都没成，就不要再试第四次** —— 回退、记录、换思路。
   本批的价值就是**没把半成品留在仓库里**。

### DT.4 下一步（换思路）

不改那四行的固定偏移布局，而是**改提示本身**：
把长提示**拆成两段短句**（各自放得下），或者给这类"开关 + 长说明"换一种呈现
（例如说明单独成行、开关行保持紧凑）。这样**不动布局**，风险低得多。

### DT.5 验证（回退后）

* `git status` → **0 改动**；构建 0 错误 0 警告；
* `shot-diff.py verify` → **29 档与基线逐字节相同**；
* `gates.py` → **19 项全过**。

---

## 附录 DV：换思路修完最后 3 条 —— **不动布局，把长说明挪到下面单独一行**（2026-10-06 批次 DV-1）

### DV.1 上一批的结论

附录 DT：改外观页那四行的**固定偏移布局**，三次都改坏、整块回退。
DT.4 写了下一步："**不改布局，改提示本身**"。

### DV.2 本批的做法

行内副标题换成**短句**，长说明用页面**已有的 `ry` 流水**单独画在下面一行
（本页其他提示本来就是这么画的，本会话已成功做过 6 次）：

| 行 | 副标题（改后） | 完整说明（挪到下面） |
|---|---|---|
| 界面语言 | `中文` / `English` | `中文 · 切换后左栏与页标题立即生效；…` |
| 高对比度 | `WCAG AA 4.5:1` | `把正文与背景的对比度提到 WCAG AA（4.5:1）以上；与主题正交` |
| 性能 HUD | `右下角 · 排障用` / `Troubleshooting` | `右下角显示真实帧时间与内存占用（排障用，默认关）` |

**没有动任何布局行**：四行的 `r1.y + 0/56/112/154` 原样保留，只在那四行**之下**
（`ry = r1.y + 154 + r1.h;` 之后）插了两条 `draw_wrapped`。

### DV.3 结果：**`--report-overflow` 清单全部清空**

* 页 0 中文：**0 条**；页 0 英文：**0 条**；
* 结合前几批（DP/DQ/DS），这份清单从 **15 条**（中 3 + 英 12）降到 **0**；
* 实拍（中文 100%）：四行分开、两条说明在它们下面、**没有重叠**。

### DV.4 ⚠️ 本批又是"差一点"的连环

1. 第一版把 `let hc_hint = ...` 插在了 `toggle_row(...)` 的**续行**上（表达式中间）→ 编译错。
   **这是本会话第五次同类翻车** —— 教训固化：**插入前先把上下文打出来，确认插入点是语句边界**；
2. 第二版把 `hud_hint` 插进了**息屏档位循环内部**（锚点匹配到了别处）→ 出了作用域 → 编译错；
3. 第三版只把副标题的**中文参数**换短了，**英文参数还是长句** → 报表仍报 2 条。
   （这条最典型：**双语参数只改一半**，靠报表才看出来。）

### DV.5 150% 下的拥挤：**既有的、且页面自己写明了**

英文 150% 下「界面字号」那一行与说明挨得近。这是**既有现象**，而且**页面自己的提示就写着**
「150% 会偏挤，见缺口清单 §3.4」——不是本批引入的。

### DV.6 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → 基线已重新标定（`settings` 与 `page-0` 变化，均为本批有意改动）；
* 报表复跑：页 0 中英均 **0 条**；`gates.py` → **19 项全过**。

---

## 附录 DW：把几何 + 空白检查**扩到英文**（英文路径此前零机器覆盖）（2026-10-06 批次 DW-1）

### DW.1 缺口

本会话所有机器判据 —— 几何断言、空白检查、像素基线 —— **全部只在中文下跑过**。
英文路径**没有任何机器覆盖**。

而就在最近两批里，我**两次改坏过英文**：

1. 把页面标题翻译了 → `match title` 不再命中 → **整页内容消失**；
2. 给 `toggle_row_hint` 少传一个参数 → 编译错（这次编译器救了）。

**第 1 次是"眼睛看出来的"** —— 按本会话的规矩：**能变成判据的就别靠眼睛**。

### DW.2 做法：英文只跑 150%

`layout-check.py` 的渲染入口加 `lang` 参数，循环改成组合表：

* **中文**：100 / 125 / 150 三档（原样）；
* **英文**：只跑 **150%** —— 那是折行/挤压最容易出问题的一档（本会话两次改坏都在它）。

**为什么不是三档都跑**：门禁时间只增加约三分之一，而不是翻倍。
**如实说明代价**：**100% / 125% 的英文仍无机器覆盖**，只有实拍覆盖。

### DW.3 结果：76 个组合，五项判红全为 0

```
检查：76 个组合（17 页 × 中文 3 档 + 英文 150% 一档）
  ① 横向越出屏幕：0      ② 左右越出设置窗口：0
  ⑤ 窗口类界面横向越界：0   ⑥ 窗口类界面同组重叠（判红）：0
  ⑦ 空白档（内容色 <= 24）：0
```

**这条结果本身有价值**：它说明这两批的英文改动**没有破坏命中区布局**，
而以前这件事只能靠"我看着没问题"。

### DW.4 验证

* `layout-check.py --strict` → 76 个组合、① ② ⑤ ⑥ ⑦ **全 0**、退出码 0；
* `gates.py` → **19 项全过**（几何那一步的时间上升约三分之一）；
* 本批**没有改产品代码**，`shot-diff.py verify` → 29 档逐字节相同。

---

## 附录 DX：给像素基线加**英文设置页**（29 → 48 档）（2026-10-06 批次 DX-1）

### DX.1 接上一批的缺口

附录 DW 给英文补了**结构**判据（几何 + 空白，150%）。但**像素**还没有：
英文路径已经出过两次 bug（整页内容消失 / 少一个参数），**第 1 次只有眼睛发现**。

### DX.2 范围：只加设置页

`shot-diff.py` 的用例表加 19 个英文设置页档位（`--lang en`）：

* **加**：19 个设置页 —— 英文的问题都出在"**文案更长**"的设置正文里；
* **不加**：窗口形态（桌面 / 控制中心 / 菜单 / 安装器 / 确认卡）—— 它们的英文由实拍覆盖。

**基线档位：29 → 48。**

### DX.3 这意味着什么

从此**英文设置页的任何视觉变化都会让门禁变红**（就像中文那样）。
代价是门禁多约 2 分钟；换来的是"英文路径不再只靠眼睛守"。

### DX.4 验证

* `shot-diff.py save` → 基线写入 **48 档**；
* `shot-diff.py verify` → **48 档与基线逐字节相同**；
* `gates.py` → **19 项全过**；本批**没有改产品代码**。

---

## 附录 DY：把"英文只跑 150%"那个缺口也补掉（中英各三档）（2026-10-06 批次 DY-1）

### DY.1 为什么要补

附录 DW 给英文补了机器覆盖，但**为了省时间只跑 150%**，并在附录里如实写了
「**100%/125% 的英文无机器覆盖**」。这条说明等于**挂着一个已知缺口**。

**实测跑满三档只多约 80 秒** —— 不值得为这点时间留一个缺口。

### DY.2 结果：114 个组合，五项判红全为 0

```
检查：114 个组合（17 页 × 中英各 3 档）
  ① 横向越出屏幕：0      ② 左右越出设置窗口：0
  ⑤ 窗口类界面横向越界：0   ⑥ 窗口类界面同组重叠（判红）：0
  ⑦ 空白档（内容色 <= 24）：0
```

**至此几何与空白检查覆盖：中英双语 × 100/125/150 三档 × 17 页 + 9 个窗口形态。**

### DY.3 验证

* `layout-check.py --strict` → 114 个组合、① ② ⑤ ⑥ ⑦ **全 0**、退出码 0；
* `gates.py` → **19 项全过**；本批**没有改产品代码**。

---

## 附录 DZ：英文实拍又发现一处 —— **粗体大标题被窗口切在词中间**（2026-10-06 批次 DZ-1）

> ⚠️ **本附录的「发现」已被撤回** —— 见 **附录 EZ**（用数据量的：标题根本没被切，是我看缩略图看错了）。

### DZ.1 发现

英文 150% 实拍页 11：大标题显示成 `Tool Permission`（应为 `Tool Permissions`）——
**粗体大字被窗口右沿硬切在词中间**，看起来像坏了。页 10 同样是 `Privacy & Permis…`。
中文标题短，这个问题一直没暴露。

### DZ.2 ⚠️ 我给标题加 `fit()` 试了三次，**三次都没生效**

| 次数 | 可用宽度用 | 结果 |
|---|---|---|
| 1 | `content.w - 2*PAD` | 基线不变（`fit` 认为放得下） |
| 2 | `r.w - 2*PAD` | 基线不变 |
| 3 | `(r.x + r.w) - (content.x + PAD)`（起点到窗口右沿） | 基线**仍然**不变 |

三次都说明：**我没找对"实际可视宽度"**（`content` 与窗口的关系在这条路径上还没搞清）。

**于是按附录 DT 立的规矩执行：三次不成，不再试第四次，换做法。**

### DZ.3 换的做法：把两个过长的英文标题改短（零风险）

标题本来就是**翻译**，是我可以定的：

* `Privacy & Permissions` → **`Privacy`**
* `Tool Permissions` → **`Permissions`**

（分组标题已经是「Privacy & Security」，语义不丢。）

**验证**：`shot-diff.py verify` → **只有 `page-10-en` 与 `page-11-en` 变了**（与预测一致），
**中文一档没动**；实拍：`Permissions` 完整显示，不再被切。

### DZ.4 如实说明：这一**类**问题没有被一般性解决

标题被切这件事，本批只是**把两个会触发的标题改短**了。
**根因（标题没有按可视宽度截断）仍在** —— 以后再加一个长标题，还会被切。
真正的修法是找对可视宽度后给标题加 `fit()`，那件事我这三次没做成，如实记在这里。

### DZ.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → 基线已重新标定（`page-10-en` / `page-11-en` 变化，属有意改动）；
* `gates.py` → **19 项全过**。

---

## 附录 EZ：**撤回**附录 DZ 的"发现" —— 标题根本没被切，是我看错了（2026-10-06 批次 EZ-1）

### EZ.1 怎么发现的：那个"修复无效"的信号

附录 DZ 里我给标题加 `fit()` **试了三次**（`content.w` / `r.w` / 起点到窗口右沿），
**三次都没让基线变化**。当时我把它解释成"没找对可视宽度"。

**本批量了数据**：

```
设置窗口：x=320 w=500 → 右沿 820
标题墨迹最右 x = 815   →  距右沿 5 px
```

**标题从来没有被切。** 我上一批的"发现"是**看缩略图看错了** ——
而且违反的正是**我自己写进附录 CW 的那条规矩**：「别拿缩略图当证据，去取数据」。

### EZ.2 处理

* **恢复原标题**：`Tool Permissions` / `Privacy & Permissions`（比缩短版更准确）；
* `shot-diff.py verify` → 与"缩短版基线"相比只有 `page-10-en` / `page-11-en` 不同，
  **重新标定后 48 档逐字节相同** —— 即**完全回到 DZ 之前的状态**；
* 本批**产品代码净改动为 0**。

### EZ.3 两条教训（这次是真学到的）

1. **如果修复没有改变基线，先怀疑问题不存在。**
   三次 `fit()` 无效，我把原因归给"我没找对参数"，
   而更简单的解释是"**根本不需要修**"。**连续失败的修复，本身就是"前提错了"的信号。**
2. **"去看数据"要落到动作上**：附录 CW 已经写过这条规矩，我还是在 DZ 里凭缩略图下了结论。
   这次是**量了窗口右沿与墨迹右边界**才纠正过来 —— 所以规矩要写成：
   **下"被切/重叠/越界"这类结论之前，先量出两个数**（边界在哪、内容到哪）。

### EZ.4 验证

* 构建 0 错误 0 警告；`cargo test --workspace`：见提交记录；
* `shot-diff.py verify` → **48 档与基线逐字节相同**（基线已回到原标题状态）；
* `gates.py` → **19 项全过**。

---

## 附录 FB：把「文字压文字」变成判据 ⑧（以前只能靠眼睛）（2026-10-06 批次 FB-1）

### FB.1 起因：用"量数据"复核自己此前的一个说法

我一直在文档里写「150% 会偏挤」（外观页那行提示本身就写着），**但从没量过**。
附录 EZ 的教训是：**下"重叠/被切"这类结论前先量两个数**。

**量出来的数**（外观页，内容区逐行找"与背景差 > 60"的像素带）：

| 语言 | 文字带 | 相邻间隙 | 最小间隙 |
|---|---|---|---|
| 中文 150% | 9 条 | `[17,16,34,16,36,18,3,12]` | **3 px** |
| 英文 150% | 10 条 | `[13,19,32,4,14,41,14,4,10]` | **4 px** |

**结论：「偏挤」这个说法是准确的**（3–4 px vs 别处 16–36 px），**但不是缺陷**（没有重叠）。
—— 与附录 DZ 那次不同，这次的说法**经得起量**。

### FB.2 更有价值的产出：判据 ⑧

现有几何判据只查**命中区**（控件矩形）重叠，**文字压文字抓不到**。
而这类问题本会话出现过：音乐列表 150% 重叠（眼睛发现）、外观页偏挤（眼睛发现）。

**做法**（加进 `layout-check.py`，**不额外渲染**，每档多花约 0.1 秒）：

1. **背景色 = 文字区里出现次数最多的颜色** —— 不假设深色/浅色主题；
2. 逐行找「与背景差 > 60」的像素 → 文字带；
3. 相邻带之间的**最小间隙**；`<= 1px` 判红。

### FB.3 ⚠️ 做这个判据时我踩了三个坑（都记下来）

1. **x 范围把行右侧的开关列包进去了** → 每行都有开关墨迹 → 永远只有"一条带"，
   判据形同虚设。**修**：x1 收到开关列左边。
2. **我假设了背景是浅色**（"深色像素算墨迹"）→ 深色主题下整片区域都算墨迹，
   又是"一条带"。**修**：改成「与背景差 > 60」，并且背景色**统计得出**而不是写死。
3. **Python 字符串里嵌了 ASCII 双引号** → 脚本语法错（本会话第三次踩这个坑，
   我自己在附录里记过）。**修**：中文里一律用「」。

### FB.4 结果

```
检查：114 个组合（17 页 × 中英各 3 档）
  ① 横向越出屏幕：0      ② 左右越出设置窗口：0
  ⑤ 窗口类界面横向越界：0   ⑥ 窗口类界面同组重叠（判红）：0
  ⑦ 空白档（内容色 <= 24）：0   ⑧ 文字压文字（间隙 <= 1px）：0
```

**"文字压文字"从此有机器判据**，中英双语 × 三档全覆盖。这条以前只能靠眼睛。

### FB.5 验证

* `layout-check.py --strict` → 114 个组合、① ② ⑤ ⑥ ⑦ ⑧ **全 0**、退出码 0；
* `gates.py` → **19 项全过**（几何那一步内新增，未新增门禁项）；本批**没有改产品代码**。

---

## 附录 FC：像素基线加**英文窗口形态**（48 → 57 档）（2026-10-06 批次 FC-1）

### FC.1 缺口

附录 DX 只把**设置页**纳入英文像素基线。而窗口形态上的
**Dock 标签 / 窗口标题 / 状态栏 / AI 栏**都是英文文案（DL / DM / DN 三批改的就是它们），
此前**只有实拍覆盖**。

### FC.2 做法

`shot-diff.py` 的固定形态各加一个 `--lang en` 档位（`layout1-4` / `desktop` /
`controlcenter` / `menu` / `installer` / `confirm`，共 **9** 档；`settings` 与 `page-0` 重复，跳过）。

**基线档位：48 → 57**（中文 29 + 英文 28）。

### FC.3 ⚠️ 我自己的改动出了一个 bug，是**验证**抓住的

第一版循环写成 `for _name, _args in list(CASES)` —— 而那时 `CASES` 里**已经含
`page-0-en … page-18-en`**，于是又包了一层，生成 `page-N-en-en`：

* 预期 57 档，**实得 95 档**；
* **我照例打印了档位表做验证** —— 一眼看到 `page-0-en-en`，当场发现。

修法：循环里跳过 `_name.endswith("-en")`。
**这正是"每个改动都要有验证"的价值**：这次它抓的是**我自己**的错。

（附带说明：修正后我在验证脚本里用 `importlib` 读用例表，断言读到 76 而非 57 ——
那是**验证脚本**的问题，工具本身 `save` / `verify` 都报 **57 档**。以工具自己的输出为准。）

### FC.4 验证

* `shot-diff.py save` → 基线写入 **57 档**；`verify` → **57 档与基线逐字节相同**；
* `gates.py` → **19 项全过**；本批**没有改产品代码**。

### FC.5 现在英文路径的覆盖

| 判据 | 中文 | 英文 |
|---|---|---|
| 几何（含文字压文字 ⑧） | 3 档 × 17 页 + 9 形态 | **3 档 × 17 页 + 9 形态** |
| 空白档 | 3 档 × 17 页 | 3 档 × 17 页 |
| 像素基线 | 29 档 | **28 档**（设置页 19 + 窗口形态 9） |

---

## 附录 FD：把「英文位是否漏译」做成门禁（19 → 20 项）（2026-10-06 批次 FD-1）

### FD.1 起因：换一种办法审英文文案

英文文案的审查**不需要看图** —— `pick(lang, "中文", "English")` 的第二个参数就是英文串。
机器能查的一件硬事实：**英文位里出现了 CJK 字符**（几乎一定是漏译）。

### FD.2 ⚠️ 我的第一版扫描**毫无价值**，而它报的是「0 处」

第一版按**行**做正则 → 只覆盖单行调用 → 而大量 `pick` / `tr` 是**多行**写法
（例如折行提示那种三行）→ 它报「0 处」。

**那个 0 不是证据，是扫描器没覆盖到任何东西。**
（这正是本会话记过的"过滤器掩盖问题"那一类。）

**修法**：全文扫描 + **括号配对**解析调用 → 覆盖 **270 个**调用 → 仍然 **0 处** ——
**这次才是真的**。

### FD.3 关键设计：判据要**断言自己的覆盖面**

`scripts/i18n-check.py` 里写了：

```python
MIN_CALLS = 200          # 实测 270 个；低于这个数说明解析器坏了
...
if scanned < MIN_CALLS:
    print("[FAIL] 扫描覆盖面不足 —— 解析器可能坏了，这个 0 不是证据")
```

**一个"扫不到东西也报通过"的判据等于没有判据。**
本会话已有这条教训（附录 DH：「一个从不触发的判据等于没有判据」），这里把它落成了代码。

### FD.4 两次**对照实验**（证明判据真的会响）

| 对照 | 做法 | 结果 |
|---|---|---|
| ① 覆盖面断言 | 把 `MIN_CALLS` 临时调到 300 | **[FAIL] 扫描覆盖面不足（270 < 300）** ✅ |
| ② 真实漏译 | 把某处 `pick(lang, "高对比度", "High contrast")` 的英文位改成中文 | **[FAIL] draw.rs:4676 pick() 的英文位含中文：高对比度** ✅ |

注入后还原 → 重新通过。**判据既能抓到问题，也能在问题消失后放行。**

### FD.5 结论：合成器的英文文案**没有漏译**

扫了 270 个 `pick` / `tr` 调用的英文位 + 全部 `title_en` / `*_EN` 常量：
**英文位含中文 0 处**。另外统计出「英文位与中文位完全相同」的只有 2 处：
`"WCAG AA 4.5:1"`（规范名）与 `"aetherd"`（服务名）—— 都**合理**。

### FD.6 范围说明（不夸大）

本判据覆盖 **`aether-compositor` 的 `pick` / `tr` / `*_EN` 位置**。
**不覆盖** `aether-ops` / `aether-install` 的 CLI 文案 —— 它们是**有意单语**的（附录里已记）。

### FD.7 验证

* `scripts/i18n-check.py` → 270 个调用、0 处漏译、退出码 0；
* 两次对照实验都按预期判红/放行；
* `gates.py` → **20 项全过**（新增第 20 项）；本批**没有改产品代码**。

---

## 附录 FE：给判据写**自检** —— 结果发现 ⑧ 有一个真盲点（20 → 21 项）（2026-10-06 批次 FE-1）

### FE.1 动机

附录 FD 立了一条：**判据要能真的响，还要断言自己的覆盖面**。
但**判据 ⑧（文字压文字）我只验证了它报 0，没证明它会报红** ——
一个从不触发的判据等于没有判据。

### FE.2 做法：合成 BMP 喂给分析函数（不改产品代码、不用等 4 分钟）

`scripts/layout-check-selftest.py` 自己造 24bpp BMP（深底 + 亮字带），断言：

* `text_min_gap`：两行相距 6px → 6；只有一行 → 9999；
* `overlap()`：完全重叠 → 面积；相接 / 分离 → 0；
* `content_colors()`：全背景 → 1；有内容 → 2。

### FE.3 ⚠️ 自检立刻抓到一个**真盲点**

合成"两行紧贴"的图 → `text_min_gap` 返回 **9999**（"只有一条带"）——
**判据根本不响，恰恰在它最该响的时候。**

原因：两行**完全并在一起**时，逐行找墨迹会把它识别成**一条带**，
而"带间间隙"这条规则就无从谈起了。

### FE.4 ⚠️ 我试着补它，结果**误报 45/114**

补法：某条带高度 >= 中位带高的 2 倍 → 判红（两条并一条时带高会翻倍）。

**真实全量跑出来 ⑧ = 45**（114 个组合里）——
**大标题带天然就是小提示带的 2 倍以上**，所以这条规则在**正确代码**上疯狂报警。

> **一个在正确代码上响 45 次的判据，等于废掉这个判据。** 已回退。

### FE.5 保留什么（以及**如实写下的边界**）

* 判据 ⑧ 保留「相邻文字带间隙 <= 1px」；
* **盲点写进代码注释与自检**：两行**完全并在一起**时判据**抓不到**。
  自检里那条用例改成**断言这个限制**（`得到 11（期望 > 1）` —— 即**不判红**），
  而不是假装能抓到。
* 顺带修正了我自己一个错的期望值：带是**闭区间**，`216-210 = 6` 而不是 5。

### FE.6 接入门禁

`scripts/layout-check-selftest.py` 接入 `gates.py` → **20 → 21 项**（2.5 秒）。

### FE.7 验证

* 自检：**8 条断言全部通过**（含"如实断言盲点"那条）；
* `layout-check.py --strict` → 114 个组合、① ② ⑤ ⑥ ⑦ ⑧ **全 0**、退出码 0；
* `gates.py` → **21 项全过**；本批**没有改产品代码**。

---

## 附录 FF：给 `scale-audit` 写正向对照 —— 顺带查出它的**覆盖边界**（21 → 22 项）（2026-10-06 批次 FF-1）

### FF.1 动机

缺口清单里背书的「**6 处缩放写死**」就是这个静态扫描器抓的，
但**没人证明它真会响** —— 按附录 FD/FE 的规矩：判据要能真的响。

### FF.2 ⚠️ 第一次对照：报「0 处」，我差点判定"判据坏了"

做法：往 `aether-compositor/src/*.rs` 里写一个**临时文件**，内含 4 类违规写法。
结果：**每一类都报 0 处**，还打印「[ OK ] 五类都没有问题」。

**隔离**（三步都验了）：

* `glob("*.rs")` **取到了**临时文件（31 个文件，含它）；
* ③ 的正则**匹配**我写的行（`x: other.x + 40`，n=40，行内无 `px`）；
* ④ 的正则**也匹配**。

那为什么报 0？—— 读源码找到了答案：

```python
DRAW = ROOT / "aether-compositor/src/draw.rs"   # 写死的**单个文件**
text = DRAW.read_text(encoding='utf-8')
```

**①②③④ 四类规则只扫 `draw.rs` 一个文件**（后面那两个 glob 是 ⑤⑥⑦ 用的）。
**是我的对照放错了地方，不是扫描器坏了。**

### FF.3 顺带查出的真实覆盖边界

既然 ①②③④ 只扫 `draw.rs`，那 `widgets.rs` / `term.rs` 等文件里的同类写法
**不在扫描范围内** —— 这是一个**真实的覆盖边界**（不夸大：本次没在那些文件里发现实例，
但边界确实存在）。

### FF.4 做法

给 `scale-audit.py` 加 `--file <路径>`（默认仍是 `draw.rs`）：

* 让对照脚本可以指向**临时文件**（**完全不碰产品源码**）；
* 也便于人工按文件审计。

### FF.5 正向对照（已固化为 `scripts/scale-audit-selftest.py`）

```
① 默认（draw.rs）：退出码 0（应为 0）
② 已写临时违规文件 zz_scale_audit_selftest.rs
   退出码 1（应为 1）；点名文件：True；③ 计到 1 处：True
③ 临时文件已删除
④ 清理后默认运行：退出码 0（应为 0）
[ OK ] scale-audit 正向对照通过：注入判红并点名文件，清理后放行
```

**判据确实会响**：注入 → `[FAIL]` + 点名文件 + 退出码 1；清理 → 放行。

### FF.6 接入门禁

`scripts/scale-audit-selftest.py` 接入 `gates.py` → **21 → 22 项**。

### FF.7 验证

* `scripts/scale-audit-selftest.py` → 四步全通过、退出码 0；
* `scale-audit.py --strict` → 默认（draw.rs）干净、退出码 0；
* `gates.py` → **22 项全过**；临时文件在 `finally` 里删除，工作区无残留。

---

## 附录 FG：逐文件扫一遍 —— **结论是不扩大范围**（有数据支撑）（2026-10-06 批次 FG-1）

### FG.1 起因

附录 FF 查出：`scale-audit` 的 ①②③④ 只扫 `draw.rs`，别的文件不在范围内。
本轮去收这个边界 —— 但**先量，再决定**。

### FG.2 先把尺寸表修对

`table` 原本从**被扫文件**里取常量 → 扫别的文件时表是空的 → ①② 永远报 0（无意义）。
常量定义在 `draw.rs`，所以**表固定从 `draw.rs` 取**。

### FG.3 逐文件扫描：6 个文件、10 处命中

| 文件 | 命中 | 判读 |
|---|---|---|
| `input.rs:256` | ④ | `buf[..n / 24 * 24]` —— **加密缓冲区**，误报 |
| `main.rs:3859` | ④ | `... / rect.w * 100.0` —— **百分比**，误报 |
| `main.rs:3933` / `4032` | ④×2 | `open_app()` 的**新窗口层叠位置**（窗口尺寸本就不随缩放，小偏移是层叠步长），误报 |
| `settings.rs:1036` | ④ | `Settings::load(..., 8 * 60)` —— **超时秒数**，误报 |
| `sha512crypt.rs:144/194` | ④×2 | `data.len() * 8` / `password.len() * 2` —— **加密**，误报 |
| `text.rs:305` | ④ | `(px * 100.0) as u32` —— **键编码**，误报 |
| `widgets.rs:363` | ⑤ | `badge()` 里写死的**胶囊高度 18** 与带高 18.0 |

**10 处里 8 处是宽规则的误报**（④ 只要求"`* N` 或 `+= N`（N>=20）且行里有 x/y 之类的关键字"，
在非布局代码里必然乱撞）。

### FG.4 结论：**不扩大门禁的默认扫描范围**

理由就是上面的数据：把 ①②③④ 铺到全 crate，会带来**大量误报** ——
而**一个在正确代码上乱叫的判据等于废掉这个判据**（附录 FE 里"误报 45/114"的同一条教训）。

**如实写下的边界**：①②③④ 默认只扫 `draw.rs`；要审计别的文件用 `--file <路径>`
（这是附录 FF 加的入口）。

### FG.5 顺手修掉唯一一处"像 bug"的

`widgets.rs::badge()` 里写死 `h: 18` 与 `vcenter(..., 18.0, ...)`。
先查调用点：**只有一处，而且在测试里**（`widgets.rs:566` 传 `None`）——
**没有生产调用点**，所以它不是"会溢出"的活 bug，是又一个「**存了不用**」的助手。

仍然把它改成跟着缩放（`crate::draw::theme::size::px(...)`，注意 `theme` 是
`draw.rs` 里的模块，从 `widgets.rs` 要走全路径）：

* 构建 **0 错误 0 警告**；
* `scale-audit --file widgets.rs` → **[ OK ] 五类都没有问题**；
* `shot-diff verify` → **57 档逐字节相同** —— 这也**印证了它确实是死代码**（改了画面完全不变）。

### FG.6 验证

* 逐文件扫描：6 个文件、10 处命中，逐条判读（见上表）；
* `gates.py` → **22 项全过**；`shot-diff verify` → 57 档不变；
* 审计附录 107 → 108。

---

## 附录 FI：把「文字超宽」接成判据 ⑨ —— 顺带抓到一个**更严重的洞**（2026-10-06 批次 FH-1）

### FI.1 起因

「文字超宽」这一类追了 10 轮（15 条 → 0 条），却**没有任何判据防止它回来**。
做法本应很省：`layout-check.py` 本来就在逐档渲染，只要给那次渲染加 `--report-overflow`。

### FI.2 接上后报 **59 条**，而且**全在 125% / 150%**

**这直接推翻了我此前「文字超宽清单已清零」的说法** —— 那只在 **100%** 下成立。
（正是提交 `0ef4324` 提醒过的「文档不能过度声称」。）

### FI.3 ⚠️ 更严重的洞：**拿不到布局 = 静默通过**

两个 flag 一起用时 `--dump-layout` 拿不到 `LAYOUT` 行，于是主循环走到：

```python
print(f"[SKIP] page-...：没拿到布局（渲染不出来？）")
return 0          # ← 门禁**静默通过**，而其实什么都没检查
```

这是"**一个从不触发的判据等于没有判据**"的另一种形态：
**检查没跑成，却报通过**。已改成 `[FAIL]` + `return 1`。

**这个洞比 ⑨ 本身更重要**：它意味着**任何渲染失败都会让几何门禁悄悄放行**。

### FI.4 ⑨ 的做法：棘轮（不假装旧的已解决）

59 条**尚未逐条判读**（可能是 125% 下逐字形取整造成的 1–2px，也可能真被裁）。
所以判据用**棘轮**：记下 59，**超过就判红**（防回归），减少也允许。

正向对照（照例做）：上限临时调 58 → `[FAIL]`、退出码 1；还原 59 → `[ OK ]`、退出码 0。

### FI.5 环境插曲（如实记）

收尾时 `cargo build --offline` 一度失败：**缺 `adler2 v2.0.1`**（缓存里没有）。
用 `cargo fetch --locked` 补回后恢复，**`Cargo.lock` 未被改动**。
—— 记下来：以后遇到 `--offline` 报缺包，先 `cargo fetch --locked`。

### FI.6 欠账（不夸大）

* **59 条超宽尚未逐条判读** —— 缺口清单里不能再写「已清零」，
  应写成「**100% 下为 0；125%/150% 下 59 条待判读**」；
* 下批工作：逐条判读这 59 条，区分"取整假阳性"与"真被裁"，然后该修的修。

### FI.7 验证

* `layout-check.py --strict` → 114 个组合、① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 59（棘轮 59）、退出码 0；
* 拿不到布局 → **判红**（不再 SKIP 通过）；
* `gates.py` → 见提交记录；审计附录 109 → 110。

---

## 附录 FJ：判读 59 条超宽 —— **没有一条是取整**，并修掉最大的 5 条（2026-10-06 批次 FJ-1）

### FJ.1 先给报表加上「超出量」

要判读就必须知道**每条超了多少 px**。`over_hits: Vec<String>` → `Vec<(String, f32)>`，
打印格式改为 `OVERFLOW <超出>px <文字>`。

### FJ.2 判读结果：59 条**全是真的**

| 档位/语言 | 条数 |
|---|---|
| 125% en | 16 |
| 125% zh | 4 |
| 150% en | 25 |
| 150% zh | 14 |
| **100%** | **0** |

* 超出 **<= 2px**：**0 条**（原以为会有一批"逐字形取整"的假阳性，**一条都没有**）；
* 超出 **> 2px**：**59 条**，最大 **247.4px**。

### FJ.3 根因

`set_over_width` 用的是**窗口宽度**（`r.w`，窗口固定 500px），
而文字**随缩放变大** —— 所以 125%/150% 下长句**必然溢出**并被窗口裁掉。
这正是那行提示自己说的「只缩放文字；行高与边距仍是固定值（150% 会偏挤）」。

**这不是 59 个独立的 bug，是同一个设计约束的表现**：窗口尺寸不随缩放变化。

### FJ.4 本批修掉最大的 5 条

| 条目 | 超出 | 修法 |
|---|---|---|
| 页9@150%·en 口令提示 | 247.4px | 普通 `tr.draw` → `draw_wrapped` |
| 页7@150%·en 宿主程序说明 | 230.9px | 同上 |
| 页16/17/18@150%·en 通知历史说明（三页共用） | 228.0px | 行内副标题改短 + 全文用 `draw_wrapped` 画在行之下（附录 DV 手法） |

**定点复验**：页7 → 0 条；页9 那条目标串消失；页16/17/18 → 0 条。
**总数 59 → 43**，棘轮随之收紧到 43。

### FJ.5 ⚠️ 过程里我又踩了三个坑（都记下来）

1. **`ry as f32` 在调用的第二行** —— 安全手法"只改首行"只适用于两者**同一行**的情况；
   多行调用要按**语句范围**逐行找。
2. **末行本来就是单独一行 `);`** —— 我插宽度时留下了一个前导逗号。
3. **「双语参数只改一半」又犯了** —— 行内副标题只把中文改短、英文仍是长句，
   于是 150% 下仍报超宽。**这是第二次**（附录 DV 记过第一次），靠**定点复验**才看出来。

### FJ.6 欠账（不夸大）

* 还剩 **43 条**超宽（全在 125%/150%）**未逐条判读**；
* 它们是**同一个设计约束**（窗口不随缩放变大）的表现，
  所以正确的下一步不是"逐条打补丁"，而是**决定**：
  要么让窗口/内容宽度跟着缩放，要么承认"150% 下长句会被裁"并写进文档；
* 棘轮保证它**不再变多**。

### FJ.7 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 43（棘轮 43）、退出码 0；
* 定点复验三处 → 0 条；构建 0 错误 0 警告。

---

## 附录 FK：**根因修复** —— 让窗口跟着缩放，150% 档超宽 39 → 0（2026-10-06 批次 FK-1）

### FK.1 决策依据（承接附录 FJ）

FJ 判读出 43 条超宽的**根因**不是 43 个独立 bug，而是**同一个设计约束**：

* `set_over_width` 用**窗口宽度**；
* 窗口尺寸 `w: 500, h: 380` 是**写死**的（`main.rs` 的 `open_app()`）；
* 文字**随缩放变大** → 125%/150% 下长句必然溢出被裁。

FJ 的结论是「下一步该决定：要么让窗口跟着缩放，要么把'150% 下长句会被裁'写进文档」。
**本批选了前者**，因为它是根因、改动小（两处尺寸）、而且有护栏（几何判据 + 像素基线 + 棘轮）。

### FK.2 改动

`main.rs` 里两处新窗口尺寸：`w: 500, h: 380` → `w: draw::theme::size::px(500), h: draw::theme::size::px(380)`。

**150% 下的窗口**（实测 dump）：`x=320 y=90 w=750 h=570` ——
`320+750=1070 < 1280`、`90+570=660 < 760`，**仍在屏幕内**。

### FK.3 实测收益

| 档位 | 改动前 | 改动后 |
|---|---|---|
| 150% | **39 条**（en 25 + zh 14） | **0 条** |
| 全档合计 | 43 条 | **0 条** |

### FK.4 为什么这次敢改（护栏）

* **几何判据 ①**（横向越出屏幕）会抓窗口变大导致的越界；
* **像素基线**会告诉我画面变了多少（这是**有意改动**，需重标定）；
* **棘轮**保证不会比改动前更糟 —— 收益不明显就整块回退。

### FK.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0（棘轮 0）、退出码 0；
* `shot-diff.py save` → 基线已重标定（窗口尺寸变化属**有意改动**）；
* `gates.py` → 22 项全过。

---

## 附录 FL：150% 实拍抓到**判据抓不到**的重叠 —— 裸数字 `154`（2026-10-06 批次 FL-1）

### FL.1 现场

附录 FK 让窗口跟着缩放后，150% 实拍外观页发现**多行文字叠在一起**：
「界面字号：100%」「语言说明」「缩放说明」「高对比度说明」全压在一处。

### FL.2 原因

DV 批把两条长说明挪到四行之下时，起点写成：

```rust
ry = r1.y + 154 + r1.h;      // ← 154 是**裸数字**
```

而上面三行的偏移都走了 `px()`（`px(56)` / `px(112)` / `px(154)`）—— **只有这一处漏了**。
于是 150% 下四行占 `px(154)+px(h)` 的高度，说明却从 `154+px(h)` 开始，正好压在行上。

### FL.3 ⚠️ 判据 ⑧ 为什么没抓到（已知盲点咬了我）

判据 ⑧（文字压文字）靠"逐行找文字带"：**两行完全叠在一起时会并成一条带** →
它看不出重叠。**这正是附录 FE 里如实记下的已知盲点。**

> **是实拍抓到的，不是判据。** 所以"改影响所有页面的东西之后必须回头亲眼看"这条规矩
> 是有价值的 —— 这一批就是它的直接证据。

（另外：判据 ⑨「文字超宽」也报 0 —— 因为问题在**纵向**，不在宽度。
判据各管一维，别指望一条判据覆盖所有形态。）

### FL.4 修法

`ry = r1.y + 154 + r1.h;` → `ry = r1.y + theme::size::px(154) + r1.h;`

顺带扫了那一段的**所有**偏移写法：**没有别的裸数字**。

### FL.5 下一步（怎么把这类变成判据）

可行的方向：拿 `--dump-layout` 的**命中区矩形**（行矩形）与渲染出的**文字带**做交叉检查 ——
文字带若落在两个行矩形的**交界**上，就是这种"压在行上"的重叠。
本批没做（预算有限），如实记在这里。

### FL.6 验证

* 150% 实拍：四行分开、说明依次排在下面、**不再重叠**；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定；
* `gates.py` → 22 项全过。

---

## 附录 FM：补 `scale-audit` 规则 ③ 的缺口 —— 赋值形式的裸数字偏移（2026-10-06 批次 FM-1）

### FM.1 缺口

附录 FL 的 bug 是：

```rust
ry = r1.y + 154 + r1.h;      // 154 是裸数字
```

**它本该被 `scale-audit` 的规则 ③ 抓到**，但 ③ 的正则是
`[xy]:\s*ident.[xy]\s*\+\s*(\d+)` —— 要求 **`x:` / `y:` 冒号**形式，
而这是**赋值**形式，**规则看不到它**。

（对照：附录 FG 的逐文件扫描也没抓到 —— 因为扫的是**别的文件**，而这一处就在 `draw.rs`。）

### FM.2 补法

给 ③ 加第二条正则：`变量 = 某标识符.[xy] + N`（N >= 20 且行内无 `px()`）。
**先量误报率**（附录 FE 的教训：误报多的判据等于废掉判据）。

### FM.3 结果：报出 4 处，**全是同一形状、全是真的**

| 位置 | 写法 | 判断 |
|---|---|---|
| `draw_settings_content` | `ry = content.y + 70` | 设置正文起点；标题在 `content.y + 18`，标题随缩放变大而起点不变 |
| `draw_installer` | `row_y0 = win.y + 78` | 磁盘列表起点；上面的说明文字会变高 |
| `draw_confirm` | `y = win.y + 74` | 正文起点 |
| `draw_confirm` | `cx = win.x + 27` | 横向偏移 |

**4 处都改成了 `theme::size::px(N)`**（与设计一致：一切都跟着缩放）。
改完规则 ③ **归 0**。

### FM.4 实拍复核

* **150% 确认卡**：badge / 标题 / 工具行 / 警告 / 输入框 / 两个按钮 —— **布局正常、无重叠**；
* **150% 安装器**：已渲染（见证据目录）。

### FM.5 自检同步

`scale-audit-selftest.py` 的违规样本补上赋值形式，断言 ③ 计到 **2 处** ——
**新规则也证明"真的会响"**（不是加了条从不触发的判据）。

### FM.6 插曲（如实记）

改完那 4 处后 `cargo build --all-targets` **报了一次编译器 ICE**
（`the compiler unexpectedly panicked`，只有 test 目标），**重试即通过** —— 是**瞬时**问题，
不是代码问题（`cargo build --bin aether-compositor` 也通过）。

### FM.7 验证

* `scale-audit.py --strict` → 五类全 0（③ 归 0）；`scale-audit-selftest.py` → 通过；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（偏移变化属有意改动）；
* `gates.py` → 22 项全过。

---

## 附录 FO：英文实拍抓到**源码扫描看不见**的漏译（`net.rs`）；运行时仪表没做成（2026-10-06 批次 FO-1）

### FO.1 现场（页6 Network @150%·en 实拍）

状态行显示**「未发现网络接口」** —— 英文界面里的中文。
**而 `i18n-check.py` 报的是「英文位含中文 0 处」。**

### FO.2 为什么判据没抓到

`i18n-check.py` 只扫 `pick(lang, zh, en)` / `tr(zh, en)` / `*_EN` 常量。
这一串来自 `net.rs` 的 `summary()`：

```rust
None => "未发现网络接口".to_string(),
```

**既不是 `pick` 也不是 `tr`** —— 判据看不见它。
**这是判据的盲区，不是"没有漏译"。**（附录 FD 里写过"270 个调用 0 处漏译"，
现在更正为：**那 270 个调用**里 0 处，**调用之外**还有漏译。）

### FO.3 修法

`net.rs` 两处 `summary()` 返回值改成 `crate::i18n::pick(crate::i18n::lang(), zh, en)`。

### FO.4 ⚠️ 运行时仪表：本批**没做成**，已回退

想法：加 `--report-cjk` —— **运行时**报表，只要英文模式下**画出来**中文就报，
这样**不管字符串怎么拼出来的**都能覆盖。

实现完了、能编译，但**对照实验证明它没生效**：

| 模式 | 输出 | 判断 |
|---|---|---|
| EN | 0 条 | 可能是"真的没有" |
| **ZH** | **0 条** | **不可能** —— 中文模式下满屏中文 |

**ZH 那条是决定性的**：仪表没生效。按附录 DT 的规矩（三次不成不再试），**回退**。
（回退时我"逐行删含 report_cjk 的行"太粗暴，把括号数坏了 —— 最后用
`git checkout` 恢复那两个文件，`net.rs` 的修复不受影响。**又是一次"回退也要用可靠手段"的教训。**）

**如实记**：这个盲区**还在**。目前的兜底是**人工**：`--lang en` 渲染 + 亲眼看
（本批就是这样发现 `net.rs` 的）。

### FO.5 顺带发现的另一个小问题（未修，如实记）

同页的 `Gateway(none)` —— 标签与值**挤在一起**（其他行有间距）。
原因是**列宽按较短的标签算**，而 `Gateway` 是其中最长的。未修（预算有限）。

### FO.6 本批实拍复核的英文页（FK 之后）

页 2 / 3 / 6 / 12 / 15 @150%·en —— **布局均正常**（FK 让窗口跟着缩放之后，
原先的超宽全部消失）。唯一一处"看着像被切"的（页15 的 read-only 说明），
**量出来还有 3px 余量**（墨迹最右 1067、窗口右沿 1070）—— **没有 bug，是我又看错了缩略图**。
（这是本会话第三次凭缩略图误判；**但这次先量了再下结论，所以没有误报**。）

### FO.7 验证

* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（`net.rs` 文案变化属有意改动）；
* `gates.py` → 22 项全过。

---

## 附录 FP：运行时 CJK 仪表**第 3 次失败**（已停）；顺带修掉 `Gateway` 列宽（2026-10-06 批次 FP-1）

### FP.1 第 3 次尝试与**精确诊断**

附录 FO 说"运行时仪表没做成"。本批再试，并加了**无条件**调试输出定位，结论更清楚了：

* 在我要加检查的那个位置，**无条件 `eprintln!` 也不打印**；
* 而**紧挨着**的 `over_w` 检查（超宽报表）**是工作的**（判据 ⑨ 有效）。

→ **结论：我改的那个 `render()` 不是设置文字实际走的那条路径。**
最可能的情况：`over_w` 的命中来自**另一处**同名/同形检查，我补错了地方。

**这是给下一次尝试的可用线索**（不必再从零试）。按附录 DT 的规矩：**三次不成，停**。

### FP.2 回退用可靠手段

第 1 次回退（附录 FO）我"逐行删掉含 `report_cjk` 的行"—— 太粗暴，把块的括号数坏了。
这次直接 `git checkout -- main.rs text.rs`（`net.rs` 的修复不在其中，不受影响）。
**教训：回退也要用可靠手段，不要手写"按内容删行"。**

### FP.3 修掉 `Gateway(none)` 列宽

网络页的值列写死在 `content.x + 110`（**裸数字**），而标签**随缩放变大** →
150% 下 `Gateway`（粗体）比 110 宽 → 值落到标签里面。

**修法**：`content.x + theme::size::px(110)` —— 列宽与标签同步缩放，比例恢复。

**实拍复验**（页6 @150%·en）：`Interface (none)` / `MAC (none)` / `Link down` /
`Gateway (none)` / `DNS (not configured)` —— **全部对齐**。
（更好的做法是**量最宽标签**决定列宽；那是把行数组提出循环的结构改动，本批没做。）

### FP.4 顺带记一个判据缺口

`content.x + 110` 这种**函数实参里的裸数字偏移**，`scale-audit` 规则 ③ **看不到**
（它的正则只覆盖 `x:` / `y:` 字段与 `变量 = 矩形.y + N` 两种形式）。
—— 和附录 FM 补的那个缺口是**同一类**，只是位置更隐蔽。

### FP.5 验证

* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（列宽变化属有意改动）；
* 实拍：页6 的五行值列**对齐**；状态行已是英文（`net.rs` 修复，已在 ffef1b9 提交）；
* `gates.py` → 22 项全过。

---

## 附录 FQ：运行时 CJK 报表**做成了** —— 根因是打印块嵌在别人的 `if` 里（2026-10-06 批次 FQ-1）

### FQ.1 三轮失败的根因（一行代码）

`main.rs` 里我的打印块被插到了 **`if report_overflow {` 的里面**：

```rust
if report_overflow {
    if let Some(t) = tr.as_ref() {
        for (s, over) in t.take_over_hits() { println!("OVERFLOW {over:.1}px {s}"); }
        if report_cjk { ... }        // ← 嵌在里面了
    }
}
```

而我一直只传 `--report-cjk`（不传 `--report-overflow`）→ **外层不成立 → 永远不打印**。
**不是仪表没生效，是打印被套在别人的 `if` 里。**

### FQ.2 我是怎么被自己误导的（重要）

上一轮我加的调试是**带条件的**（只在文本含 CJK 时打印），它没打印，我就判断
"这段代码没执行"。这一轮加了**无条件** `eprintln!` → **打印 289 次** → 代码在执行。

> **教训：调试要放在"不受条件影响"的位置。** 用带条件的调试去判断"代码有没有执行"，
> 会把"条件不成立"误读成"没执行"，从而**把排查带偏一整轮**。

### FQ.3 现在的仪表

* `--report-cjk`：只要**画出来**的文字含 CJK 就报（**来源不限** ——
  这正是源码扫描（`i18n-check.py`）做不到的：`net.rs` 那种硬编码它看不见）；
* 接成**判据 ⑩**，与 ⑨ **合并同一次渲染**（两个报表都不用 `--dump-layout`）；
* **只统计英文模式**（中文模式下满屏中文，没有信号）。

### FQ.4 实测与解读

英文模式 CJK 总数 **1608**，棘轮定在此值。**但每页几乎都是 84 条**：

```
页0=84, 页1=84, 页2=84, 页3=93, 页4=84, 页5=84, 页6=87, ...
```

**84 条是背景噪声**（桌面/文件窗口里**夹具建的中文目录名**「文档」「图片」等，
以及 Dock 上的中文应用名）。**设置页本身基本没有漏译** ——
只有 **页3（+9）** 与 **页6（+3）** 有额外中文，**留作下一步排查**。

（如实说明：这个判据的**信噪比不高**，棘轮只能防"上升"。要真正可用，
下一步应当把"背景噪声"（夹具数据）与"设置页自己的文字"分开统计。）

### FQ.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0（棘轮 0）、⑩ = 1608（棘轮 1608）、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定；
* `gates.py` → 22 项全过。

---

## 附录 FR：判据 ⑩ **第一次实战**就抓到 4 处源码扫描看不见的漏译（2026-10-06 批次 FR-1）

### FR.1 用差集找信号

判据 ⑩ 报的是"画出来的中文"，**每页都有约 26 条背景噪声**
（桌面/文件窗口里夹具建的中文目录名等）。所以拿**页0 当基线求差集** ——
多出来的才是设置页自己的可疑漏译。

### FR.2 抓到 4 处

| 位置 | 文字 | 性质 |
|---|---|---|
| `shortcuts.rs:113` | `方向键 / Tab / Delete` | `Shortcut` 有 `desc`/`desc_en` 成对，**`keys` 只有一份** |
| `shortcuts.rs:127` | `Alt+1…4（键位翻译）` | 同上 |
| `shortcuts.rs:134` | `Super（元键）` | 同上 |
| `net.rs:158` | `非 Linux 目标：无网络接口信息` | `note` 字段硬编码 |

**这 4 处 `i18n-check.py`（源码扫描）全都看不见** —— 它们不在 `pick`/`tr`/`*_EN` 位置。
**这正是 ⑩ 存在的意义**，而且它**第一次实战就证明了价值**。

### FR.3 修法

* `shortcuts.rs`：加 `keys_in(&self, lang)` 方法（镜像已有的 `desc_in`），
  **只翻译那 3 条带中文注释的**，其余（`Alt+1…4`、`Enter` 这类本来就跨语言的键名）原样返回；
  `draw.rs` 的 `sc.keys` 改成 `sc.keys_in(lang)`；
* `net.rs`：`note` 走 `pick(crate::i18n::lang(), zh, en)`。

### FR.4 复验

**差集归零**：页3 / 页6 / 页15 / 页1 相对页0 的"额外中文"**全部为 0**。
英文模式 CJK 总数 **1608 → 1596**，棘轮随之收紧。

### FR.5 这一批串起来的两条经验

1. **运行时报表能抓到源码扫描看不见的漏译** —— 本会话验证了两次：
   `net.rs` 那次是**眼睛**发现的，这 4 处是**判据 ⑩**发现的；
2. **判据要有信号可读** —— ⑩ 的信噪比不高（背景噪声占大头），
   所以用**差集**（页0 当基线）而不是看绝对数。**下一步应当把"背景噪声"与
   "设置页自己的文字"在判据里分开统计**（附录 FQ 已记）。

### FR.6 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 1596（棘轮 1596）、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定；
* `gates.py` → 22 项全过。

---

## 附录 FS：判据 ⑩ 从**棘轮**做成**精确硬判 0**（2026-10-06 批次 FS-1）

### FS.1 问题：棘轮混着背景噪声

附录 FQ 里 ⑩ 是个棘轮（1596），因为里面混着**背景噪声**：桌面/文件窗口里
夹具建的中文目录名、音乐曲目名等。棘轮只能防"上升"，**读不出真正的信号**。

### FS.2 两步收紧

**第一步：噪声 = 交集。** 实测每页的 CJK 集合几乎完全相同（页0=26、页15=26、页1=26…）——
背景噪声**每页都有**。所以取**所有 (页, 档) 英文运行的交集**当噪声，
`条目 - 交集` 才是该页自己的文字。

做完这步仍报 **6 条**，但一看内容就知道它们是**数据**不是漏译：

```
冷启动 · 安装说明.md · 安装说明.… · 微光层 · 视界线 · 长夜频道
```

（文件名与音乐曲目名 —— 来自**背景窗口**。）

**第二步：范围 = 设置正文。** `set_over_width` **只在设置正文渲染期间**被设置
（`draw_settings_content` 里那一处）。所以给 CJK 检查加一个条件：
**只在 `over_w` 处于激活状态时才记** —— 范围自然收敛到"设置正文"，
**与"文字超宽"判据同域，且不需要任何白名单**。

### FS.3 结果

```
⑩ 英文模式下**页面专属**的中文（已扣掉每页都有的噪声 2 条）：0
退出码 0
```

**判据 ⑩ 现在是"设置正文里不得出现中文"的硬判 0** —— 而不是一个读不出信号的棘轮。
（附录 FR 抓到的那 4 处漏译，正是这条判据现在会拦住的。）

### FS.4 一条方法论

**判据的范围要用代码里已有的状态来界定，不要用白名单。**
这里的"已有状态"就是 `over_w`（它本来只服务于"文字超宽"报表）——
借它界定范围，比维护一张"哪些中文算数据"的名单**更可靠、也不会随数据变化失效**。

### FS.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、**⑩ = 0（硬判）**、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（本次改动只影响只读报表，画面不变）；
* `gates.py` → 22 项全过。

---

## 附录 FT：判据 ⑩ 的**正向对照** —— 注入一处中文，它判红了（2026-10-06 批次 FT-1）

### FT.1 为什么做

按本会话的规矩（附录 FD / FE / FF）：**判据要能真的响**。
判据 ⑩（"设置正文里不得出现中文"）此前只验证了"报 0"，
**没有证明它会报红** —— 而"一个从不触发的判据等于没有判据"。

### FT.2 做法与结果

把设置正文里「高对比度」标签的**英文位**改成中文：

```rust
crate::i18n::pick(lang, "高对比度", "High contrast")   →   pick(lang, "高对比度", "高对比度")
```

跑全量 `layout-check.py --strict`：

```
⑩ 英文模式下**页面专属**的中文（已扣掉每页都有的噪声 2 条）：1
退出码：1
```

**判据 ⑩ 判红了** ✅。然后用**原文回退**（在 `finally` 里恢复），
`git status` 确认工作区 **0 改动**、构建干净。

### FT.3 为什么不做成常驻门禁

全量 `layout-check` 约 **6 分钟**（114 个组合 + 每次一次额外渲染），
把它塞进门禁会让每次提交都多等 6 分钟。所以这份对照以**可复现的手工步骤**记录在此
（注入哪一行、期望什么结果、怎么回退都写清了）。

### FT.4 顺带确认的一件事

回退时我用的是"**从内存里的原文写回**"，并用 `git status` 核对为 0 改动 ——
对照附录 FO / FP 记的教训（**回退也要用可靠手段，不要手写"按内容删行"**）。

### FT.5 验证

* 注入 → ⑩ = 1、退出码 1；回退 → 工作区 0 改动、构建 0 错误 0 警告；
* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 FU：规则 ③ 再扩一档 —— **任意位置**的 `矩形.x/y + 裸数字`（2026-10-06 批次 FU-1）

### FU.1 缺口（附录 FP 记下的）

`content.x + 110`（网络页值列，写在**函数实参**里）规则 ③ 看不到：
它只覆盖 `x:` / `y:` **字段**形式与 `变量 = 矩形.y + N` **赋值**形式。

### FU.2 做法

加**第三条**正则：`标识符.[xy] + N`（N >= 20 且行内无 `px()`），
并**按 (行号, 匹配文本) 去重**（三条正则会重叠）。

**先量误报率**（附录 FE 的教训）—— 结果查出 **8 处，全部同类**，没有误报：

| 行 | 写法 | 判断 |
|---|---|---|
| 4795 / 4924 / 4940 / 4956 / 4981 / 5061 | `content.x + N` | 设置列表的**值列**（与已修的 `+110` 同类） |
| 6097 | `bar.x + 58` | AI 栏**头像之后**的文字 |
| 6858 | `r.x + 34` | toast **图标之后**的文字 |

**共同形状**：**固定偏移跟在会缩放的元素后面** —— 150% 下标签/图标变大，而偏移不变 → 挤在一起。

### FU.3 修法

8 处全部改成 `+ theme::size::px(N)`（偏移与元素同步缩放）。

**实拍复验**（页5 @150%·en）：`Version` / `License` / `Architecture` / `Kernel` /
`Settings file` 的取值**从同一列开始**，对齐正常。

### FU.4 规则 ③ 的三条正则（现在）

1. **字段形式**：`x:` / `y:` 里 `标识符.[xy] + N`；
2. **赋值形式**：`变量 = 标识符.[xy] + N`（附录 FM 补的）；
3. **任意位置**：`标识符.[xy] + N`（本批补的）。

三条都要求 `N >= 20` 且**行内没有 `theme::size::px`** —— 这就是"写死的偏移"的判据。

### FU.5 验证

* `scale-audit.py --strict` → 五类全 0（③ 归 0）；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（偏移变化属有意改动）；
* `gates.py` → 22 项全过。

---

## 附录 FV：**重测**"要不要把扫描范围扩到全部源文件"（2026-10-06 批次 FV-1）

### FV.1 为什么要重测

附录 FG 的结论是"**不扩**"—— 但那时规则 ③ 只有**两条**正则，误报多。
现在 ③ 有**三条**（字段 / 赋值 / 任意位置，见附录 FM 与 FU），判据更准。
按 FE 的教训：**先量误报率再决定**，所以重测一次。

### FV.2 重测结果

用 `--file <路径>` 逐个扫 `aether-compositor/src/*.rs`：

| 文件 | 命中 | 内容 |
|---|---|---|
| `input.rs:256` | ④ | `buf[..n / 24 * 24]`（加密缓冲区） |
| `main.rs:3859` | ④ | `... / rect.w * 100.0`（百分比） |
| `settings.rs:1036` | ④ | `Settings::load(..., 8 * 60)`（超时秒数） |
| `sha512crypt.rs:144/194` | ④×2 | `data.len() * 8` / `password.len() * 2`（加密） |
| `text.rs:315` | ④ | `(px * 100.0) as u32`（键编码） |

**6 处全部是规则 ④，全部是误报**；**规则 ③ 现在每个文件都是 0**。

### FV.3 结论：决定不变，理由更精确

**不把默认范围扩到全部文件** —— 但现在的理由是：

* **④ 是唯一会误报的规则**（它只看"乘/加裸数字 >= 20 且行里有 x/y 之类关键字"，
  在**非布局代码**里必然乱撞）；
* **①②③ 是精确的**（③ 现在全库为 0，说明它不会乱叫）。

这条已写进 `scale-audit.py` 的注释，并记下**将来若要扩范围的做法**：
**只扩 ①②③、把 ④ 留在 `draw.rs`**。

### FV.4 为什么本批**没有**直接做"部分扩范围"

扩范围需要把 ①②③ 的循环从"单文件"改成"遍历文件"（并让输出带上文件名）——
那是**改动一个已验证、正在跑的门禁**。而重测显示：**其他文件里 ①②③ 现在是 0**，
**当下的收益是 0**。所以：**记录清楚 + 留好做法**，不为了一个"将来的守卫"去动
一个正在工作的判据。（这条判断本身也写进了注释。）

### FV.5 验证

* `scale-audit.py --strict` → 五类全 0、退出码 0；
* `scale-audit-selftest.py` → 正向对照通过、退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 FW：判据 ⑨ 的**宽度模型是错的** —— 它藏住了 54 处真实超宽（2026-10-06 批次 FW-1）

### FW.1 发现链

1. **亲眼看**页9 @150%·en：`Only human accounts (uid 1000–65533); system accounts are hi…`
   看着像被切，**而判据 ⑨ 报 0**；
2. **量**（附录 EZ 的规矩）：墨迹最右 **1067**、窗口右沿 **1070** —— 差 3px；
3. 但完整英文串末尾还有 `hidden`（约 20px 宽）→ 若画出来会到 **1087 > 1070** →
   **它确实被裁了**；
4. **根因**：`set_over_width` 用的是 `r.w - 2*PAD`（**窗口宽度**减两侧内边距），
   而文字是从 `content.x + PAD` 开始画的。对**缩进绘制**（值列、正文缩进）而言，
   **实际可用宽度 = r.x + r.w - (content.x + PAD)** 比 `over_w` **小** → ⑨ **漏报**。
   （与附录 DD / DZ 记过的 `content` 与窗口的差异是同一个坑，这次栽在宽度上。）
5. **修法**：`over_w` 改成"**从文字起点到窗口右沿**"。因为
   `content.x = r.x + SIDEBAR_W`、`content.w = r.w - SIDEBAR_W`，
   它就等于 `r.w - SIDEBAR_W - PAD`（所以可以放在 `set_clip` 那处，那里 `tr` 已是引用）。
6. 修好后 ⑨ 从 **0 涨到 54** —— **不是判据变严了，是它终于看得见了**。

### FW.2 本批修掉的 6 处

3 条英文行（中英两版一起修，都改成 `draw_wrapped`）：

| 页 | 文字 |
|---|---|
| 9 | `Only human accounts (uid 1000–65533); system accounts are hidden` |
| 15 | `Mounted read-only (ro,nodev,nosuid,noexec): browse files, no writes` |
| 6 | `The config itself stays read-only; every change goes through aetherd.` |

**实拍复验**：页 9 / 15 / 6 @150%·en 的 `--report-overflow` 都归 **0**。

### FW.3 ⚠️ 附带纠正我自己的两次错误结论

我此前**两次**用"墨迹距右沿 3px"判断"**没被切**"（页15、页9）—— **两次都错了**：

> 墨迹停在 3px 处，**正是因为被裁剪切在那里**。
> **教训加强**："有余量"要看**余量够不够放没画出来的那部分**，
> 不能只看"没贴边"。（附录 EZ 只写了"要量两个数"，这里补上"**第三个数的算法**"：
> 把"没画出来的部分"的宽度估出来。）

### FW.4 剩下的 41 处：**如实记账**

修完这 6 处后 ⑨ 还有 **41 处**（棘轮定在 41）。**这是一笔明确的欠账**：

* 大多是**中文**串（中文字形宽，同样的语义更容易超）；
* 也有"设计上就长"的说明文字（如页0 的"只缩放文字；行高与边距仍是固定值…"）；
* **不是误报** —— 它们在真实渲染里**确实被裁剪**（⑨ 报的正是这个）。

**处理建议（下一步）**：按页逐条改成 `draw_wrapped`，或把过长的中文串写短。
棘轮保证**不再变多**。

### FW.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑩ = 0、**⑨ = 41（棘轮 41）**、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定；
* `gates.py` → 22 项全过。

---

## 附录 FX：还清 ⑨ 的 41 处超宽欠账（41 → 0）（2026-10-06 批次 FX-1）

### FX.1 三步走

1. **收集**：按 (页, 档, 语言) 全量跑 `--report-overflow` —— 41 条命中，
   去重后是 **19 处唯一文字**（同一串在 3 档各报一次）；
2. **缩短**：改 **29 条**字符串（**中英成对的都一起改** —— 附录 DV / FJ 的教训：
   只改一边等于没改），每条都**断言**替换成功，失败就报出来，不静默跳过；
3. **复验**：⑨ **41 → 0**，棘轮回到 **0**。

### FX.2 缩短的原则

**保留关键信息，去掉括注里的次要细节** —— 反正那些细节现在也是**被裁掉看不见**的。
例：

| 原文 | 改后 |
|---|---|
| `Switch virtual desktop (4 by default, up to 8; windows stay alive)` | `Switch virtual desktop (4 default, up to 8)` |
| `Move the active window to desktop N (4 by default, up to 8)` | `Move the active window to desktop N` |
| `Open the command palette (apps / settings / files in the current folder)` | `Open the command palette` |
| `切换虚拟桌面（默认 4 个、可增删到 8；切走的窗口不会关掉）` | `切换虚拟桌面（默认 4 个、最多 8 个）` |

### FX.3 实拍复验

页3 @100%·en（**变化最大的一页**）：`Switch virtual desktop (4 default, up to 8)`、
`Move the active window to desktop N`、`Open the command palette`
—— **全部完整可见**，不再被裁。

### FX.4 为什么这批值得做

附录 FW 说明：这 41 处**不是误报**，它们在真实渲染里**确实被裁剪** ——
也就是说，**用户此前看到的是被切掉一半的说明文字**。
本批把 ⑨ 的棘轮从 41 收紧回 **0**，并把"⑨ 必须为 0"重新变成**硬判**。

### FX.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、**⑨ = 0（棘轮 0）**、⑩ = 0、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定（文字变化属有意改动）；
* `gates.py` → 22 项全过。

---

## 附录 FY：按 AGENTS.md 禁止清单的**机械合规扫查**（2026-10-06 批次 FY-1）

### FY.1 结果

| 检查项 | 命中 | 结论 |
|---|---|---|
| 生产代码里的调试输出 | 204 | **不是问题**（见 FY.2） |
| 过宽文件权限（777 / 666） | 1 | **误报**：`aetherd/src/snapshots.rs:80` 的 `recorded & 0o7777` 是**权限掩码**（用来检测 setuid/setgid/sticky），不是授予权限 |
| 关闭 TLS 校验 | **0** | ✅ |
| 硬编码密钥 | 1 | **误报**：`aetherd/src/main.rs:62` 的 `api_key: "ollama"` 是**本地 Ollama 的约定占位符**（Ollama 不需要密钥） |
| 废弃加密算法（MD5/SHA1/DES/RC4） | **0** | ✅（`sha512crypt` 是 SHA-512，不在禁用之列） |

### FY.2 "调试输出 204 处"为什么不是问题

AGENTS.md 禁的是**遗留的调试语句**。这里的 `eprintln!` 是**这个项目的日志通道**：

* musl 最小化环境**没有日志框架**（引入 `log` + 后端会显著增大镜像，与"最小化"目标冲突）；
* 每条都带**模块前缀**（`aether-compositor:` / `[aetherd]` / `[wayland]`），是**有意输出的运行信息**，
  不是"打印变量看看"；
* 唯一"像调试"的是 `text.rs` 的 `dump_metrics` —— 它是 **`--fonttest` 走查的显式输出**，
  函数上有文档说明（"换字体或升级 fontdue 后用 `--fonttest` 复跑"）。

### FY.3 本批**没有改代码**

扫查结论是"无需改动" —— 两处命中都是误报，其余为 0。
按本会话的规矩（附录 DZ / EZ）：**先量、再下结论**；这里量完发现**没有要修的东西**，
所以**不改**（而不是"为了交差改点什么"）。

### FY.4 与"整体审计 3 遍"的关系

AGENTS.md 要求"**所有任务全部完成后，整体执行审计 3 遍**"。
本批是**每个任务后 1 遍**的那一遍（机械合规角度）。
"全部完成后的 3 遍"会在收口时执行，**角度会不同**（OWASP 分类 / 数据流 / 权限边界）。

---

## 附录 GA：**页数虚高** —— 实际 17 页，"114 个组合"应为 102（2026-10-06 批次 GA-1）

### GA.1 怎么发现的（第 1 条规矩又救了一次）

按本会话的**第 1 条规矩**（看渲染前先比 sha256）：本批渲染的页16 / 页17 / 页18
三张图**哈希完全相同**（`fd46e8bf…`）。

查 `settings.rs` 的 PAGES 表：**只有 17 条**（页 0..16）。页 17 / 18 **不存在** ——
`--settings-page` 越界后**回落到最后一页**，所以三张图一模一样。

### GA.2 影响：覆盖率数字虚高

`scripts/layout-check.py` 里写着 `PAGE_COUNT = 19`，于是遍历 `range(19)`：

* 页 17 / 18 是**重复渲染页 16**；
* 判据报的"**114 个组合（19 页 × 中英各 3 档）**"**虚高**；
* **实际是 17 × 6 = 102**。

### GA.3 本批改动

* `PAGE_COUNT` 19 → **17**（附注释：怎么推导、为什么曾误写、错了会怎样）；
* 文档里的"19 页" → "17 页"；
* 删掉两张**重复**的证据图（页17 / 页18 实际就是页16）；
* 实测：`检查：102 个组合（17 页 × 中英各 3 档）`，退出码 0。

### GA.4 ⚠️ 更正说明（关于历史附录）

历史附录（例如 DY）里写的"**114 个组合（19 页 × …）**"是**当时的记录**，
按当时对页数的认识写的。**正确值是 17 页 / 102 组合**。

本批的自动替换把那些附录里的"19 页"也改成了"17 页" —— 这会让"114 个组合（17 页 × …）"
看起来自相矛盾。**此处明确更正**：历史附录里的 **114** 应读作 **102**；
**改动页数这一项本身是错的（改了历史记录）**，正确做法是保留原文 + 在本附录里更正。
（这是本会话第 14 条经验：**修正历史记录要用"追加更正"而不是"改写原文"**。）

### GA.5 验证

* `layout-check.py --strict` → `检查：102 个组合`、① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GB：整体审计**第 1 遍** —— 权限与确认流程（2026-10-06 批次 GB-1）

> 按 AGENTS.md："**所有任务全部完成后，整体执行审计 3 遍**"。
> 这是**第 1 遍**，角度：**权限与确认流程**（安全边界）。
> 另外两遍的角度计划为：**数据流 / 信任边界**、**文件与路径**。

### GB.1 审计对象

`aetherd` 的确认机制（`src/perm.rs` 的 `Approvals` / `Gate`，`src/main.rs` 的
`PendingConfirm` 与"暂停 → 上抛 UI → 带令牌重发"的链路）。

### GB.2 ✅ 稳的部分（逐条核对）

| 性质 | 实现 | 判断 |
|---|---|---|
| **绑定请求** | 令牌绑定 `(tool, args)` —— "换个工具或改一个参数都算不匹配" | ✅ 防混淆代理 |
| **有效期** | `TOKEN_TTL = 300s`，`issued.elapsed() >= TTL` 即拒 | ✅ |
| **一次性** | `map.remove(token)` 在**比对之前** → 同一令牌无法重放 | ✅ |
| **失败即作废** | 校验失败时令牌已被移除 → **fail-closed** | ✅ |
| **不泄漏内存** | `issue()` 里 `retain` 清理过期项 | ✅ |

### GB.3 ⚠️ 发现并修掉 1 处：令牌的随机性不够

原实现：

```rust
let now = SystemTime::now()...as_nanos() as u64;
let mut h1 = std::collections::hash_map::RandomState::new().build_hasher();
h1.write_u64(now);
...
```

**问题**：`RandomState` 的文档**明确说它不用于密码学用途**（它是 SipHash + 随机种子的
哈希器，不是 CSPRNG）。而确认令牌是**安全边界** —— 它批准一次 L2/L3 操作。

**攻击面评估**（如实说明）：要利用它，攻击者需要在 300s 内**猜中**某个**挂起请求**的令牌，
且令牌还要匹配该请求的 `(tool, args)`。所以**风险不高**，但这是**应当修**的一处：
安全边界上的随机数不该用"非密码学"来源。

**修法**：优先读 **`/dev/urandom`**（16 字节 → 32 位十六进制，**格式不变**）；
读不到时退回旧办法，并**打印警告**（安全降级必须可见，不能静默）。

### GB.4 补回归测试

```rust
#[test]
fn 确认令牌是32位十六进制且每次不同() { ... }
```

锁住两点：**格式不变**（32 个十六进制字符）与**每次不同**。
全量测试 **749 → 750**。

### GB.5 门禁抓到了文档过期（这是门禁存在的意义）

新增回归测试后测试数 **749 → 750**。`doc-numbers-check.py` **正确地**报了不一致：

```
① 测试数：文档 749 / 实测 750
[FAIL] 文档里的数字与来源不一致
```

修了**收尾报告**里的三处（749 → 750；顺带把同一行的"19 项门禁"改成"22 项"）。

**历史附录里的 749 保留不改** —— 它们是**当时的记录**。
（这正是附录 GA 记的教训：**修正历史记录要用"追加更正"而不是"改写原文"**。
GA 批我改错了，这批做对了。）

### GB.6 验证

* `cargo test --workspace` → **750 通过 / 0 失败**（8 个测试目标）；
* 构建 0 错误 0 警告；`gates.py` → 22 项全过。

---

## 附录 GC：整体审计**第 2 遍** —— 数据流 / 信任边界（2026-10-06 批次 GC-1）

> AGENTS.md："所有任务全部完成后，整体执行审计 3 遍"。这是**第 2 遍**，
> 角度：**数据流与信任边界**（谁能连进来、收到什么、边界上怎么校验）。

### GC.1 审计对象

`aetherd` 的 IPC 入口（`src/server.rs` 的监听建立与连接处理、
`aether-ipc` 的对端校验、`src/clipboard.rs` 的数据通道）。

### GC.2 逐条核对（**未发现新问题**）

| 边界 | 实现 | 证据 |
|---|---|---|
| **谁能连** | Unix socket 权限 `0600` | `server.rs:106` |
| **对端身份** | `aether_ipc::check_peer`，读不到凭据即拒绝（**fail-closed**） | `server.rs:115`；注释明确写着"**权限位正确**"与"**真的只接受预期身份**"是两件事 |
| **连接占用** | **读、写超时都设** | `server.rs:119/124`；注释解释了"只设读超时会被'连上不读'占满槽位" |
| **非回环监听** | **必须显式 `AETHER_ALLOW_REMOTE_IPC=1`，否则 `exit`** | `server.rs:151`；并打印"IPC 无认证，请确认网络可信" |
| **单条消息大小** | `MAX_LINE_BYTES = 1 MiB`，**按行**判 | `server.rs:16/334` |
| **连接数** | `MAX_CONNECTIONS = 32`，超限立即拒绝 | `server.rs:18/71` |
| **剪贴板通道** | `MAX_BYTES = 64 KiB`，**拒绝整段不做截断**（fail-closed） | `clipboard.rs:27/50` |

### GC.3 值得记一笔：一个"改对了"的历史 bug

`server.rs:297` 的注释记录了曾经的**两个**相反方向的错误：

> 原来用 `BufReader::take(MAX_LINE_BYTES)`：`take` 作用在**读取器**上，是**连接级累计**上限
> —— 一条连接累计读满 1MB 后被静默关闭（**正常的长连接、多轮对话会莫名断开**），
> 而**单条超长行反而不受这个限制约束**（它在累计到 1MB 之前就能把内存吃满）。

改成**按行判**之后，两个方向都对了。**并且有精确的回归测试**（`server.rs:1577`）：
"按行限长必须**精确** —— 恰好上限、上限+1、带不带换行**四种组合**"。

—— 这正是本会话反复强调的做法：**限长这类判据要卡在边界值上**，
"恰好"和"恰好+1"都必须有用例。

### GC.4 结论

**IPC 的信任边界是稳的**：入口有身份校验、有超时、有连接数上限、有按行的消息上限，
非回环监听必须显式开关；剪贴板数据通道有 64 KiB 上限且**拒绝整段**。
本遍审计**未发现需要修的问题**。

### GC.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace` → 750 通过 / 0 失败；
* `gates.py` → 22 项全过。

---

## 附录 GD：整体审计**第 3 遍** —— 文件与路径（2026-10-06 批次 GD-1）

> AGENTS.md："所有任务全部完成后，整体执行审计 3 遍"。这是**第 3 遍（最后一篇）**，
> 角度：**文件与路径**（路径遍历、符号链接、凭证文件、临时文件、权限位）。
> 前两遍：GB（权限与确认流程）、GC（数据流 / 信任边界）。

### GD.1 审计对象

`aetherd/src/tools.rs` 的路径策略（`resolve_readable` / `check_read_policy` /
`check_write_policy` / `READ_ALLOWED_ROOTS` / `READ_DENY_SUBPATHS`）、
`timesync.rs` 的文件名处理、`apps.rs` 的 `canonicalize` 用法。

### GD.2 逐条核对（**未发现新问题**）

| 关注点 | 实现 | 判断 |
|---|---|---|
| **白名单 vs 黑名单** | 用白名单，注释写明理由："黑名单永远漏（符号链接、大小写、`..` 回绕、`/proc/self/cwd` 之类的间接路径）" | ✅ 论证本身是对的 |
| **可读根** | `/home`、`/etc/aether`、`/var/log/aether`、`/run/aether`、`/var/apps`、`/tmp` | ✅ 系统区一律不可读 |
| **凭证子路径** | `.ssh`、`.gnupg`、`.aws`、`ui.key` —— **即使落在白名单根内也拒** | ✅ 纵深防御 |
| **可写范围** | **更窄**：只允许用户数据区（家目录、`/tmp`），理由是"否则等于让 AI 改自己的权限规则与审计记录" | ✅ 边界划得对 |
| **`..` 与符号链接** | `canonicalize` 解析二者 → "两条绕行路径因此被堵死" | ✅ |
| **错误顺序** | **先 `canonicalize` 再查白名单** —— 越权路径得到的是"无法访问"，不泄漏"是否存在" | ✅ 不泄漏信息 |
| **目标尚不存在** | 新建/重命名时 **canonicalize 父目录** | ✅ 常被漏掉的细节 |
| **读取体量** | `read_file` 截断 4000 字符 | ✅ |
| **`timesync` 文件名** | 拒绝 `..`、绝对路径、反斜杠、空串，**写前再 `canonicalize` 确认** | ✅ 二次确认 |

### GD.3 值得整段引用的一段注释（高危 H-1）

`tools.rs:737` 起记录了一次**真实的高危发现**：

> `ui.key` 是 2026-10-02 安全审计补上的（**高危 H-1**）：UI 通道密钥此前落在
> `/var/log/aether/ui.key`，而 `/var/log/aether` **正好在读取白名单里** ——
> 于是 `read_file`（L0、免确认、不要求已注册）就成了"**任意本机进程取走 UI 密钥**"
> 的现成工具，拿到密钥即可注册为 UI 通道并**自我确认 L3 操作**。
> 密钥文件本身是 0600，但"**以 root 运行的 aetherd 代读**"绕过了文件权限。
> **双保险**：这里拒绝（拦读取路径）+ `server::UI_KEY_PATH` 已迁到 `/run/aether/`。

**这段记录的价值在于它把"攻击链"写全了**：白名单包含了一个恰好放着密钥的目录 →
免确认的读取工具 → 以 root 代读绕过 0600 → 自我确认 L3。
—— 这正是本会话反复强调的"**判据要有正向对照、审计要写清利用链**"的同一思路。

### GD.4 三遍审计总结

| 遍 | 角度 | 结果 |
|---|---|---|
| 1 | 权限与确认流程 | **发现并修掉 1 处**：确认令牌随机性（`RandomState` → `/dev/urandom`） |
| 2 | 数据流 / 信任边界 | 未发现新问题（IPC 入口、消息上限、剪贴板上限都稳） |
| 3 | 文件与路径 | 未发现新问题（白名单 + canonicalize + 凭证子路径拒绝） |

### GD.5 验证

* 构建 0 错误 0 警告；`cargo test --workspace` → 750 通过 / 0 失败；
* `gates.py` → 22 项全过。

---

## 附录 GE：判据 ⑧ 盲点的**仪表已就位**（判据本身本批没接上）（2026-10-06 批次 GE-1）

### GE.1 要解决的问题

判据 ⑧ 从**像素**推文字带：两行完全叠在一起时会**合并成一个带**，于是看不见
（附录 FL 记过）。当时试过"带高 >= 2 倍中位数"的启发式，在正确代码上**误报 45/114**，已回退。

### GE.2 ✅ 做成了：让渲染器自己登记带区（**已验证**）

**思路**：不要从像素猜，**让画字的人自己说**。渲染器有精确的 `x`、`y`
（文档写明"`y` 为文字行顶部"）与 `px`，带区 = `[x, x+宽度] × [y, y+行高]`，
行高取**真实字体度量**（`horizontal_line_metrics` 的 ascent + descent），不估算。

* `text.rs`：加 `report_bands` 开关与 `bands` 列表（`set_band_report` / `take_bands`），
  在 `render` 开头登记；
* `main.rs`：`--dump-layout` 时输出 `"texts"` 字段。

**验证**（页0 @150%·en）：**292 段**带区，坐标精确：

```
291,45   68x18   aether
43,101  157x18   ← Parent folder
43,146   61x18   Home
```

### GE.3 ❌ 没做成：把判据接进 `layout-check.py`（已回退）

要在每组合循环里读 `d["texts"]`、两两判矩形重叠（`ox > 2 且 oy > 2`），
但**插了 3 次缩进都不对**（那段代码嵌套比看上去深）。按附录 DT 的规矩：
**三次不成不再试** → 用 `git checkout` **回退** `layout-check.py`（可靠手段）。

**所以：判据 ⑧ 的盲点还没关。** 本批的产出是**仪表**（最难的一半），不是判据。

### GE.4 下一步的确切做法（留给下一批）

在 `scripts/layout-check.py` 的每组合循环里（判据 ⑧ 那段附近）：

```python
ts = d.get("texts") or []
for i in range(len(ts)):
    for j in range(i + 1, len(ts)):
        a, b = ts[i], ts[j]
        ox = min(a["x"]+a["w"], b["x"]+b["w"]) - max(a["x"], b["x"])
        oy = min(a["y"]+a["h"], b["y"]+b["h"]) - max(a["y"], b["y"])
        if ox > 2 and oy > 2:
            BAND_OVERLAPS.append((page, scale, lang, a["text"][:24], b["text"][:24]))
```

先**跑全量看数量**（可能是**新的真实欠账**，像附录 FW 那样），再决定"逐条修"还是"先棘轮"。
**注意缩进**：那段代码的嵌套比看上去深，改前先打印目标区域。

### GE.5 验证

* 构建 0 错误 0 警告；`--dump-layout` 输出含 `texts`（292 段）；
* `layout-check.py --strict` → 退出码 0（**判据没有回归**：新字段不影响原有判据）；
* `gates.py` → 22 项全过。

---

## 附录 GF：判据 ⑪「两段文字带区重叠」接上了 —— ⑧ 的盲点有了替代（2026-10-06 批次 GF-1）

> 附录 GE 把**仪表**做好了（渲染器登记精确带区）但没接上判据。本批接上。

### GF.1 判据本身

`layout-check.py` 新增判据 ⑪：读 `--dump-layout` 的 `texts` 字段，
两两判矩形是否**在两个方向上都重叠超过 2px**。**不用任何启发式** ——
这正是附录 FL 那次失败的教训（"带高 >= 2 倍中位数"在正确代码上误报 45/114）。

### GF.2 三次收窄（每次都有数据支撑）

| 版本 | 命中 | 问题 | 收窄方式 |
|---|---|---|---|
| 第一版 | **1914** | 不同窗口的文字互相"重叠"（窗口本来就会互相盖住） | 按 `clip` 分组 |
| 第二版 | **431** | 实测 303 段带区里 **259 段 `clip=null`** —— `set_clip` **只有设置窗口在调** | 只比**有 clip** 的组（就是设置窗口） |
| 第三版 | **23** | —— | 这 23 处**都是真的** |

**这条路径本身就是附录 FS 那条经验的复现**：**判据的范围要用代码里已有的状态界定**，
不要用白名单。这里的"已有状态"是 `clip`。

### GF.3 修掉 3 处真 bug

| 页 | 症状 | 根因 | 修法 |
|---|---|---|---|
| 1 | `-1` 与组标题「时间同步」压住 | 时区按钮行（`y: ry + 6`、高约 `CTRL_H`）之后 `ry` **只前进了 `GAP_TIGHT`** | 前进量改成 `CTRL_H + 6 + GAP_TIGHT` |
| 0 | 长提示折行的续行压住下一行 | 提示串过长 | 缩短（中英一起） |
| 16 | 同上 | 同上 | 缩短（中英一起） |

顺手还修了一个**规则 ③ 漏掉的裸数字**：`content.x + 124`（那一行里还有
`theme::size::px(42)`，于是规则 ③ 的"行内无 `px()`"条件把它跳过了 —— **又一处缺口，如实记**）。

### GF.4 还剩 12 处：**如实记账**（棘轮 12）

修完上面 3 处后从 23 降到 **12**，都集中在页0 / 页16，且配对里出现 `'§3.5）'` ——
**那是上一条提示的结尾**，说明**真正折行的是它上面那条**，缩短下面那条没用。

按附录 DT 的规矩（同一处不成不再试），本批**停手**，棘轮定在 **12**：
**这是一笔明确的欠账**，且**棘轮保证不再变多**。下一步该动的是页0/页16 **上面那条**提示串。

### GF.5 范围说明（**重要，如实记**）

判据 ⑪ **目前只覆盖设置窗口**：它按 `clip` 分组，而 `set_clip` **只有设置窗口在调**。
其余窗口（文件、终端、音乐、AI 栏）的带区 `clip=null`，**没有被判据覆盖**。

要覆盖它们，需要渲染器知道"我正在画哪个窗口"（一个窗口 id）——
那是**下一步**，做法与 `clip` 同源（都是"用已有状态界定范围"）。

**但⑧的盲区已经有关键替代**：附录 FL 那个 bug（设置行压住下一行）**正是设置窗口里的**，
⑪ 现在能抓到它。

### GF.6 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、**⑪ = 12（棘轮 12）**、退出码 0；
* 构建 0 错误 0 警告；`scale-audit.py --strict` → 五类全 0；
* `shot-diff.py save` → 基线已重标定（文字与间距变化属有意改动）；
* `gates.py` → 22 项全过。

---

## 附录 GG：判据 ⑪ 的 12 处**是同一个根因** —— 一处修好全部（2026-10-06 批次 GG-1）

### GG.1 根因（`draw_wrapped` 的前进量）

```rust
let step = theme::size::px(20);
let segs = tr.wrap(text, px, max_w, 3);
for (i, seg) in segs.iter().enumerate() {
    tr.draw(buf, w, h, x, (*ry + i as i32 * step) as f32, seg, px, rgb, alpha);
}
*ry += (segs.len() as i32 - 1) * step;   // ← 只前进到**最后一行的开头**
```

`ry` 的约定是"**下一个元素的 y**"（所以单行绘制之后调用方加 `LINE_GAP` 就是行距）。
但折行版只前进到**最后一行的开头**，调用方再加一个 `GAP_TIGHT`（比行高小得多）
→ **下一个元素压在最后一行上**。

判据 ⑪ 报的 12 处**全是这一个根因**：

* 页0：`对比度提到 WCAG AA` 压住上一条提示的续行；
* 页16：`提示音` 压住 `在控制中心里打开`；
* 语言行：那条值压住下面的对比度提示。

**修法**（一行）：前进量补上**最后一行自身的高度**（用 `LINE_GAP`，与单行绘制后的行距同一常量）：

```rust
*ry += (segs.len() as i32 - 1) * step + theme::size::px(theme::size::LINE_GAP);
```

**结果**：⑪ **12 → 0**，接成**硬判 0**。

### GG.2 ⚠️ 一条方法论：**先找共同根因，不要逐条修症状**

上一批我按"页0/页16 各一条"去**逐条缩短字符串** —— 改了两条，⑪ **还是 12**。
这次去看**画字的那个函数**，才发现 12 处是**一个** bug。

> 判据报多处时，**先看它们是不是同一个根因**。逐条修症状会白改，而且改完还看不懂为什么没效。
> 这与本会话的"**在唯一收窄点修，而不是逐个调用点打补丁**"是同一条。

### GG.3 ⚠️ 顺带修掉我自己造成的回归

上一批我把英文 `notification center opens from the control center (…).`
替换成 `opens from the control center`，和前缀 `The ` 拼成了
**"The opens from the control center"** —— **语法错误**。

**本批改成 `Opens from the control center`。**

教训：**替换一个字符串片段时，要看它和前后缀拼起来是什么**。
（`i18n-check` 只能查"英文位有没有中文"，查不出语法；⑪ 也查不出。
这类问题只能靠**替换后回读整句**来防 —— 本批就是这么发现的。）

### GG.4 实拍复核

页16 @150%·en：`Opens from the control center` 与下方 `Sounds` **间距正常**，
折行说明 `When off, nothing is recorded (…)` 仍是两行且不压任何东西。

### GG.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、**⑪ = 0（硬判）**、退出码 0；
* 构建 0 错误 0 警告；`shot-diff.py save` → 基线已重标定；
* `gates.py` → 22 项全过。

---

## 附录 GH：给渲染器加**窗口身份**；判据 ⑪ 的放宽**没做成**（已回退）（2026-10-06 批次 GH-1）

### GH.1 ✅ 做成了：窗口身份（**已验证**）

* `text.rs`：加 `win_id: Cell<Option<u32>>` 与 `set_win_id`，带区元组多带一个窗口号；
* `draw.rs`：窗口绘制循环（`for (i, win) in desktop.wins.iter().enumerate()`）里
  每个窗口设一次，**循环后重置为 `None`**（循环外的文字——桌面、Dock——不该继承
  最后一个窗口的身份）；
* `main.rs`：`--dump-layout` 输出 `"win"` 字段。

**验证**（页0 @100%）：文字按窗口分成 `{1: 187, 3: 57, 2: 39, 0: 18}` —— 身份是对的。

**顺带**：这一处又踩到附录 DI 记过的事 —— 那个位置的 `tr` 是 `Option`，
要写成 `if let Some(t) = tr { t.set_win_id(...) }`。

### GH.2 ❌ 没做成：判据按窗口分组（已回退）

把 ⑪ 从"按 `clip` 分组（只有设置窗口）"放宽到"按窗口分组（所有窗口）"后，
⑪ 报 **108 处** —— 但**看内容**就明白：

```
'系统时区：未知（读不到 /etc/timezon' ∩ '好的，已把窗口排成两列。' = 143.4x5.2px
'非 Linux 目标：无网络接口信息'        ∩ '好的，已把窗口排成两列。' = 83.9x15.2px
```

`'好的，已把窗口排成两列。'` 是 **AI 气泡** —— 它是**浮层**，
**本来就该盖住别的文字**。命令面板、提示条同理。**那不是 bug。**

要覆盖浮层，判据得先**能识别浮层**（哪些窗口是浮层、浮层与谁比是允许的）——
那是另一件事。所以判据**回退到 `clip` 版**（已验证 0），**保留窗口身份这个仪表**。

### GH.3 一条方法论：放宽判据要看**内容**，不能只看数量

本批 108 处，若只看数量会以为"发现了一堆 bug"；看内容才知道是浮层的合法覆盖。
**放宽判据要一步一个数据**（附录 FS / GF 都是这么走的：1914 → 431 → 23 → 0），
而且每一步都要**抽查命中的内容**。

### GH.4 下一步（要覆盖浮层的话）

1. 在窗口上标注**是否浮层**（`Win` 加一个 kind，或在绘制循环里判断）；
2. 判据里：**浮层与普通窗口之间不比**；浮层之间、同窗口内部照比；
3. 再跑全量，**抽查内容**，再决定放宽还是收紧。

### GH.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、**⑪ = 0**、退出码 0；
* 构建 0 错误 0 警告；`--dump-layout` 的 `texts` 带 `win` 字段（已验证分组）；
* `gates.py` → 22 项全过。

---

## 附录 GI：判据 ⑪ 现在覆盖**所有窗口**（108 → 0）（2026-10-06 批次 GI-1）

### GI.1 上一批为什么报了 108 处

附录 GH 里我把 ⑪ 放宽到"按窗口分组"，结果 108 处、绝大多数是 AI 气泡。
根因有两层：

1. **AI 气泡继承了窗口 #3 的身份**（实测 `win=3`）——
   因为 GH 那批我写的"循环后重置 `set_win_id(None)`"**根本没插进去**：
   那段插入是**条件式**的（`if count == 1`），**没命中时脚本静默跳过**，
   而我只在成功时打印，于是**以为加上了**。
2. 即使重置正确，`Win` 里本来就有 `floating: bool` —— 浮动窗口**本来就该盖住别人**，
   按普通窗口分组仍会互相报。

### GI.2 ⚠️ 教训：**插入脚本必须显式报告"未命中"**

条件式插入（`if count == 1: 替换`）**静默跳过**是这次绕远的直接原因：
判据报了一堆假命中，我却以为是"真发现"。

**做法**：插入失败要 `assert` 或至少 `print("✗ 未命中")`。
本会话后来改成了 `assert`，但 GH 那批还是旧写法 —— **同一类问题在脚本里要不一致地防住**。

### GI.3 本批的两处修正

**① 循环后重置**（位置：窗口循环 `}` 之后、桌面图标之前）：

```rust
if let Some(t) = tr { t.set_win_id(None); }
```

循环外的浮层（AI 气泡、命令面板、提示条）拿到 `None` → 判据**跳过**它们。

**② 浮动窗口用独立命名空间**（`Win` 已有的 `floating` 字段）：

```rust
let id = if win.floating { 1000 + i as u32 } else { i as u32 };
```

浮动窗口**内部**的重叠照查（它自己也该排整齐），**跨窗口**不比。

**验证**（页0 @100%）：

```
分组：{'1': 187, '1003': 43, '2': 39, '0': 18, 'None': 14}
气泡：win=None  '好的，已把窗口排成两列。'
```

（设置窗口的 id 是 **1003** —— 它本身就是浮动窗口，正好验证了命名空间生效。）

### GI.4 结果

**判据 ⑪ 覆盖所有窗口，实测 0**（108 → 0）。

**正向对照已经具备**：按窗口分组的那次实测 **108 处** —— 判据**确实会响**，
不是"从不触发的判据"。

### GI.5 验证

* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、**⑪ = 0**、退出码 0；
* 构建 0 错误 0 警告；`--dump-layout` 的 `texts` 带 `win`（浮层为 null、浮动窗口为 1000+i）；
* `gates.py` → 22 项全过。

---

## 附录 GJ：判据 ⑪ 的**正向对照** —— 注入它为之而生的那个 bug，它判红了（2026-10-06 批次 GJ-1）

### GJ.1 为什么用这个注入

按本会话的规矩（附录 FD / FE / FF / FT）：**判据要能真的响**。
判据 ⑪（"两段文字带区重叠"）此前只在放宽过程中见过非零值，
**没有用它"本来要抓的那个 bug"验证过**。

那个 bug 就是附录 GG 修掉的 `draw_wrapped` 前进量问题。所以对照就用它：

```rust
// 修好的写法
*ry += (segs.len() as i32 - 1) * step + theme::size::px(theme::size::LINE_GAP);
// 注入（旧写法，只前进到最后一行的开头）
*ry += (segs.len() as i32 - 1) * step;
```

### GJ.2 结果

```
⑪ 两段文字带区重叠（>2px）：12
退出码：1
  page-0@100%·zh：'中文 · 切换后左栏与页标题立即生效' ∩ '对比度提到 WCAG AA' = 121.2x15.8px
  page-16@100%·zh：'在控制中心里打开' ∩ '提示音' = 36.0x7.8px
  ...
```

**判据 ⑪ 判红了** ✅，而且命中的正是附录 GG 修掉的那 **12 处**（连页面和配对都一样）。

**判据不只是"会响"，它响的正是它为之而生的那个 bug。**

回退：从内存里的原文写回（`finally`），`git status` 确认工作区 **0 改动**、构建干净。

### GJ.3 为什么不做成常驻门禁

全量 `layout-check` 约 **6 分钟**（102 个组合 + 每次一次额外渲染），
塞进门禁会让每次提交都多等 6 分钟。所以这份对照以**可复现的手工步骤**记录在此
（改哪一行、期望什么结果、怎么回退都写清了）—— 与附录 FT 的做法一致。

### GJ.4 判据 ⑪ 的完整状态

| 项 | 状态 |
|---|---|
| 覆盖范围 | **所有窗口**（浮层跳过、浮动窗口独立命名空间） |
| 当前实测 | **0** |
| 会响 | ✅（本批：注入真 bug → 12 处、退出码 1） |
| 依赖 | 渲染器登记的精确带区（`--dump-layout` 的 `texts`） |

### GJ.5 验证

* 注入 → ⑪ = 12、退出码 1；回退 → 工作区 0 改动、构建 0 错误 0 警告；
* `layout-check.py --strict` → ① ⑤ ⑥ ⑦ ⑧ 全 0、⑨ = 0、⑩ = 0、⑪ = 0、退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GK：收尾报告被"自我延续的孤立行"污染；给数字判据加"恰好一处"（2026-10-06 批次 GK-1）

### GK.1 发现

收尾报告（读者的入口）**第一行**是：

```
| 审计附录 | 134 篇 |# AetherOS 收尾报告（2026-10-04）
```

—— 一条**孤立的表格行**被插到了**文件开头**，破坏了文档结构。

**它是自我延续的**：本会话多个脚本用
`rt[:m.start()] + row + rt[m.end():]` 去更新这一行。一旦行跑到开头，
**第一个匹配永远是它**，于是**真正那一行再也不会被更新** ——
本批实测它还是 **112 篇**（当时应是 134）。

而 `doc-numbers-check.py` 的 `declared()` **只读第一个匹配**（`re.search`），
所以它读的是那条孤立的、**看不见**真正那行已经过期。

### GK.2 本批修正

| 位置 | 改前 | 改后 |
|---|---|---|
| 报告第一行 | 孤立的 `\| 审计附录 \| 134 篇 \|` | **删除** |
| §1 表 审计附录 | 112 篇 | **134 篇** |
| §6 验收命令注释 | 19 项门禁 | **22 项门禁** |
| §4.2 整体审计表 | 记到第 5 遍 | 补**第 6~8 遍**（GB / GC / GD） |
| §8 方法学 | 7 条 | **14 条** |

### GK.3 加防：数字判据要求"恰好一处"

`declared()` 现在先 `findall`：

```python
_hits = re.findall(pattern, text)
if len(_hits) != 1:
    problems.append(f"「{pattern}」在报告里出现 {len(_hits)} 次（应为 1 次）")
    return None
```

这样"插到开头"（两处）与"被删掉"（零处）都会**当场暴露**。

**正向对照**（本批做过）：在报告开头注入第二处 → 退出码 **1**；
回退 → 退出码 **0**、工作区无残留。

### GK.4 一条方法论（第 15 条）

> **"第一个匹配"式的读取会让重复项永远隐身** —— 判据要要求"**恰好一处**"，
> 而不是"至少一处"。

这条与第 7 条（手抄数字必然失实）同源：都是"**让判据去核对事实，而不是核对副本**"。
区别是第 7 条管"数字对不对"，这条管"**这个数字是不是被更新过**"。

### GK.5 验证

* `doc-numbers-check.py` → ① 750/750 ② 22/22 ③ 134/134、退出码 0；
* 注入第二处 → 退出码 1；回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GL：README 的数字**没人管**（444 → 750）；把门禁扩到 README（2026-10-06 批次 GL-1）

### GL.1 发现

README（**最常被读的文件**）里：

* "**444 项单元测试**"出现在 **4 处** —— 而实际是 **750**；
* "**19 张**归档走查图逐像素比对，当前 19/19 零差异" ——
  `assets/ui-archive/` 实际 **20 张**，而**同一份 README** 第 294 行又写 "**20 张**"，
  **自相矛盾**；而且"逐像素比对 19 张图"这个**说法本身也过期**了：
  真正的像素回归是 **57 档 `--shot` 逐字节比对**。

**根因**：`doc-numbers-check.py` **只查收尾报告** —— README 的数字**没人管**。
附录 GK 刚给那份报告加了"恰好一处"的守卫，但**覆盖范围**没跟着扩。

### GL.2 本批修正

| 位置 | 改前 | 改后 |
|---|---|---|
| README（4 处） | 444 项单元测试 | **750 项** |
| README 视觉回归行 | "19 张归档走查图逐像素比对，19/19 零差异" | "**57 档 `--shot` 逐字节比对**，当前 57/57 零差异" |
| 收尾报告 | 49,893 行 | **50,528 行**（`repo-stats.py` 权威；代码长了报告没跟） |

> 顺带确认：README 第 12 行的 "50,528 行 / 71 个源文件" **是对的** ——
> 过期的是**收尾报告**。所以"哪份文档过期"不能靠猜，要**跟权威口径比**。

### GL.3 加防：门禁扩到 README

`doc-numbers-check.py` 增加 **③b README** 一节：检查**测试数**与**源文件数**
（同样要求"恰好一处"），源文件数从 `repo-stats.py` 的输出推导。

**正向对照**（本批做过）：把 README 的 750 改成 749 → 退出码 **1**，报
"README 测试数：文档 749，实测 750"；回退 → 退出码 **0**。

### GL.4 ⚠️ 顺带修掉我自己守卫里的一个 bug

`declared()` 里我用了 `problems` —— 那是 `main()` 的**局部变量**。
之前没暴露，是因为那些调用**恰好一处匹配**，走不到 `problems.append`；
README 的措辞没匹配上（我按收尾报告的 `**750 项全绿**` 想当然了，
README 实际是 `| **验证** | 750 项单元测试全绿`），于是当场 `NameError`。

> **教训（第 16 条）：新加的守卫要立刻用"不匹配"的输入试一次** ——
> 否则它的**失败路径**根本没被执行过。这次是"恰好没走到"才没早暴露。

### GL.5 验证

* `doc-numbers-check.py` → ① 750/750 ② 22/22 ③b README 750/750 · 71/71 ③ 135/135、退出码 0；
* 注入 749 → 退出码 1；回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GM：**系统扫查**过期数值，又抓到 4 个文件（2026-10-06 批次 GM-1）

### GM.1 为什么要"扫"而不是"改看到的那个"

附录 GL 修了 README 与收尾报告。但**同类过期值可能还在别处** ——
所以本批把已知的过期值在**全部文档**里扫了一遍：

`444`（旧测试数）· `49,893`（旧行数）· `19 张` / `19/19`（旧视觉回归说法）·
`112 篇`（旧附录数）· `18 项门禁` / `19 项门禁`（旧门禁数）。

**又抓到 4 个文件**：

| 文件 | 命中 | 说明 |
|---|---|---|
| `README.en.md` | 444 ×3、19/19 | **英文入口** —— 上一批只改了中文那份 |
| `docs/代码索引（INDEX）` | 444 ×2、19 张 | README 明说"看现状请用 INDEX"，是**现状文档** |
| `docs/路线图与已知缺口（ROADMAP）` | 444 ×1 | —— |
| `docs/报告/安全审计` 正文 | 444 / 19 张 | 正文的**发现记录**，保留；附录是历史记录，保留 |

**教训（第 17 条）：中英成对的不只是界面文案，文档也是。**
附录 DV / FJ 那条"只改一边等于没改"的教训，这次发生在 `README.md` 与 `README.en.md` 之间。

### GM.2 顺带查清"19 张"到底是什么

审计正文第 465 行早就记着：

> `| 12 | 走查图张数：README 10 张 / INDEX 19 张（同文件另处又写 10） | 实际 20 张文件 / 19 张在门禁清单 | ❌ 三处不一致 |`

所以"**19 张**"指的是**归档工具 / 门禁清单**的张数，**不是**像素回归的规模；
而"逐像素比对 19 张图"那句该说 **57 档 `--shot`**。

**结论**：`INDEX` 里的"19 张"**是归档工具的计数，正确，保留**；
README（中英两份）里"19/19 零差异"**是过期说法**，本批改成 57 档。

—— 这条也说明：**同一个数字在不同文档里可能指不同东西**，
不能见"19"就改，要先查它指的是什么。

### GM.3 加防

`doc-numbers-check.py` 也检查 **`README.en.md`** 的测试数（中英成对）。
现在这份门禁覆盖：**收尾报告**（测试 / 门禁 / 附录）+ **README 中英两份**（测试 / 源文件数）。

### GM.4 验证

* `doc-numbers-check.py` → ① 750/750 ② 22/22 ③b 中文 README 750/750 · 71/71
  ③c 英文 README 750/750 ③ 136/136、退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GN：英文 README 的规模**差了 20,370 行**；把它加进门禁（2026-10-06 批次 GN-1）

### GN.1 发现

`README.en.md` 第 19 行：

```
| **Scale** | 30,158 lines of Rust / 48 source files / 7 crates (measured 2026-10-02) |
```

而权威口径（`repo-stats.py`）是 **50,528 行 / 71 文件** —— **差了 20,370 行**。

### GN.2 ⚠️ 为什么上一批（GM）的扫查没发现

上一批我用**中文的措辞与数字**（`50,528`、`71 个源文件`、`444`）去英文那份里找 ——
**没命中，就以为"英文 README 没有规模数字"**。实际它写的是**另一套数字**（30,158 / 48）。

> **教训（第 18 条）："没找到"不等于"没有"。**
> 要按**语义**找（"规模那一行"），不要按**具体数字**找 ——
> 否则"数字本身错了"这种最该抓的情况，恰好**搜不到**。

### GN.3 顺带发现：门禁早就有，只是清单少一份

`repo-stats.py --check` **早就在门禁里**（`gates.py:151`），而且覆盖**三层**：
合计 / 逐 crate / 逐文件（71 条）+ INDEX 的测试分布表。**规模数字早就有门禁。**

但它检查的清单是：`代码索引（INDEX）`、`README.md`、`ROADMAP` ——
**没有 `README.en.md`**。本批把英文那份加进去
（只比对**合计**层，它没有逐 crate 表），并加了对应的声明正则：

```python
# README.en.md 的规模行： | **Scale** | 50,528 lines of Rust / 71 source files / 7 crates
DECL_README_EN: Pattern[str] = re.compile(
    r"\*\*Scale\*\*\s*\|\s*([\d,]+)\s+lines of Rust\s*/\s*([\d,]+)\s+source files"
)
```

**正向对照**：把英文那份的 `71 source files` 改成 `48` → 退出码 **1**，
报 `[FAIL] README.en.md: 声明 50,528 行 / 48 文件，实测 50,528 行 / 71 文件`；
回退 → 退出码 **0**。

### GN.4 这个脚本的注释里记着同一类历史事故

`repo-stats.py` 的模块注释写着：

> 2026-09-29 复核时 README 的 `aetherd` 行数与实测**差了 1,130 行**，合计差了 1,393 行。
> 于是"**README 写 15,890 / INDEX 写 15,926 / 实测 16,021**"这种三方不一致照样全绿
> —— 门禁给了**虚假保证**。

—— 与本轮 GL / GM 是**同一类问题**（文档数字漂移）。
**这类问题早就被记过、也早就建了门禁，但"覆盖范围"没跟上**（英文那份漏了）。

**这说明一件事**：修完一类问题之后，要问的不是"修好了吗"，
而是"**同一类问题的覆盖范围，有没有漏掉的地方**"（中英两份、报告与 README、四份规模文档）。

### GN.5 验证

* `repo-stats.py --check` → INDEX / README.md / **README.en.md** / ROADMAP 全部
  50,528 行 / 71 文件；逐 crate、逐文件、测试分布表全过；退出码 0；
* 注入 `48 source files` → 退出码 1；回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GO：扫**已发布站点**；INDEX 头部那句被门禁**漏掉**（2026-10-06 批次 GO-1）

### GO.1 为什么扫站点

`site/` 是 `gen-site.py` 生成的**已发布**产物 —— 最公开的面。
安全审计正文第 623 行早就记过：**站点"17 张界面走查图"曾是手写常量**，与门禁清单长期不一致。

### GO.2 站点的两个数字：**都不是过期**（查清了）

| 位置 | 内容 | 判断 |
|---|---|---|
| `site/index.html:108` | "19 张界面走查图逐像素回归" | **对**。`gen-site.py:85` 写明"口径是**被逐像素门禁覆盖的张数**… 当前目录 20 张、**清单 19 张**"；而且这个数字由 `gen-site.py:430` 的 `re.sub` **从实测推导**，不是手写 |
| `site/changelog.html:293` | `"17 张界面走查图"与门禁清单的 19 张不符` | **对**。这是**引用审计的历史发现**（"曾经是手写的 17"），不是现状声明 |

—— 所以站点这次**没有**同类问题。**但"扫了才发现"，不能靠猜。**

### GO.3 真问题：INDEX 头部那句

`docs/代码索引（INDEX）` 第 3 行：`源码 **30,158 行**` —— 而权威口径是 **50,528**。

**`repo-stats.py --check` 为什么放过它**：它的 `DECL_INDEX` 只匹配**合计行**
（`| **合计** | **50,528** | **71** | |`），**头部那句是同一个事实的第二种写法**。

> **教训（第 19 条）：门禁覆盖的是"它认识的那种写法"** ——
> **同一个事实的第二种写法就没被覆盖**。
> 这与附录 GN 是同一条的延伸（GN 是"少覆盖了一个文件"，这里是"少覆盖了一种写法"）。

### GO.4 修法（含我自己踩的一个坑）

1. INDEX 头部改成 `源码 **50,528 行 / 71 文件**`（**带上文件数**）；
2. `repo-stats.py` 加 `DECL_INDEX_HEAD` 正则 + 一条三元组，**独立成一项**（标签写明"头部行"）。

**我踩的坑**：第一版正则只写了**一个捕获组**，而 `declared()` 返回的是
**(行数, 文件数) 二元组** —— 检查时拿 int 比二元组，**必然不等**，门禁当场变红。
修法是让头部也带上文件数，正则改成两个捕获组。

> 这又一次印证第 16 条：**新加的守卫要立刻跑一次**（而且要跑**通过**的那一次，
> 不只是注入失败的那一次）—— 我这次是先看到"通过"失败，才发现的。

### GO.5 验证

* `repo-stats.py --check` → 五项全 OK（INDEX 合计 / **INDEX 头部行** / README.md /
  ROADMAP / README.en.md），逐 crate、逐文件、测试分布表全过；退出码 0；
* 注入 `30,158` → 退出码 1，报
  `[FAIL] 代码索引（INDEX）-2026-09-11.md 头部行: 声明 30,158 行 / 71 文件，实测 50,528 行 / 71 文件`；
  回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GP：把"**同一个事实的全部写法**"纳入门禁（2026-10-06 批次 GP-1）

### GP.1 做法

附录 GO 的教训是"门禁只认它认识的那种写法"。本批把它**系统化**：
对每个受保护的事实（测试数 / 门禁数 / 附录数 / 行数 / 文件数），
在**现行文档**里找出**每一处说法**，逐条判断是否被覆盖。

### GP.2 找到 4 处"第二种写法"没被覆盖

| 文件 | 写法 | 覆盖情况 |
|---|---|---|
| `README.md:13` | `750 项单元测试全绿` | ✅（GL 加过） |
| `README.md:260` | `Windows 750 项全绿` | ❌ **本批补** |
| `README.en.md:20` | `750 unit tests green` | ✅（GM 加过） |
| `README.en.md:28` | `Beyond the 750 unit tests` | ❌ **本批补** |
| `收尾报告:21` | `**750 项全绿**` | ✅ |
| `收尾报告:132` | `「750 项测试 / 22 项门禁」` | ❌ **本批补** |
| `收尾报告:138` | `# 22 项门禁（含…` | ❌ **本批补** |

（`ROADMAP:111` 的 "`cargo test` 65 项全绿" **保留不改** ——
那是**里程碑勾选项**，记录的是当时的状态，不是现状声明。）

### GP.3 修法：一张"事实表"，而不是给每种写法写一个函数

`doc-numbers-check.py` 新增 ③d 一节：

```python
for _label, _path, _pat, _expect in (
    ("README.md Windows 行", README, r"Windows (\d+) 项全绿", real_tests),
    ("README.en Beyond 行", README_EN, r"Beyond the (\d+) unit tests", real_tests),
    ("收尾报告 自引用行", REPORT, r"「(\d+) 项测试 / (\d+) 项门禁」", (real_tests, real_gates)),
    ("收尾报告 验收命令注释", REPORT, r"# (\d+) 项门禁（含", real_gates),
):
```

**关键**：表里同时处理**单值**与**二元组**两种形状 ——
附录 GO 我就是栽在"正则只写一个捕获组，而 `declared()` 返回二元组"上。

**正向对照**：把 `README.md` 的 `Windows 750 项全绿` 改成 749 → 退出码 **1**，
并**精确定位**到 `③d README.md Windows 行：[749] / 期望 750`；回退 → 退出码 **0**。

### GP.4 一条方法论（第 20 条）

> **判据要按"事实"组织，不要按"文件"组织。**

原来的 `doc-numbers-check.py` 是**按文件**写的（收尾报告一节、README 一节、README.en 一节），
于是**同一个事实的每一种新写法都要记得去加一条** —— 漏了就静默漂移（GL / GM / GN / GO 连着四批）。
本批改成**按事实**列：一个事实一张表，表里列出它在**各处**的写法。

这与附录 FS / GF 那条"**判据的范围要用代码里已有的状态界定**"是同一思路的两面：
**范围**要覆盖全（不漏写法），**界定**要用已有状态（不写白名单）。

### GP.5 验证

* `doc-numbers-check.py` → ① 750/750 ② 22/22 ③b 750/750 · 71/71 ③c 750/750
  ③d 四处全部符合 ③ 139/139、退出码 0；
* 注入 749（第二种写法那一处）→ 退出码 1；回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GQ：把**发布事实**纳入门禁（ISO 体积 / 下载链接形态）（2026-10-06 批次 GQ-1）

### GQ.1 权威在哪

`site/release.json` —— `gen-site.py:105` 称它为"**已发布产物的权威事实**"，
并写明理由：官网要描述的是**用户实际下载到的那个文件**，不是本地工作区里的那份。

站点由它**生成**（`<!--DATA:iso_size-->` 占位符），所以**站点不会漂**；
但 **README 中英两份里的 `38.5 MB`（共 4 处）是手写的**。

### GQ.2 两条不变量（都来自 release.json 的权威说明）

1. **ISO 体积** = `iso_size`（`38.5 MB`）—— README 两份各 2 处，必须一致；
2. **下载链接形态** —— release.json 的 `_prerelease_note` 写着：

   > GitHub 的 `/releases/latest` 只返回**非预发布**版本。这个 Release 是 **prerelease**，
   > 所以任何指向 `releases/latest` 的链接都会 **404** —— 站内与 README **必须**用
   > `/releases/tag/<tag>`。

   所以：README 两份 + 站点四个页面里**不得出现 `releases/latest`**，
   且必须**含有**指向该 tag 的链接。

### GQ.3 而 release.json 的注释里记着**同一类历史事故**

> 两者曾经不一致：release 资产 40,327,168 字节 / sha256 7c50f481…，
> 而本地 ISO 40,359,936 字节 / 43c53f4c…，于是**下载页公布的校验和根本对不上
> 用户下载的文件**（2026-10-02 安全审计发现）。

—— 又一次是"**同一个事实在多个地方各写一份**"（附录 GL / GM / GN / GO / GP 是同一族）。
本批把**发布事实**也按"事实表"的方式纳入门禁。

### GQ.4 ⚠️ 我自己对照里的一个错误（第 21 条）

对照 2 的第一版**没生效**：README 里写的是 `/releases/**download**/v0.1.0/`，
我替换的却是 `releases/**tag**/v0.1.0` —— **没匹配上，文件根本没变**，于是退出码 0。

我差点把它读成"守卫没响"。实际是**对照没做**。

> **教训（第 21 条）：对照实验本身也要验证"注入真的生效了"。**
> 做法：注入后**先断言内容确实变了**（并从盘上读回来再确认一次），**再**跑判据。

重做后：注入 → 退出码 **1**，报 `README.md 用了 /releases/latest（prerelease 会 404）`；
回退 → 与原文**逐字节一致**、退出码 **0**。

### GQ.5 验证

* `doc-numbers-check.py` → ③e README.md / README.en.md ISO 体积均为 `38.5 MB`；
  站点四个页面无 `releases/latest` 且含 tag 链接；退出码 0；
* 对照一（体积写错 `39.0 MB`）→ 退出码 1；对照二（改成 `releases/latest`）→ 退出码 1；
* `gates.py` → 22 项全过。

---

## 附录 GR：干净重建抓到 **musl 下 3 条告警**；而门禁**一直是绿的**（2026-10-06 批次 GR-1）

### GR.1 起点：按 README 的建议做一次干净重建

README 的"关于 0 告警的重要限定"（附录 CB.2）写着：

> cargo **只对重新编译的 crate 重新报告告警**，所以增量构建下的"0 告警"
> **不等于**全量重建也没有告警。……**结论：验收时应周期性 `cargo clean` 一次。**

本会话一直增量构建，于是本批做了 `cargo clean -p aether-compositor` + 全量重建：

```
② 全量重建（统计告警）
   退出码 0 · 告警 0 条 · 错误 0 条
```

**Windows 侧确实 0 告警** ✅ —— 但接着逐 crate 查 musl：

```
aether-compositor    退出码 0 · 告警 3 · 错误 0
```

### GR.2 3 条告警是什么

1. `methods set_cjk_report, take_cjk_hits, set_band_report, take_bands, take_over_hits are never used`
   （`text.rs:295`）—— **本会话为判据 ⑩ / ⑪ 加的那批报表方法**；
2. `method code is never used`（`settings.rs:605`，`FormProblem::code`）—— **既有**的一处。

两者都是**只在 `--shot` / `--dump-layout` 走查路径被调用**，而那条路径在 Linux 目标下被 `cfg` 掉。

### GR.3 ⚠️ 而门禁**一直是绿的** —— 这才是真问题

`gates.py` 的 musl 步骤原来跑的是：

```python
["cargo", "check", "--target", "x86_64-unknown-linux-musl", "--all-targets", "-p", ...]
```

**只看退出码**。而 **cargo 有告警也返回 0** —— 所以这一步验证的是"**能否编译**"，
而 README 声称的是"**musl 侧 6/7 crate 0 警告**" ✗。

**这正是本会话第 10 条**："**一个从不触发的判据等于没有判据**"。
它绿了很久，而声称早已不成立（其中一条还是我自己引入的）。

### GR.4 修法

1. **告警本身**：给那 5 个方法 + `impl FormProblem` 加
   `#[cfg_attr(target_os = "linux", allow(dead_code))]` ——
   **照代码库既有先例**（`text.rs` 的 `dump_metrics` 就是这么写的，注释"仅 --fonttest 走查使用"）。
   修后：musl `aether-compositor` **0 告警**、Windows 全量 **0 告警**、6 个 crate 逐个 **0 告警**。
2. **门禁**：新增 `scripts/musl-warn-check.py` —— 逐个 crate 跑 musl check，
   **统计 `warning:` 行**（排除 `ring@` 构建脚本噪声），有告警就非零退出；
   用它**替换**原来那一步（**门禁数仍是 22**，自述表同步更新）。

**正向对照**（按第 21 条：先断言注入生效、并从盘上读回来确认）：
往 `impl TextRenderer` 里注入 `fn dead_probe_for_control() {}` → 退出码 **1**，
报 `[FAIL] aether-compositor: 1 条告警`；回退 → 与原文**逐字节一致**、退出码 **0**。

### GR.5 一条方法论（第 22 条）

> **"能编译"与"没有告警"是两件事，要分开判。**
> 编译器把告警当**非致命**，所以任何"只看退出码"的检查都**只覆盖了前者**。

这与第 10 条（判据要会响）互为表里：那条讲"判据要能失败"，
这条讲"**别把两种不同的事实混在一个退出码里**"。

### GR.6 验证

* `cargo clean -p aether-compositor` + 全量重建 → **0 告警 0 错误**；
* `scripts/musl-warn-check.py` → 6 个 crate 全部 **0 告警**、退出码 0；
* 注入死函数 → 退出码 1；回退 → 退出码 0；
* `gates.py` → 22 项全过。

---

## 附录 GS：同步脚本的"只认中文措辞"缺口 —— **同一个事实有两个消费方**（2026-10-06 批次 GS-1）

### GS.1 怎么发现的

附录 GR 修完 musl 告警后，门禁**红了** —— 因为我加的 `cfg_attr` 行让**行数变了**
（50,528 → 50,534），而文档没跟。这是**门禁正常发挥作用**。

于是跑 `sync-doc-numbers.py` 收敛，它却说：

```
第 1 轮无改动，停止
仍有 1 条未同步：
  [FAIL] README.en.md: 声明 50,528 行 / 71 文件，实测 50,534 行 / 71 文件
```

### GS.2 根因：替换模式**只有中文**

`sync-doc-numbers.py` 靠**解析 `repo-stats.py --check` 的 FAIL 消息**来定位"哪份文档写错了"，
但它的**替换模式只有 7 条，全是中文**：

```python
(f"{ol} 行 / {of} 文件", ...),
(f"{ol} 行 / {of} 个源文件", ...),
(f"{ol} 行 / {of} 个 .rs", ...),
(f"{ol} 行 Rust / {of} 个源文件", ...),
(f"{ol} 行 Rust / {of} 个源文件 /", ...),
(f"| **合计** | **{ol}** | **{of}** | |", ...),
(f"| **合计** | **{ol}** | **{of}** |", ...),
```

英文那句 `50,528 lines of Rust / 71 source files` **没人认领** ——
于是它**每轮都改不动**，收敛循环**永远差一条**。

### GS.3 ⚠️ 这是附录 GN 的**同一处疏漏**

附录 GN 我把 `README.en.md` 加进了 `repo-stats.py --check`（**检查方**），
**但没有加进 `sync-doc-numbers.py`（同步方）**。

> **教训（第 23 条）：同一个事实有**多个消费方**（检查、同步、生成……），
> 只改一处不够 —— 要顺着"谁在读这个事实"数一遍。**

这与第 20 条（判据要按事实组织）配套：
第 20 条讲**判据内部**要把各处写法列全；
这条讲**判据之外**还有别的消费者，也要一起改。

**修法**：给同步脚本补一条英文模式：

```python
# 英文措辞（附录 GR：README.en.md 那句原来没人认领，收敛循环永远差一条）
(f"{ol} lines of Rust / {of} source files", f"{nl} lines of Rust / {nf} source files"),
```

修后：

```
总量 README.en.md: 50,528/71 → 50,534/71
第 2 轮无改动，停止
已收敛：共改动 1 处，`repo-stats.py --check` 全部通过 ✓
```

### GS.4 验证

* `sync-doc-numbers.py` → 已收敛、`repo-stats --check` 全部通过；
* `repo-stats --check` → INDEX（合计 + 头部行）/ README.md / ROADMAP / README.en.md
  全部 **50,534 行 / 71 文件**；逐 crate、逐文件、测试分布表全过；退出码 0；
* 站点已刷新（`rust_lines=50,534`）；
* `gates.py` → 22 项全过。

---

## 附录 GT：同步脚本的"**假收敛**" —— 它只问一个判据（2026-10-06 批次 GT-1）

### GT.1 出发点：第 23 条

附录 GS 得出第 23 条："**同一个事实有多个消费方**（检查、同步、生成），只改一处不够"。
于是本批顺着它问：**还有哪些事实只有"检查方"、没有"同步方"？**

### GT.2 用实验回答（不读代码，做注入）

往收尾报告里注入三个错数字：**749 项测试 / 21 项门禁 / 142 篇附录**，跑同步脚本：

```
第 1 轮无改动，停止
已收敛：共改动 0 处，`repo-stats.py --check` 全部通过 ✓

测试数：749（未修）
门禁数：21（未修）
附录数：142（未修）
```

**一个都没修，而它宣布"已收敛"** ✗ —— **文档是错的，它却给出了"全部通过"的保证**。

### GT.3 根因

```python
def run_check() -> str:
    return subprocess.run(
        [sys.executable, "scripts/repo-stats.py", "--check"], ...).stdout
```

`run_check()` **只跑 `repo-stats.py --check`**（只管**规模**：行数 / 文件数 / 逐 crate），
**从不跑 `doc-numbers-check.py`**（管**测试数 / 门禁数 / 附录数**）。
于是后三类**既不同步、也不检查** —— 而收尾报告里它们都有。

### GT.4 修法

1. `sync_once` 增加**第 6 节**：跑 `doc-numbers-check.py`，解析它的三行
   （`① 测试数：文档 X / 实测 Y`、`② 门禁数：…`、`③ 审计附录：…`），
   不一致就把收尾报告里的数字改成**实测值**；
2. `run_check()` 改成**同时跑两个检查** —— 这样"已收敛"才是真的；
3. 收尾措辞从"`repo-stats.py --check` 全部通过"改成"**规模 + 数字两个门禁全部通过**"。

### GT.5 验证（重做同一个实验）

```
✓ 已注入 749 / 21 / 142（并确认写盘）
  数字 收尾报告（FINAL）-2026-10-04.md: 测试/门禁/附录 已按实测修正
  第 2 轮无改动，停止
  已收敛：共改动 1 处，规模 + 数字两个门禁全部通过 ✓
  测试数：750（已修）  门禁数：22（已修）  附录数：143（已修）
```

### GT.6 一条方法论（第 24 条）

> **"收敛"/"通过"这类断言，必须由"所有相关判据"共同支撑。**
> 只问一个判据就说"全部通过"，是**假声明**。

这条与第 23 条是同一族的两面：
第 23 条讲**改**的时候要顺着"谁在读这个事实"数一遍；
这条讲**断言**的时候要顺着"有哪些判据在看这个事实"数一遍。

**一个判据全绿，不等于事情是对的** —— 本会话第 10 条（判据要会响）说的是
"判据不能从不触发"，这条说的是"**不能只看一个判据就下结论**"。

### GT.7 验证

* 注入 749 / 21 / 142 → 同步脚本**全部修正**、退出码 0；回退 → 逐字节一致；
* `gates.py` → 22 项全过。

### GT.8 顺带：性能门禁红了，但**不是**本批引入的

跑全量门禁时 `性能门禁（P99 帧时间 ≤ 一帧预算）` 红了：P99 38~50 ms（阈值 33 ms）。
但**平均值只有 20.7 ms** —— 而 README 记的优化后是 15.5 ms，说明**整机慢了约 33%**。

宿主负载的证据（前 8 名进程 CPU 时间）：

```
fn-sync-client 6553.2s · Doubao 2274.1s · DeepSeek Harness 1148.6s · explorer 947.5s
msedge 792.3s · ChatGPT 484.5s · DeepSeek Harness 396.9s · msedgewebview2 371s
```

一堆 AI 客户端 + 同步客户端在跑。README 附录 T.6 早就记过同一现象：
"**性能门禁本会话曾多次因宿主负载飘红，已用同条件 A/B 对照证明不是回归**"。

**同条件 A/B**（照 T.6 的做法：暂存改动 → 重建 → 对比 → 恢复并核实）：

| 轮次 | 平均 | P99 | 退出码 |
|---|---|---|---|
| 首次（工作区） | 20.7 ms | 38~50 ms | 1 |
| **A：工作区** | 13.09 ms | **32.56 ms** | **0** |
| **B：HEAD（不含本批改动）** | 16.50 ms | **13.51 ms** | **0** |

**⚠️ 诚实说明：这次 A/B 不是干净的对照。** 三轮的 P99 是 38~50 → 32.6 → 13.5 **单调下降**，
说明**宿主负载在两次之间变了** —— 负载本身就是第二个变量。

所以能下的结论只有：**两次都通过、没有回归证据**；而且本批改动是 **6 行属性
（零运行时影响）**，从机理上也不可能影响帧时间。

> **教训（第 25 条）：A/B 对照要求"只改一个变量"。**
> **宿主负载是第二个变量** —— 在负载会变的机器上做不了干净的性能对照，
> 能给的只是"**没有回归证据**"，不能给"**证明无回归**"。
> 这与第 21 条（对照要验证注入生效）配套：那条管"**注入有没有生效**"，
> 这条管"**除了注入之外还有没有别的变量**"。

---

## 附录 GU：收口前的**完整取证**（一次跑完所有判据）（2026-10-06 批次 GU-1）

### GU.1 为什么

目标要求"**在宣告完成前收集证据**"。本会话的判据已经很多，
但**从没有一次性把它们全跑一遍并汇总** —— 都是分批跑的。

### GU.2 汇总表（10 项，全部通过）

| 项 | 退出码 | 结果 |
|---|---|---|
| 布局判据（逐页 × 语言 × 缩放） | 0 | 判红项 ① ② ⑤ ⑥ ⑦ ⑧ ⑨ ⑩ ⑪ **全 0**；③ = 55、④ = 0（两者**都是 report-only**，见下） |
| 缩放规则 ①~⑦ | 0 | 五类都没有问题 |
| 缩放规则自检（正向对照） | 0 | 注入判红并点名文件，清理后放行 |
| i18n 检查 | 0 | 英文位无中文（有意保留 0 处） |
| 文档数字（含中英 README / 发布事实） | 0 | 750/750 · 22/22 · 144/144 · README 750/750 · 71/71 · 英文 750/750 · ③d 四处 · ③e 体积与链接 |
| 规模文档（三层 + 集合相等） | 0 | 五项全部 50,534 行 / 71 文件；逐 crate、逐文件、测试分布表一致 |
| musl 告警（6 crate） | 0 | 6 个 crate 全 0 告警 |
| 像素回归（`--shot` 逐字节） | 0 | 57 档一致 |
| 单元 + 集成测试 | 0 | **750 passed / 0 failed**（8 个目标：457+35+17+15+0+0+226+0） |
| 全部门禁 | 0 | **22 项全过** |

### GU.3 ⚠️ 而我的汇总脚本**读错了两个数字**（第 26 条）

第一版汇总脚本报出 `③=55` 与 `457 passed`，**退出码全是 0**。核对后：

1. **③ = 55 是设计如此** —— layout-check 的 ③ 是
   "**纵向越界（= 这一页需要滚动，不判红）**"，**本来就是 report-only**；
   我的正则还从附录引用里抓了个"③"，把两个来源混在一起；
2. **457 只是 8 个目标里的第一个** —— `cargo test --workspace` 会跑 8 个目标，
   总数是 **750**。

**两个数字都不是"事实错了"，是"仪器读错了"** —— 而**退出码全 0，读错是静默的**。

> **教训（第 26 条）：汇总脚本本身也是测量仪器，也要验证。**
> 它与第 21 条（对照要验证注入生效）、第 25 条（对照要控制变量）同族：
> 三条讲的都是"**别信工具的输出，除非你验证过这个工具**"。
> 做法：拿**已知口径**（脚本自己打印的逐行、逐目标计数）对一遍。

### GU.4 关于 ③ = 55 与 ④ = 0

这两项**明确标注为"不判红 / 只报告"**：

* ③ 纵向越界 = 这一页**需要滚动** —— 设置页内容比窗口高是**正常的**，不该判红；
* ④ 控件矩形重叠 = 判据不够硬（控件边框相切是正常的），只报告。

所以"判红项全 0"的说法是：**① ② ⑤ ⑥ ⑦ ⑧ ⑨ ⑩ ⑪ = 0**。

### GU.5 验证

* 上表 10 项逐项退出码 0；
* ③ / ④ 的性质由 `layout-check.py` 的判据行自述（本批逐行打印核对）；
* 测试总数由 8 个目标的 `N passed` 相加得到（本批逐目标打印核对）。

---

## 附录 GV：`platform/` 审计 —— **安装器根本没进镜像** + 一处少换行（2026-10-06 批次 GV-1）

### GV.1 为什么审 `platform/`

本会话此前**从未审过** `platform/`（构建/部署侧，17 个文件 / 316 行）。
先按风险模式扫（`dd` / `mkfs` / `chmod 777` / `curl|sh` / `sudo` / `rm -rf /`）——
**结果干净**：`dd` 只用于**读** MBR 签名（`skip=510 count=2`），`mount` 只在 `/init` 里，
无 `chmod 777`、无管道执行、无 `sudo`、无破坏性删除。

**而且这个脚本质量很高**，注释里记着多次审计发现：

* **口令哈希不硬编码**（M-4）：走 `AETHER_ROOT_PW_HASH` 环境变量，
  并说明**为什么不写进 overlay**（`platform/overlay/etc/` 被 git 跟踪，
  一次 `git add -A` 就把哈希提交进仓库）；
* **TOFU 哈希对账**（M-9）：注释记着一个**真 bug** ——
  "⚠️ 这段必须在 `if [ ! -d ... ]` 之外（对抗审查发现）：原来它被包在'还没解包'的分支里，
  于是…**只要压缩包还在，就能拿它绕过校验**"；
* **`isohybrid` + MBR 自检**（H-6）：注释记着"装机时 dd 会毁掉目标盘首 32MB，
  盘却起不来，程序还会打印'安装完成'"；
* `--locked`（M-9）、`set -euo pipefail`。

### GV.2 bug 一：**`aether-install` 没进镜像**

`build-iso.sh` 的组件清单是：

```sh
COMPONENTS=(aether-init aetherd aether-ops aether-compositor)
```

**没有 `aether-install`** —— 而**同一个脚本**第 123 行的注释却在讨论
"`aether-install` 的前提是'源镜像带 MBR 签名…'"。

后果：README 说"**`platform/build-iso.sh` 已补上 isohybrid 步骤与自检，
重新构建后装机可用**" —— **重建也不会带上安装器**，所以那句话是**假的**。

### GV.3 bug 二：`post-build.sh` **少了一个换行**

```sh
echo "==> post-build: 写入系统指纹（不再是 Buildroot 默认值）"printf 'aether\n' > "$TARGET/etc/hostname"
```

两个命令挤在一行：`echo` 把后面的 `printf` 当**参数**一起打印，
而重定向作用于**整条命令**。**跑真实 shell 实测**，`/etc/hostname` 的内容是：

```
==> post-build: 写入系统指纹（不再是 Buildroot 默认值）printf aether\n
```

—— **主机名成了那句提示语**。

**注意 `sh -n` 查不出来**（语法完全合法），所以门禁里现有的 shell 语法检查**帮不上忙**。

### GV.4 修法 + 守卫

1. `COMPONENTS` 加上 `aether-install`；
2. `post-build.sh` 补上换行；
3. 新增 `scripts/iso-components-check.py`：**凡 `workspace.members` 里带 `src/main.rs` 的 crate，
   都必须出现在 `COMPONENTS` 里**；接进门禁（**门禁数 22 → 23**）。

**守卫立刻又抓到第三个**：`aether-shell` —— 它确实是"仓库里有、镜像里没有"。
查证后确认它是**明写的 M0 骨架占位**（`src/main.rs` 文档注释自述），
所以**不进镜像是对的**。

于是加**例外机制**，但按本会话第 4 条（"**过滤器/白名单会隐藏 bug**"）：
**例外必须写明理由，而且会被打印出来** ——

```
例外：aether-shell —— M0 骨架占位（src/main.rs 文档注释自述「当前为 M0 骨架占位，防止结构空转」）
[ OK ] 所有二进制 crate 都在镜像里
```

**正向对照**（按第 21 条先断言注入生效）：把 `aether-install` 从 `COMPONENTS` 去掉
→ 退出码 **1**，报 `[FAIL] 这些二进制 crate 没进镜像：aether-install`；回退 → 逐字节一致。

### GV.5 一条方法论（第 27 条）

> **"写进仓库"不等于"进了产物"。**
> crate 在仓库里、构建脚本没带上 —— **功能就是不可用的**，
> 而 README 会因为"代码写完了"而声称它可用。

这与本会话第 24 条（假收敛）同族：**都是"断言"与"事实"脱节**。
区别是第 24 条在**文档**层，这条在**产物**层 —— 而产物层的脱节**更隐蔽**，
因为代码、测试、门禁**全都是绿的**（`cargo test` 会测那个 crate，它当然通过）。

**`sh -n` 这类语法检查也帮不上忙**：少个换行是**合法语法**，
所以"检查了脚本语法"不等于"检查了脚本行为"。

### GV.6 验证

* `scripts/iso-components-check.py` → 6 个二进制 crate（5 进镜像 + 1 明写例外）、退出码 0；
* 去掉 `aether-install` → 退出码 1；回退 → 逐字节一致；
* `post-build.sh` 的后果由**真实 shell 实测**（本批贴出了 `/etc/hostname` 的实际内容）；
* `gates.py` → 23 项全过。

### GV.7 顺带：门禁数 22 → 23 引发的连锁，以及**第 28 条**

加进门禁后门禁数变成 23，于是 `doc-numbers-check.py` 立刻红了 ——
**一次点名三处**（收尾报告的 ① 表、自引用行、验收命令注释）。
这正是附录 GP 那批做的"**同一事实的全部写法**"在起作用：**判据自己抓到了自己**。

但接着暴露出一个新问题：**同步脚本修不了其中一处**。

* 检查方（`doc-numbers-check.py` 的 ③d）认**四种**写法：
  `**X 项全绿**` / `**X 项全过**` / `「X 项测试 / Y 项门禁」` / `# X 项门禁（含…`；
* 同步方（`sync-doc-numbers.py` 第 6 节，GT 批加的）只认**三种** —— 漏了最后那种。

于是出现"**检查会红、同步修不了**"的状态：同步脚本报"仍有 1 条未同步"，
收敛循环**永远不收敛**（与附录 GS 的"永远差一条"是同一个形状）。

> **教训（第 28 条）：同一个事实的"检查方"与"同步方"，覆盖范围必须一致。**
> 否则不是"漏改"，而是**死循环**：检查一直红，而自动修复永远够不着那一处。

**本批的处置**：
1. 给同步脚本补上第四种写法；
2. 但实测它**仍未生效**（同步脚本报"第 1 轮无改动"），于是**本批直接改了那两行**，
   把检查修绿 —— **不留在"以为它会自动修"的状态**；
3. 同步脚本第 6 节为什么不生效（`find_doc` 对全角括号文件名？还是 `re.sub` 的 `{n}`
   与正则分组冲突？）**记在这里，留给后续**。

**这条也说明**：一个"自动修复"工具如果**默默不生效**，比没有它更危险 ——
你会以为文档是自动同步的。这与第 26 条（仪器要验证）同族。

---

## 附录 GW：修好"**默默不生效的同步**"，以及我自己在里面栽的两个跟头（2026-10-06 批次 GW-1）

### GW.1 起因

附录 GV.7 留了一个尾巴：给同步脚本补了第四种写法，**但实测它仍未生效**（报"第 1 轮无改动"）。

### GW.2 根因：守卫的粒度

第 6 节原来的守卫是：

```python
m = re.search(r"② 门禁数：文档 ([\d,]+) / 本次运行 ([\d,]+)", dn)
if m.group(1) != m.group(2):
    s = re.sub(old_pat, new_tpl.format(n=m.group(2)), s, count=1)
```

而 `doc-numbers-check.py` 的 ② 行报的是 **`文档 23 / 本次运行 23`** ——
因为它**只看报告里那张表的行**。于是：

* 表里的行是对的（23 = 23）→ **守卫判"不用改"**；
* 而同一事实的**其它写法**（自引用行 `「750 项测试 / 22 项门禁」`、
  验收命令注释 `# 22 项门禁（含…`）**在它眼里根本不存在** —— 报告里明明还是 22。

调试同时证明：**替换逻辑本身是好的**（`re.sub` 确实改得动那两处）。**坏的是守卫的粒度。**

> **教训（第 29 条）："检查方"的粒度，决定了"同步方"能看见什么。**
> 检查方只报一个**汇总数字**，同步方就只能修**那一处**；
> 其余写法不是"修不动"，而是**它根本不知道它们存在**。

### GW.3 修法：逐写法比较

不要用"汇总数字"当守卫，改成**逐写法**：

```python
real = m.group(2)                                  # 该写法的实测值
if any(v != real for v in re.findall(old_pat, s)): # 文档里现有的值
    s = re.sub(old_pat, new_tpl.format(n=real), s) # 全部出现处都改（不是 count=1）
```

### GW.4 ⚠️ 而我在修的过程中栽了两个跟头

**跟头一：表项用错模式。** 自引用行 `「X 项测试 / Y 项门禁」` 里 **Y 是门禁数**，
我却**复用了 ① 的模式**去取 —— 于是写成了 `「750 项测试 / 750 项门禁」`，
把**测试数**填进了门禁位置。

**跟头二（更值得记）：我的断言是"不等于错值"。** 我上一轮验的是：

```python
print(f"同步后仍含 22：{'22 项门禁」' in now}")   # 期望 False
```

它**通过了** —— 因为写进去的是 **750**，不是 22。**一个错误的 750 骗过了这个断言。**

> **教训（第 30 条）：断言要检查"等于期望值"，不是"不等于错值"。**
> "不等于错值"会被**任何其它错误值**骗过 —— 而"其它错误值"恰恰是这类 bug 的常见形态
> （取错字段、错位、类型混淆）。

改成**等于期望**之后：

```
自引用行 == 期望：True
验收命令注释 == 期望：True
仍含非 23 的门禁行：无
doc-numbers-check 退出码：0
```

### GW.5 验证

* 注入 22 → 同步脚本把它改成 23，且**等于期望**（不是"不等于 22"）；回退 → 逐字节一致；
* `doc-numbers-check.py` → 退出码 0、无 FAIL；
* `gates.py` → 23 项全过。

### GW.6 本批代码变更的审计（AGENTS.md：每完成一个任务审 1 遍）

改动仅 `scripts/sync-doc-numbers.py`（**构建/校验工具**，不进镜像、不参与运行时）：
把"汇总数字驱动"的守卫改成"逐写法比较"，并修正自引用行取值的模式。
**不涉及**输入校验、权限、路径、密钥；**不引入**新依赖；**无**调试输出。
安全影响：无。唯一风险是"同步脚本改错文档"，已用**等于期望**的断言与逐字节回退核实。
