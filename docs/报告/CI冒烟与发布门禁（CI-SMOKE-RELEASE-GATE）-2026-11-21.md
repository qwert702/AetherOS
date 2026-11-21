# CI 冒烟与发布门禁（CI-SMOKE-RELEASE-GATE） · 2026-11-21

> **对应工作项**：E3「硬件 CI / VM 冒烟」、E4「文档/站点自动检查进发布流程」。
> 本次交付把 QEMU 启动冒烟收成可本地与 CI 复用的单一入口，并把既有发布前检查
> 聚合成一条发布门禁；所有子脚本**只被调用、语义未改**。

## 1. 交付清单（改动 / 新增文件，绝对路径）

| 类型 | 路径 | 说明 |
|---|---|---|
| 新增 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\scripts\vm-smoke.py` | E3 VM 冒烟脚本：QEMU 检测 → headless 启动 → 串口就绪判据 |
| 新增 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\.github\workflows\vm-smoke.yml` | E3 冒烟 workflow（定时 / push / 手动） |
| 新增 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\scripts\release-gate.py` | E4 发布门禁聚合脚本 |
| 修改 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\.github\workflows\ci.yml` | 新增 `release-gate` job（离线） |
| 修改 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\scripts\gates.py` | fast 档追加离线 release-gate 静态子项（最小侵入） |
| 证据 | `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether\docs\证据\release-gate-offline-2026-11-21.json` | 本次 release-gate 实跑的机器可读结果 |

未改动：`docs/README.md`、任何 `src/`、`Cargo.toml`、其他既有脚本的语义。

---

## 2. E3 · VM 冒烟

### 2.1 脚本行为（`scripts/vm-smoke.py`）

1. 先用 `shutil.which("qemu-system-x86_64")` 检测 QEMU；**不存在 → 打印 `SKIP: 无 QEMU` 并以退出码 77 退出**，不返回 0 假成功。
2. 存在时：自动在仓库根目录 / `dist/`、`dist/archive/`、`dist/candidate/` 查找 ISO（可用 `--iso` 指定）；找不到 ISO 同样以 77 明确跳过（环境不具备，不是产品失败）。
3. QEMU 参数沿用既有 `iso-smoke.yml` / `qemu-verify.sh` 的同一套：TCG 加速、`-display none` headless、串口落文件、`-no-reboot`、1024M 内存、2 vCPU。
4. 轮询串口日志，命中就绪关键字（与 `iso-smoke.yml` 同一判据）`aether-init|AetherOS|aether-compositor|login:` → **PASS（0）**；超时或未命中 → **FAIL（1）**，打印串口日志尾部，失败日志落到 `dist/vm-smoke-logs/<时间戳>/`。

### 2.2 退出码约定

| 退出码 | 含义 | CI 语义 |
|---|---|---|
| 0 | PASS：串口命中就绪关键字 | green |
| 1 | FAIL：超时 / 未命中 / QEMU 非零退出 | red |
| 2 | 配置错误（预留） | red |
| 77 | SKIP：环境不具备（无 QEMU / 无 ISO） | neutral / skip，**不算 failure** |

参数：`--timeout`（默认 120s）、`--iso`、`--keep-logs`。

### 2.3 Workflow 触发条件（`.github/workflows/vm-smoke.yml`）

- `workflow_dispatch`：手动触发，可选填 `run_id` / `artifact_name` 下载指定 ISO artifact；留空则用仓库内现有 ISO。
- `schedule`：每周一 03:25（UTC），cron `25 3 * * 1`。
- `push`：仅 `main` 分支，路径过滤 `aether-compositor/**`、`aether-init/**`、`platform/**`。

Job `vm-smoke`：`ubuntu-latest`，超时 15 分钟，安装 `qemu-system-x86` 后跑脚本；
退出码 77 时打 `::notice::` 并在 job 输出里记 `skipped=true`，**不判失败**；非零非 77 才失败；
日志始终上传为 artifact `vm-smoke-logs`（`if-no-files-found: warn`）。

文件末尾附了一段**注释掉的自托管 job**（`vm-smoke-self-hosted`），预留接 KVM 加速 / 本地 ISO；
启用时去掉注释、改 `runs-on` label 即可。

> YAML 注意：YAML 1.1 会把裸 `on:` 解析成布尔 `True`。新 workflow 把键写成带引号的 `"on":`，
> 保证 pyyaml 解析与 GitHub Actions 两侧都拿到字符串键（见 §5.4 校验结果）。

---

## 3. E4 · 发布门禁聚合（`scripts/release-gate.py`）

### 3.1 聚合的子项

| key | 调用命令 | 覆盖 |
|---|---|---|
| `site` | `gen-site.py --check` | 站点页面 / 发布事实 / 脚本哈希自洽 |
| `doc-numbers` | `doc-numbers-check.py` | 文档数字与来源一致 |
| `links` | `check-links.py` | 本地文档链接可解析 |
| `repo-stats` | `repo-stats.py --check` | 文档规模三层 + 集合相等 |
| `site-health` | `site-health-check.py` | 线上站点可用性 / 安全头 / 事实一致（**仅联网模式**） |
| `release-readiness` | `release-readiness.py` | ISO 字节 / SHA256 / isohybrid / release↔data 一致 |

- 串行执行；每项记录：步骤 / 命令 / 退出码 / 状态（PASS·FAIL·SKIP）/ 耗时 / 关键输出。
- 退出码映射：0 → PASS；77 → SKIP；其余 → FAIL。
- **任一 FAIL → 退出 1；全 PASS/SKIP → 0。** `release-readiness` 无 ISO 时按脚本自身语义判红，聚合层不吞掉。
- 参数：`--offline`（跳过 `site-health` 线上巡检）、`--only a,b`（只跑指定 key）、`--json out.json`（写机器可读结果）。

### 3.2 接入点

- **`ci.yml`**：新增独立 `release-gate` job（`ubuntu-latest`，15 分钟超时），跑
  `python scripts/release-gate.py --offline`。干净 checkout 无 ISO 时 `release-readiness`
  会按脚本语义判红，红灯即提示需要先构建 / 挂上 ISO artifact，不在聚合层抹平。
- **`gates.py` fast 档**：追加一条
  `python scripts/release-gate.py --offline --only site,doc-numbers,links,repo-stats`。
  **刻意不含 `release-readiness`**：它需要待发布 ISO，干净 checkout / 日常 fast 上必然判红，
  不适合进秒级 fast 门禁；该子项留在发布前流程。`site-health` 由 `--offline` 在聚合层跳过。

---

## 4. 本次实际执行记录（2026-11-21，Windows 本机，Python 3.13.18）

### 4.1 字节码编译

命令：

```powershell
python -m py_compile scripts\vm-smoke.py scripts\release-gate.py scripts\gates.py
```

结果：**退出码 0**，无语法错误。

### 4.2 `vm-smoke.py` 实跑（本机无 QEMU）

命令：`python scripts\vm-smoke.py`

原始输出：

```
SKIP: 无 QEMU（未找到 qemu-system-x86_64）；环境不具备，跳过本次冒烟。
vm-smoke exit=77
```

**退出码 77（SKIP）**，未假成功。真实 QEMU 启动 / 串口命中路径在本机未执行（见 §6）。

### 4.3 `release-gate.py --offline` 实跑结果表

命令：`python scripts/release-gate.py --offline --json docs\证据\release-gate-offline-2026-11-21.json`

**合计：PASS 2 · FAIL 3 · SKIP 1；退出码 1。**

| 步骤 | 退出码 | 状态 | 耗时 | 关键输出 / 归因 |
|---|---:|---|---:|---|
| 站点自洽（gen-site --check） | 1 | FAIL | 0.8s | `FileNotFoundError: [WinError 2] 系统找不到指定的文件。` —— 脚本内部调 `git log -1` 取提交日期，**本机 PATH 无 git**（环境缺命令，非站点内容漂移）；CI checkout 环境带 git，行为需在 CI 复核 |
| 文档数字一致性 | 1 | FAIL | 0.6s | 收尾报告代码行数：文档 52659，实测 54338；README 源文件数 80 vs 实测 81。**既有文档/代码漂移**，本次未改文档与源码 |
| 文档链接门禁 | 0 | PASS | 2.4s | 检查 278 条本地链接，断链 0 条 |
| 文档规模三层门禁 | 1 | FAIL | 0.8s | INDEX/README/ROADMAP 声明 52,659 行 / 80 文件，实测 54,337 行 / 81 文件；漏登 `aether-compositor/src/wayland/fd.rs`；测试表 compositor 469 vs 490、aetherd 246 vs 258。**既有漂移** |
| 线上站点巡检 | — | SKIP | 0.0s | `--offline` 跳过线上站点巡检（未访问 https://aether.cbnac.com） |
| 发布就绪（ISO / 哈希 / isohybrid） | 0 | PASS | 0.4s | 9/9 项通过：release↔data 五项一致；`dist/aetheros-0.1-amd64-20261105.iso` 大小 27,262,976 字节、SHA256 `53580ae5…1711ed`、offset 510 = `55aa` |

> 三项 FAIL 均**不是本次新增脚本引入**：`site` 是本机缺 git 的环境问题；
> `doc-numbers` / `repo-stats` 是仓库既有文档数字漂移。按约束本次不改 `docs/` 声明数字与 `src/`，
> 如实呈现为红灯，待后续 `gen-site.py --refresh` / 文档回填处理。

### 4.4 YAML 校验

环境：Python 3.13.18 + PyYAML 6.0.3（`import yaml; yaml.safe_load(...)`）。

| workflow | 解析结果 |
|---|---|
| `.github/workflows/ci.yml` | OK（dict；jobs：`fast` / `release-gate` / `test`） |
| `.github/workflows/iso.yml` | OK |
| `.github/workflows/iso-install.yml` | OK |
| `.github/workflows/iso-smoke.yml` | OK |
| `.github/workflows/iso-update-e2e.yml` | OK |
| `.github/workflows/vm-smoke.yml` | OK（`on` 为字符串键；push 路径、schedule cron、jobs 结构均解析正确） |

补充：`vm-smoke.yml` 解析后 `on` 为 dict，含 `workflow_dispatch.inputs`、
`schedule: [{cron: '25 3 * * 1'}]`、`push.branches: [main]`、
`push.paths: [aether-compositor/**, aether-init/**, platform/**]`。

### 4.5 附加自检

- `release-gate.py --offline --only links,site-health`：PASS 1 · SKIP 1，退出码 0 —— `--only` 过滤与离线跳过均生效。
- 两个脚本 `--help` 正常，参数与 §2.2 / §3.1 一致。

---

## 5. 本地复跑命令

```powershell
# E3：VM 冒烟（无 QEMU 时退出 77；有 QEMU + ISO 时真实启动）
python scripts\vm-smoke.py
python scripts\vm-smoke.py --iso dist\aetheros-0.1-amd64-20261105.iso --timeout 180 --keep-logs

# E4：离线发布门禁（日常 / CI fast）
python scripts\release-gate.py --offline

# 联网模式（含线上站点巡检）
python scripts\release-gate.py

# 只跑指定子项 / 导出 JSON
python scripts\release-gate.py --offline --only links,site
python scripts\release-gate.py --offline --json out.json

# 门禁总入口（fast 档已含 release-gate 静态子项）
python scripts\gates.py --fast
```

---

## 6. 未验证项 / 已知红灯

1. **真实 QEMU 启动路径未验**：本机无 `qemu-system-x86_64`，只验证了 SKIP(77) 分支；
   串口关键字命中、超时判 FAIL、日志尾部输出、`--keep-logs` 保留路径均需在带 QEMU 的 Linux 环境（CI 或构建机）复跑确认。
2. **GitHub Actions 执行链路未跑**：workflow 仅做了 YAML 解析校验；artifact 下载、
   77 → neutral/skip 的 job 呈现、日志 artifact 上传、自托管 job 模板均未在真实 Actions 运行过。
3. **线上站点巡检未跑**：`--offline` 跳过 `site-health-check.py`；线上可用性、安全响应头、
   线上事实与 `site/data.json` 一致性本次未探测。
4. **`gen-site.py --check` 在带 git 环境的结果未复核**：本机 FAIL 归因于缺 git；
   CI（ubuntu checkout）上该步是 PASS 还是事实漂移，需首次 CI 运行后看日志。
5. **既有文档漂移红灯未修**：`doc-numbers` / `repo-stats` 报出的行数 / 文件数 / 测试数 /
   漏登文件 `wayland/fd.rs` 属仓库历史欠账，按本次约束未改，留待文档回填与 `--refresh` 流程处理。
6. **`gates.py --fast` 全量未在本次跑通**：仅做了 `py_compile` 与 release-gate 子命令级验证；
   fast 档追加的 release-gate 静态子项会复现 §4.3 中同类既有漂移红灯。
7. **自托管 / KVM 加速冒烟**：仅留注释模板，未配置 self-hosted label、未接真实串口或真机。
