# 自动更新在线复测（UPDATE-ONLINE-VERIFY）— 2026-11-21

> 对应里程碑：**M8 可装可更**剩余项 —— 自动 updater 在线拉取复测。
> 复测脚本：[`../../scripts/update-online-refresh.py`](../../scripts/update-online-refresh.py)。
> guest 侧实现：`aetherd/src/updater.rs`、`aetherd/src/update.rs`；发布侧：
> `scripts/publish-updates.py`、`scripts/update-contract-check.py`。
>
> **当前状态：未执行（无可用 guest）。** 本机无运行中的 guest，paramiko 亦未安装，
> live 复测按设计退出码 2 报告"未执行"；本文不写"已验证"结论。

## 一、现状（已完成 / 仍缺）

| 项 | 状态 | 证据 |
|---|---|---|
| 探测包 E2E（0.1.2 探测包） | ✅ 已完成：ISO Update E2E run #9，guest 拉取 → 验签 → 暂存 → 重启应用 | [../证据/更新E2E-2026-11-05.md](../证据/更新E2E-2026-11-05.md) |
| 本地 QEMU 安装 / keepvar / 原地更新 / 回滚 | ✅ 已完成 | [../证据/QEMU双次装机-2026-11-14.md](../证据/QEMU双次装机-2026-11-14.md) |
| **生产应用更新包** | ⬜ 未产出 | 见 [生产应用更新包发布流程（APP-UPDATE-RELEASE）](生产应用更新包发布流程（APP-UPDATE-RELEASE）-2026-11-21.md) |
| **updater 在线拉取复测（生产包）** | ⬜ 本轮未执行 | 本文流程 |

run #9 用的是 `update-probe.txt` 探测包，不是生产应用二进制；探测包不能冒充生产更新。

## 二、复测链路（guest 视角，与 updater.rs 对齐）

```text
测试源 HTTPS
  GET <base>/update.json          清单（先验签，后解析）
  GET <base>/update.json.sig      Ed25519 原始 64 字节签名
  GET <base>/<path 去掉开头的 />   每个文件
        │
        ▼
aetherd updater 线程
  验签 → 版本比较（拒降级）→ 下载到 incoming/<version>/.partial/
  → 逐文件 sha256 + 大小核对 → 改名投递 incoming/<version>/
  → stage() 拷入 staged/，最后写 ready.json / apply.list
        │
        ▼
下次重启 → aether-init 读 apply.list 逐文件复核 sha256 → 应用 → 清 staged/
```

关键路径（与 Rust 常量一致）：

| 用途 | 路径 |
|---|---|
| updater 配置 | `/etc/aether/updater.json`（`{"url":"https://…","check_minutes":5}`） |
| 更新根目录 | `/var/lib/aether/update` |
| 投递目录 | `incoming/` |
| 暂存目录 | `staged/` |
| 就绪标记 | `staged/ready.json` |
| 应用清单 | `staged/apply.list`（每行 `sha256 ⏵ bytes ⏵ target`） |
| 版本文件 | `/etc/aether/version` |
| 探针标记（示例） | `/var/lib/aether/update-probe.txt` |

## 三、前置条件

1. **guest 可达**：SSH 环境变量齐全（见下），或本机有可连的 QEMU/VirtualBox guest；
2. **测试源**：HTTPS 可达，已按发布布局摊平（清单 / 签名 / 文件同目录）；
3. 镜像内嵌公钥 `aetherd/keys/update_pubkey.bin` 与签名私钥成对；
4. guest 内 `aetherd` 由可重启的方式托管（systemd 或 init 监督重启）；
5. Python 3；live 模式需要 `paramiko`（`scripts/requirements.txt`），dry-run 仅标准库。

### guest 连接环境变量（与 `scripts/vm.py` 同一套）

| 变量 | 默认 | 说明 |
|---|---|---|
| `AETHER_VM_HOST` | `127.0.0.1` | guest 地址 |
| `AETHER_VM_PORT` | `2222` | SSH 端口 |
| `AETHER_VM_USER` | `aether` | 登录用户 |
| `AETHER_VM_KEY` | — | 私钥路径（与密码二选一） |
| `AETHER_VM_PASSWORD` | — | 密码（与私钥二选一） |
| `AETHER_UPDATE_SOURCE` | `https://aether.cbnac.com/updates` | 测试源 Base URL（也可 `--source`） |

## 四、复跑步骤

### 1. 静态检查（不需要 guest）

```bash
# 检查 update.json / update.json.sig 可达，清单内每个文件哈希与大小一致
python scripts/update-online-refresh.py --dry-run
python scripts/update-online-refresh.py --dry-run --source https://<测试源>/updates
```

dry-run 同时尝试用 `aetherd/keys/update_pubkey.bin` 验签；公钥缺失或未装
`cryptography` 时跳过密码学验签并明确标注，不算失败。

### 2. live：在 guest 内触发一轮在线检查 / 拉取 / 暂存

```bash
export AETHER_VM_HOST=127.0.0.1 AETHER_VM_PORT=2222 AETHER_VM_USER=aether
export AETHER_VM_KEY=~/.ssh/id_ed25519     # 或 AETHER_VM_PASSWORD=…
python scripts/update-online-refresh.py --source https://<测试源>/updates --timeout 300
```

脚本在 guest 内：

1. 读 `/etc/aether/version` 当前版本；
2. 写入 `/etc/aether/updater.json`（https、`check_minutes=5`）；
3. 重启 aetherd（默认 `sudo -n systemctl restart aetherd`，可用 `--restart-cmd` 覆盖），
   让 updater 以 `last_check=0` 立刻跑一轮；
4. 轮询 `incoming/` 与 `staged/ready.json`（默认 300 秒）；
5. 核对 staged 包：清单、签名长度 64 字节、`apply.list`、逐文件 sha256 / 大小；
6. 检查重启后标记文件（`--marker`，默认 `/var/lib/aether/update-probe.txt`）。

### 3. 重启后复核

```bash
# guest 重启应用生效后，单独复核标记文件与 staged 清空
python scripts/update-online-refresh.py --after-reboot
```

## 五、PASS 判据

| 步骤 | 通过证据 |
|---|---|
| 验签 | `update.json.sig` 64 字节；公钥验签通过（或标注跳过原因） |
| 暂存 | `staged/ready.json` 存在，版本与清单一致；`apply.list` 非空 |
| 文件完整性 | staged 内每个文件 sha256 与清单一致、大小一致 |
| 重启应用 | 标记文件存在且内容为新版本；`staged/` 已被 init 清空 |
| 版本推进 | `/etc/aether/version` 变为远端新版本 |

任一不满足 → 脚本退出码 1，并逐项打印 `[FAIL]` 与证据。

## 六、退出码

| 退出码 | 含义 |
|---|---|
| `0` | PASS |
| `1` | 校验失败（哈希不符 / 缺文件 / 验签失败 / 标记缺失） |
| `2` | **未执行**：无可用 guest（缺 paramiko 或缺 SSH 凭据） |
| `3` | 连接失败（guest SSH 或测试源网络不通） |

## 七、失败排查

| 症状 | 处置 |
|---|---|
| 退出码 2，"缺少 paramiko" | `python -m pip install -r scripts/requirements.txt` |
| 退出码 2，"缺少 AETHER_VM_KEY / AETHER_VM_PASSWORD" | 补 guest 凭据；不要写进仓库 |
| 退出码 3，SSH 连接失败 | 核对 host/port/user；hostfwd 是否开着；主机密钥记录（`scripts/.vm_known_hosts`） |
| 清单 404 | 源上布局不是摊平结构；先跑 `update-contract-check.py` 验契约 |
| 验签失败 | 公钥与签名私钥不配对；清单被篡改；不要用 `AETHER_ALLOW_UNSIGNED_UPDATE=1` 上生产 |
| 超时未见 `ready.json` | 看 guest 内 aetherd stderr：版本未更新（拒降级）、下载超限、哈希不符 |
| 标记文件不存在 | 暂存已完成但应用尚未重启；重启后复跑 `--after-reboot` |

## 八、本机实测记录（2026-11-21）

| 命令 | 退出码 | 输出摘要 |
|---|---|---|
| `python -m py_compile scripts/update-online-refresh.py` | `0` | 编译通过 |
| `python scripts/update-online-refresh.py --help` | `0` | 参数、环境变量、退出码已打印 |
| `python scripts/update-online-refresh.py`（live，无 guest 凭据环境） | `2` | `未执行：无可用 guest（缺少 paramiko；pip install -r scripts/requirements.txt）` |
| `python scripts/update-online-refresh.py --dry-run` | `3` | 清单可达性检查在 TLS 握手阶段失败：`SSLV3_ALERT_HANDSHAKE_FAILURE`（连接失败，非未执行） |

**未执行项**：guest 内配置 updater、在线拉取、验签、暂存、重启应用与标记复核
均未在真实 guest 中跑过；生产应用更新包尚未发布，在线复测无对象可测。
