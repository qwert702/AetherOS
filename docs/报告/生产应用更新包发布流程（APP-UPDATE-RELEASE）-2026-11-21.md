# 生产应用更新包发布流程（APP-UPDATE-RELEASE）— 2026-11-21

> 对应里程碑：M7/R7 发布链路上的**生产应用更新包**环节。
> 打包脚本：[`../../scripts/make-app-update.py`](../../scripts/make-app-update.py)；
> 复用既有：[`../../scripts/mkupdate.py`](../../scripts/mkupdate.py)（清单/哈希格式）、
> [`../../scripts/update-keygen.py`](../../scripts/update-keygen.py)（Ed25519 密钥）、
> [`../../scripts/publish-updates.py`](../../scripts/publish-updates.py)（上传摊平）、
> [`../../scripts/update-contract-check.py`](../../scripts/update-contract-check.py)（契约）。
>
> **当前状态：私钥不在仓库 → 未产出生产签名包。** 仓库内只有 32 字节公钥
> `aetherd/keys/update_pubkey.bin`；私钥按设计放在仓库外（`~/.aether/`），
> 本轮未进行生产签名与发布。

## 一、包结构

应用级更新包与系统文件级更新包同构（`mkupdate.py` 格式，不另造）：

```text
updates-app/<version>/
  update.json                 清单：版本 + 逐文件 path/sha256/bytes
  update.json.sig             Ed25519 原始签名 64 字节（仅签名包存在）
  var/apps/<app-id>/...       包内相对路径 = 目标路径去掉开头的 '/'
```

发布到测试源 / 生产源时**摊平**到 Base URL 下（guest 按此 URL 拉取）：

```text
<base>/update.json
<base>/update.json.sig
<base>/var/apps/<app-id>/...
```

## 二、清单字段

`update.json`（UTF-8，无 BOM；字段名与 `aetherd/src/update.rs` 对齐）：

```json
{
  "version": "0.2.0",
  "files": [
    {
      "path": "/var/apps/hello/hello",
      "sha256": "<64 位小写十六进制>",
      "bytes": 20
    }
  ]
}
```

| 字段 | 约束 |
|---|---|
| `version` | 只允许数字与点（`0.2.0`）；语义化数字段比较，拒降级 / 同版本 |
| `path` | 绝对路径、无 `..`、无控制字符、命中白名单前缀 |
| `sha256` | 64 位小写十六进制；打包后独立复检逐条重算 |
| `bytes` | 打包后按实际大小写入；复检逐条核对 |

白名单前缀（系统组件 `/usr/bin/*` 刻意不在内，走整机 ISO + `--keep-var`）：

* `/var/apps/`（应用级更新默认**只收**此前缀）
* `/etc/aether/`、`/var/lib/aether/`（需显式 `--allow-system-paths`）

## 三、签名与密钥

### 密钥约定（复用 `update-keygen.py`）

* 私钥：Ed25519 PKCS#8 PEM，默认 `~/.aether/update-signing-key.pem`，**0600，绝不入库**；
* 公钥：32 字节原始公钥，入库 `aetherd/keys/update_pubkey.bin`；
* 私钥丢失 = 无法再签新更新；需离线备份；轮换公钥要走发布流程，旧客户端不再接受新清单。

首次生成（仅在发布机执行，仓库外）：

```bash
python scripts/update-keygen.py --private-key ~/.aether/update-signing-key.pem
# 公钥写入 aetherd/keys/update_pubkey.bin；打印公钥 SHA-256 指纹，留档核对
```

### 打包与签名

```bash
# ① 先打 unsigned bundle（不生成 .sig，不写假签名）
python scripts/make-app-update.py --version 0.2.0 --from stage/hello --out updates-app

# ② 私钥就位后出签名包（--key 指向仓库外私钥）
python scripts/make-app-update.py --version 0.2.0 --from stage/hello --out updates-app \
    --key ~/.aether/update-signing-key.pem
```

签名规则：对 `update.json` 的**原始字节**签名；`.sig` 固定 64 字节。
不带 `--key` 时脚本只打印待执行的签名命令，**不产出占位签名**；
`--key` 指向的私钥不存在 → 退出码 2 并明确报错，不现造钥匙冒充生产签名；
私钥路径落在仓库内 → 拒绝使用。

### 打包后自检

```bash
# 独立复检：清单逐条重算 sha256 / 大小；签名包额外验签（脚本内已自动执行）
python scripts/mkupdate.py --selftest
# 发布布局 ↔ guest URL 契约（起本地临时 HTTP 服务逐字节核对）
python scripts/update-contract-check.py
```

## 四、发布步骤

```bash
# ① 干跑：只列要传什么（默认行为，可逆优先）
python scripts/publish-updates.py updates-app/0.2.0 --dry-run

# ② 确认无误后真上传（先文件、最后清单，避免 guest 拉到半成品）
export AETHER_VPS_HOST=<VPS 地址>
export AETHER_VPS_KEY=<部署私钥路径>
python scripts/publish-updates.py updates-app/0.2.0 --apply

# ③ 源上静态复测（不需要 guest）
python scripts/update-online-refresh.py --dry-run --source https://<源>/updates
```

发布前核对：

* 清单版本高于 guest 当前版本（`version_is_newer`，否则 guest 拒降级 / 同版本）；
* 每个文件在白名单前缀内；无符号链接、无 `..`、无控制字符；
* `.sig` 存在且 64 字节；公钥指纹与入库公钥一致；
* HTTPS 源（明文 http 被 updater 拒绝）。

## 五、回滚

* **文件级更新不支持在线降级**：updater 版本比较会拒绝旧版本 / 同版本包，
  回滚坏更新应走**快照回滚**（§5.6，需明确用户确认）；
* 整机级回滚：保留 `--keep-var` 的上一版 ISO 重新安装；
* 发布侧回退：从源上撤下新版本清单与文件，恢复上一版摊平布局；
* 暂存未应用前：删掉 guest 侧 `incoming/<version>/` 与 `staged/` 残留即可。

## 六、校验命令速查

| 目的 | 命令 |
|---|---|
| 打包（unsigned） | `python scripts/make-app-update.py --version <v> --from <stage> --out updates-app` |
| 打包（签名） | 上一行加 `--key ~/.aether/update-signing-key.pem` |
| 格式 / 逻辑自检 | `python scripts/mkupdate.py --selftest` |
| 发布契约 | `python scripts/update-contract-check.py` |
| 源上静态复测 | `python scripts/update-online-refresh.py --dry-run` |
| guest 在线复测 | `python scripts/update-online-refresh.py`（见 UPDATE-ONLINE-VERIFY 报告） |

## 七、本机实测记录（2026-11-21）

| 命令 | 退出码 | 结果 |
|---|---|---|
| `python -m py_compile scripts/make-app-update.py` | `0` | 编译通过 |
| `python scripts/make-app-update.py --help` | `0` | 参数与签名约定已打印 |
| unsigned 冒烟包（临时目录，未入库） | `0` | 1 个文件，`update.json` 字段齐全；包内**无** `.sig`；复检通过；打印待签名与发布命令 |
| `--key` 指向不存在的私钥 | `2` | 报错"签名私钥不存在"，提示先跑 `update-keygen.py`；未生成假签名 |
| 暂存树含 `/usr/bin/aetherd` | `2` | 整包拒绝：不在白名单前缀 |
| `/etc/aether/` 文件（默认应用级模式） | `2` | 整包拒绝：应用级只允许 `/var/apps/`（`--allow-system-paths` 可放开） |
| 临时测试私钥签名包（临时目录，非生产） | `0` | `.sig` 64 字节，独立复检 + 验签通过；测试用密钥已随临时目录删除 |

**未执行项**：生产私钥未在本环境出现，未对任何真实应用二进制出签名包；
未发布到测试源 / 生产源；guest 在线复测见 UPDATE-ONLINE-VERIFY 报告，状态未执行。
