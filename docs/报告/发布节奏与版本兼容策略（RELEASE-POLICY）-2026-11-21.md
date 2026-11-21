# AetherOS 发布节奏与版本兼容策略（RELEASE-POLICY）— 2026-11-21

> 本文回答：**版本号怎么排、升级/回滚怎么走、对外承诺哪些兼容、每次发布前必须过哪些检查、通道与撤回怎么操作。**
> 基线事实：当前发布 `v0.1.1`（prerelease，ISO 27,262,976 字节 / SHA256 `53580ae5…`，见 `site/release.json`）；更新链路为文件级离线包 + 整机换镜像保 `/var`，镜像级 OTA 未立项（见 [镜像级OTA与A-B分区 ADR](../架构决策（ADR）-镜像级OTA与A-B分区-2026-11-21.md)）。
> 本文是策略文档，不是发布日期承诺；时间估算沿用 [后续发展计划（NEXT-PLAN）-2026-10-31](后续发展计划（NEXT-PLAN）-2026-10-31.md) 的口径。

## 一、版本号双轨

系统同时存在两套版本号，各管各的范围，不混用：

### 1.1 系统镜像版本（对外发布轨）

- 格式：`v<里程碑>.<补丁>`，当前处于 `v0.1.x` alpha 线；下一里程碑为 `v0.2`。
- 镜像版本记录在 guest 的 `/etc/aether/version`；更新清单 `update.json` 的 `version` 字段使用同一版本串，`version_is_newer` 拒降级（`aetherd/src/updater.rs:233-235`、`aetherd/src/update.rs:106-123`）。
- `v0.1.x` 内部：`x` 递增 = 兼容或修复性质的文件级更新；`v0.1 → v0.2` = 里程碑推进，可包含界面、布局、应用格式的调整。
- **1.0 之前不承诺长期兼容**：minor 提升可以带不兼容变更，但必须在更新说明与本文档兼容性章节写明破坏点。

### 1.2 工程语义版本（IPC / API / 应用包轨）

- 对 `aether-ipc` 协议、应用包格式、更新清单格式采用 `MAJOR.MINOR.PATCH` 语义：
  - `PATCH`：纯新增、不改既有语义；
  - `MINOR`：向后兼容的新增能力；
  - `MAJOR`：破坏既有契约。1.0 之前，破坏变更允许在 `MINOR` 发生，但必须显式标注。
- 协议握手预留版本字段（`hello{version}`）；任何未知 `type` 必须返回 `error{code:1, message:"unknown type"}`，保证前后兼容（[进程间通信协议（IPC-PROTOCOL）-2026-09-05](../进程间通信协议（IPC-PROTOCOL）-2026-09-05.md) 第 45–46 行）。

## 二、升级 / 回滚矩阵

| 场景 | 升级机制 | `/var` 处理 | 回滚方式 | 回滚窗口与边界 |
|---|---|---|---|---|
| `0.1.x` 补丁（文件级更新包） | updater 拉取清单 → Ed25519 验签 → 逐文件 SHA-256 校验 → 暂存 → 重启由 `aether-init` 应用（`aetherd/src/update.rs`、`aether-init/src/update.rs`） | 不动 `/var`；白名单前缀仅 `/var/apps/`、`/etc/aether/`、`/var/lib/aether/` | 文件级回滚：按暂存表恢复清单内文件（QEMU 演练已 PASS，见 [QEMU双次装机-2026-11-14](../证据/QEMU双次装机-2026-11-14.md)） | 未重启前可弃用暂存；应用后回滚**只覆盖清单内文件**，清单外改动不自动回滚；旧版本清单被 `version_is_newer` 拒绝，不能降级刷回 |
| 系统组件升级（`/usr`、内核、系统库等） | 新 ISO + 无人安装 + `--keep-var`（文件级白名单刻意不含系统组件，见 `scripts/mkupdate.py` 文件头说明） | keepvar 复用 `/var` ext4 分区，UUID 不变；历史分区项索引缺陷已在两轮 keepvar 升级中复核（[T3验证-keepvar保住var-2026-10-17](../证据/T3验证-keepvar保住var-2026-10-17.md)） | 重装上一版 ISO + `--keep-var`；升级前用现有配置快照页留一份备份 | 上一版 ISO 资产与 SHA256 必须在 Release/站点保留可下载；重装期间用户数据依赖 `/var` 完整性，失败时按救援控制台流程处理 |
| `0.1.x → 0.2` 里程碑 | 先走 beta 通道探测 E2E，再决定 keepvar 升级或建议全新安装 | 同 keepvar 规则；升级前必须核对 `settings.json`、已装应用、审计日志的目录迁移兼容 | 重装 `0.1.x` 上一版 ISO + keepvar | `0.2` 转 stable 前不向 `0.1.x` 自动全量推送；不兼容项在发布说明逐条列出 |
| 更新包损坏 / 签名不符 | 整包拒绝，不落地应用；篡改包、缺签名包被拒（E2E 口径见 [更新E2E-2026-11-05](../证据/更新E2E-2026-11-05.md)） | 不触碰 | 无需回滚：系统保持旧版 | 验签失败、哈希不符、路径越界任一命中即拒绝并留审计 |

> 镜像级 A/B 槽位回滚不在本矩阵内；立项条件见 [镜像级OTA与A-B分区 ADR](../架构决策（ADR）-镜像级OTA与A-B分区-2026-11-21.md)。

## 三、兼容性承诺

### 3.1 IPC 协议

- 既有请求/响应 `type` 与其字段语义在同一 MINOR 内不改坏；新增字段必须可选，旧客户端忽略未知字段。
- 未知 `type` 返回 `error code 1`；服务端不得对未知类型静默成功。
- 破坏变更：协议文档标注版本与迁移说明，`aether-ipc` 内测试补双向断言；1.0 前随 MINOR 发布，1.0 后随 MAJOR。

### 3.2 应用包格式

- 格式基线见 [应用包格式（APP-PACKAGES）-2026-09-28](../应用包格式（APP-PACKAGES）-2026-09-28.md)；镜像 glibc 基线 `GLIBC_2.38`，向后兼容。
- 包清单新增字段向后兼容；安装预检（依赖闭包、路径穿越、glibc 家族拦截）规则收紧时，旧包行为变化必须在发布说明写明。
- 应用包签名/依赖解析落地后（NEXT-PLAN P1），本节补充签名格式与验签边界。

### 3.3 更新清单格式

- `update.json` + `update.json.sig`（Ed25519 detached 64 字节签名）；清单条目为 `path / sha256 / bytes`；契约由 `scripts/update-contract-check.py` 守门。
- 白名单前缀集合不随补丁版本扩大；新增可更新路径必须改 ADR 并补负向测试。
- 清单格式演进保持双版本兼容一个发布周期：旧 guest 能识别新清单的新增字段，新 guest 能解析旧清单。

## 四、发布前 Checklist（顺序执行，缺一不发）

> 脚本位置均为仓库根目录 `scripts/`。发布执行人与复核人职责见 [维护交接与双人发布（MAINTAINER-HANDOVER）](维护交接与双人发布（MAINTAINER-HANDOVER）-2026-11-21.md)。

1. **测试**：`cargo test --workspace --locked` 全绿；Linux 侧 `cargo check --target x86_64-unknown-linux-musl --all-targets` 无新增告警。
2. **全量门禁**：`python scripts/gates.py`（全量，含像素/性能；`--fast` 不能替代发布门禁）。
3. **像素回归**：涉及渲染改动时 `python scripts/shot-diff.py verify`，差异为 0 或有明确批准记录。
4. **站点生成一致性**：`python scripts/gen-site.py --check`，页面与磁盘数据逐字节一致。
5. **文档数字**：`python scripts/doc-numbers-check.py` 通过；行数/测试数/文件数只取权威来源，不手抄。
6. **本地链接**：`python scripts/check-links.py` 解析全部新增本地链接。
7. **站点健康**：`python scripts/site-health-check.py`，本地/线上事实一致，安全响应头在位。
8. **发布物就绪**：`python scripts/release-readiness.py --iso <待发布 ISO>`，ISO 大小 / SHA / isohybrid / release.json 一致性全部通过。
9. **更新包契约**（发更新包时）：`python scripts/update-contract-check.py`；清单签名用离线私钥，公钥指纹与入库公钥核对。
10. **装机与保留盘验证**：VM 全新安装一次；`python scripts/keepvar-verify.py`（或等价 keepvar 演练）确认 `/var` 保留；回滚演练记录归档到 `docs/证据/`。
11. **发布事实回填**：按 `site/release.json` 的 `_how_to_update` 顺序——重建 ISO → 上传 Release 资产 → **把资产下载回来**算大小与 SHA256 → 更新 `release.json` → `gen-site.py --refresh` → `gen-site.py --check`；站点/README/更新日志三处数字对齐。

## 五、发布通道与撤回流程

### 5.1 通道

- **beta（预发布）**：`prerelease: true`（当前 `v0.1.1` 即此状态）。用于探测包、里程碑前 E2E 与小范围验证；updater 默认不拉取 beta，仅在显式配置下接收。beta 资产允许标注已知未验证项，但不得把未验证写成已支持。
- **stable（稳定）**：全量推送。条件：第四节 checklist 全绿、至少一台真机/VM 完成安装 + 升级 + 回滚演练、双人复核通过。stable 发布说明必须列出兼容范围、已知边界与回滚步骤。

### 5.2 撤回流程（发现发布物有缺陷时）

1. **更新通道撤回**：在更新源下架当前 `update.json` 与 `update.json.sig`；`publish-updates.py` 先干跑核对，再 `--apply` 回退上一版清单；确认 guest 重新拉取时拿到的是上一版或无可用更新。
2. **下载页撤回**：`site/release.json` 回滚到上一版资产的字节数与 SHA256，`gen-site.py --refresh` 后 `--check` 复核；线上经 `site-health-check.py` 确认。
3. **Release 资产处理**：GitHub Release 重新标记 prerelease，或撤下问题资产；站内链接一律用 `/releases/tag/<tag>`，不依赖 `releases/latest`（`release.json` 的 prerelease 备注）。
4. **已安装端**：系统不主动自动降级；发布公告给出手动回滚步骤（重装上一版 ISO + `--keep-var`），并说明哪些清单内文件需要人工恢复。
5. **留痕**：撤回原因、涉及版本、操作人/复核人、时间与证据归档到 `docs/证据/`；签名密钥若怀疑泄露，按密钥轮换流程处理（私钥不入库、不明文共享）。

## 六、什么条件下重新评估

1. 进入 `v1.0` 筹备：兼容性承诺从「pre-1.0 可破」收紧为正式 semver 承诺；
2. 镜像级 A/B OTA 立项落地：第二节矩阵补充槽位级升级/回滚行；
3. 第三方应用生态形成：应用包格式与 glibc 基线承诺范围需要扩展；
4. 发布频次或通道结构变化（如长期 beta/stable 并行多版本）：重写第五节通道规则。
