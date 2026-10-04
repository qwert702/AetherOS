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
