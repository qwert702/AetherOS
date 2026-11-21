# 依赖迁移（DEP-MIGRATION）：ureq 2 → 3.5.1，minifb 0.27 → 0.29

- 日期：2026-11-21
- 工作副本：云主机（vm, Linux）`/tmp/aether/deps`（由 `/tmp/aether/src3` 整树复制的**独立副本**；`src3` 为已通过 854 项全量测试、等待回灌 Windows 主机的候选树，本次未改动）。
- 工具链：cargo 1.100.0 / rustc 1.100.0，target `x86_64-unknown-linux-gnu`；cargo 在线拉取 crate。
- 结论口径：本报告严格区分 **Linux 副本已编译跑测** / **非 Linux 预览分支未编译验证**；未跑通的部分如实标注，不写"应该没问题"。

---

## 1. 版本变更概览

| crate | 旧（Cargo.toml / Cargo.lock） | 新（Cargo.toml / Cargo.lock） | 所属 crate |
|---|---|---|---|
| `ureq` | `"2"` / 2.12.1 | `"3.5.1"` / 3.5.1 | `aetherd` |
| `minifb` | `"0.27"` / 0.27.0 | `"0.29"` / 0.29.0 | `aether-compositor`（仅非 Linux、非 musl 目标） |

调研依据：ureq 3.5.1 源码包内 `MIGRATE-2-to-3.md`、`src/agent.rs`、`src/request.rs`、`src/response.rs`、`src/body/mod.rs`、`src/error.rs`、`src/config.rs`；minifb 0.27.0 与 0.29.0 源码逐文件 diff。未凭猜测改 API。

---

## 2. ureq 2 → 3.5.1：breaking change 逐条清单

### 2.1 Agent 构建方式：`AgentBuilder` 移除

- **API 变化**：v2 用 `ureq::AgentBuilder::new().timeout(Duration).build()` 构建 Agent；v3 移除 `AgentBuilder`，改为 `Agent::config_builder()` 产出 `ConfigBuilder<AgentScope>`，`.timeout_global(Some(d))` 配置后 `.build()` 得到 `Config`，再经 `impl From<Config> for Agent` 转成 `Agent`。
- **受影响文件:行**：`aetherd/src/updater.rs:139-146`（`fn agent()`）。
- **改法**：
  ```rust
  let config = ureq::Agent::config_builder()
      .timeout_global(Some(HTTP_TIMEOUT))
      .build();
  config.into()
  ```
  `timeout_global` 覆盖重定向与响应体读取全程，对应 v2 `AgentBuilder::timeout(..)` 的语义。

### 2.2 响应体读取入口：响应改为 `http::Response<Body>`，体需先 `into_body()`

- **API 变化**：v2 的 `Response::into_reader()` / `Response::into_string()` 直接挂在响应上；v3 响应是 `http::Response<Body>`，正文在 `Body` 内，必须先 `resp.into_body()` 再调 `Body::into_reader()` / `Body::read_to_string()` / `Body::read_to_vec()`。`BodyReader` 仍实现 `std::io::Read`，`take`、`read_to_end`、循环 `read` 用法不变。
- **受影响文件:行**：
  - `aetherd/src/updater.rs:158`（`fetch_bytes`，reader）
  - `aetherd/src/updater.rs:191`（`download_to`，reader）
  - `aetherd/src/llm.rs:75-76`（`complete`，读响应字符串）
- **改法**：
  - `resp.into_reader()` → `resp.into_body().into_reader()`
  - `resp.into_string()` → `resp.into_body().read_to_string()`
  - `fetch_bytes` 中 `reader.take(..)` 按值消费 reader，`mut` 不再需要，去掉 `mut`（纯编译器建议，无行为变化）；`download_to` 循环内 `reader.read(..)` 仍需 `mut`，保留。

### 2.3 错误类型：`Error::Status(code, response)` 移除，错误不再携带响应体

- **API 变化**：v2 `ureq::Error::Status(u16, Response)` 可从错误臂里取回响应正文；v3 对应变体改为 `Error::StatusCode(u16)`，**只带状态码、不带响应**。这是本次迁移最关键的行为断点。
- **受影响文件:行**：`aetherd/src/llm.rs` 原 61-74 行的 `Err(ureq::Error::Status(code, r))` 臂 → 新实现 `llm.rs:59-95`。
- **改法**：请求层关闭"非 2xx 转 Err"，自己判状态码、自己读正文，以保留既有契约——"非 2xx 时必须把服务器正文带出来"（Key 无效 / 模型名不存在 / 限流 / 网关 502 的真正原因写在正文里）：
  ```rust
  let resp = match ureq::post(&url)
      .header("Authorization", &format!("Bearer {}", endpoint.api_key))
      .config()
      .http_status_as_error(false)
      .timeout_per_call(Some(std::time::Duration::from_secs(120)))
      .build()
      .send_json(body)
  {
      Ok(r) => r,
      Err(e) => { /* 传输层错误：anyhow::Error::new(e).context(...)，与 v2 一致 */ }
  };
  let code = resp.status().as_u16();
  let text = resp.into_body().read_to_string().with_context(|| ...)?;
  if !(200..=299).contains(&code) {
      // 带正文片段 bail（中英文两条分支与 v2 文案一致）
  }
  ```
  既有测试 `http_error_keeps_server_body`（401 必须带出 `invalid api key`）在新路径下通过，见 §4.1。

### 2.4 请求头方法改名：`.set(..)` → `.header(..)`

- **API 变化**：v2 `RequestBuilder::set(name, value)` 在 v3 改名为 `header(name, value)`。
- **受影响文件:行**：`aetherd/src/llm.rs:60`（`Authorization: Bearer <key>`）。
- **改法**：`.set("Authorization", ..)` → `.header("Authorization", ..)`。

### 2.5 请求级超时：`.timeout(Duration)` 移除，改走请求级配置

- **API 变化**：v2 请求上的 `.timeout(Duration)` 在 v3 移除；请求级超时通过 `.config().timeout_per_call(Some(d)).build()` 设置。
- **受影响文件:行**：
  - `aetherd/src/llm.rs:63`（`complete`，120 秒）
  - `aetherd/src/llm.rs:139-143`（`local_available`，2 秒探活）
- **改法**：`.timeout(d)` → `.config().timeout_per_call(Some(d)).build()`。

### 2.6 `json` feature 保留，未拆 crate

- **API 变化**：`json` feature 在 v3 仍存在（依赖 `serde` / `serde_json`）；`send_json(&T: Serialize)` 保留在 `RequestBuilder` 上（`#[cfg(feature = "json")]`）。无需把 JSON 序列化拆到独立调用点。
- **受影响文件:行**：`aetherd/Cargo.toml:15`。
- **改法**：`features = ["json"]` 原样保留；版本号 `"2"` → `"3.5.1"`。

### 2.7 行为差异：幂等方法不再自动重试（非 API 断点，记录在案）

- **API 变化**：v2 对 GET / HEAD 等幂等方法做内置自动重试；v3 无内置重试。
- **影响评估**：`updater.rs` 下载清单 / 签名 / 文件、`llm.rs::local_available` 探活在失败时不再自动重试一次。既有设计本就是"失败只记日志、不重试轰炸，下一轮（默认 6 小时）再看"（见 `updater.rs` 模块注释），本次**不补**重试逻辑，行为口径不变。

### 2.8 未改动的依赖关系

- `aetherd` 直接依赖 `ring = "0.17"` 未动：ureq 3 默认 `rustls` feature 经 `_ring` feature 仍把 ring 拉进依赖树（lock 内 `ring 0.17.14` 仍在），应用包完整性校验（Ed25519 / SHA-256）路径不受影响。

---

## 3. minifb 0.27 → 0.29：breaking change 逐条清单

### 3.1 `WindowOptions` 新增字段 `use_gpu: UseGPU`

- **API 变化**：0.29 在 `WindowOptions` 上新增 `use_gpu: UseGPU`（枚举 `Auto`（`#[default]`）/ `Disabled`，仅 Wayland 后端生效）。手写结构体字面量且不漏新字段才会编译失败；`Default` 实现已补 `use_gpu: UseGPU::default()`。
- **受影响文件:行**：`aether-compositor/src/app_main.rs:535`。
- **改法**：既有代码使用 `WindowOptions::default()`，未写字面量，**无需改动**即兼容。

### 3.2 限帧 API：`limit_update_rate` 已 deprecated，迁移到 `set_target_fps`

- **API 变化**：`limit_update_rate(Option<Duration>)` 自 minifb 0.26 起标 `#[deprecated]`（0.29 仍保留但会告警），推荐 `set_target_fps(usize)`。
- **受影响文件:行**：`aether-compositor/src/app_main.rs:537-538`（旧）→ `538-539`（新）。
- **改法**：
  ```rust
  // 旧
  #[allow(deprecated)]
  window.limit_update_rate(Some(std::time::Duration::from_micros(16_600)));
  // 新
  window.set_target_fps(60);
  ```
  `set_target_fps(60)` 内部按 `1/60 s ≈ 16.67 ms` 限帧，与原先 16.60 ms 的间隔等价；同时移除不再需要的 `#[allow(deprecated)]`。

### 3.3 枚举加变体（additive，非破坏性）

- **API 变化**：`MouseButton` 新增 `Back`、`Forward`；`Key` 仅加 `#[repr(u8)]`，71 个变体逐一比对**无增减**；`KeyRepeat` / `MouseMode` / `Scale` / `ScaleMode` 变体无变化。
- **受影响文件:行**：
  - `aether-compositor/src/app_main.rs:574`（`MouseButton::Left`）
  - `aether-compositor/src/app_main.rs:1279-1328`（`key_to_char` match）
  - `aether-compositor/src/app_main.rs:1335-1347`（`minifb_nav` match）
- **改法**：两处 match 均带 `_ =>` 通配臂，新增变体落入通配，无需改码；`MouseButton::Left`、`Key::*` 引用不变。

### 3.4 已核对签名未变的 API

- `Window::new(&str, usize, usize, WindowOptions) -> Result<Window>`：非 wasm32 签名未变；0.29 的第二个 `new` 重载仅 `#[cfg(target_arch = "wasm32")]`，不影响本项目。
- `update_with_buffer(&[u32], usize, usize) -> Result<()>`：签名未变（`app_main.rs:1269`）。
- `is_open` / `is_key_down` / `get_mouse_pos(MouseMode)` / `get_mouse_down(MouseButton)` / `get_keys_pressed(KeyRepeat)`：签名均未变。

---

## 4. 验证结果（本机真实执行摘要）

环境：vm Linux，cargo 1.100.0（rustc 1.100.0），`x86_64-unknown-linux-gnu`。

### 4.1 `cargo test --workspace --locked`

原始 `test result` 行（10 个测试目标）：

| 测试目标 | passed | failed | ignored |
|---|---:|---:|---:|
| aether-compositor（unittests src/main.rs） | 488 | 0 | 0 |
| aether-init（unittests src/main.rs） | 45 | 0 | 0 |
| aether-install（unittests src/main.rs） | 18 | 0 | 0 |
| aether-ipc（unittests src/lib.rs） | 15 | 0 | 0 |
| aether-ops（unittests src/main.rs） | 26 | 0 | 0 |
| aether-shell（unittests src/lib.rs） | 5 | 0 | 0 |
| aether-shell（unittests src/main.rs） | 0 | 0 | 0 |
| aetherd（unittests src/main.rs） | 257 | 0 | 0 |
| Doc-tests aether_ipc | 0 | 0 | 0 |
| Doc-tests aether_shell | 0 | 0 | 0 |

**合计：854 passed，0 failed，0 ignored。** 与基线 854 持平；未删除任何测试、未放宽任何断言。

`aetherd` 内与本次改动直接相关的 5 个 `llm::tests` 全部 `ok`：

```
test llm::tests::body_snippet_keeps_short_body_intact ... ok
test llm::tests::body_snippet_collapses_newlines_and_truncates ... ok
test llm::tests::http_error_keeps_server_body ... ok
test llm::tests::non_json_body_is_reported_with_snippet ... ok
test llm::tests::successful_completion_parses_message ... ok
```

`updater::tests` 6 个（配置 HTTPS/区间、清单/签名 URL、检查间隔、已暂存版本、版本比较）全部 `ok`。

### 4.2 `cargo clippy --workspace --all-targets --locked`

- **0 error**：日志中 `error[` / `error:` / `^error` 计数为 0；命令退出码 0，`Finished dev profile`。
- 现存 warning 均为基线既有（manifest unused-dependency、`unnecessary_cast`、`useless_vec`、`iter_cloned_collect`、`single_char_add_str`、`doc_lazy_continuation` 等）。本次改动文件内仅出现两条 warning，且都位于**未触碰的代码段**：
  - `aetherd/src/updater.rs:16`（模块 doc 注释缩进，文件头注释，本次未改）
  - `aether-compositor/src/app_main.rs:1969`（`iter().copied().collect()`，远离本次 538-539 行改动）
- 本次新增/改写的代码（`agent()`、`fetch_bytes`、`download_to`、`complete`、`local_available`、`set_target_fps` 调用）未产生新的 clippy warning。

### 4.3 Cargo.lock 可复现性

- `cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --all-targets --locked` 均带 `--locked` 通过，lock 无需再更新。
- 流程：先构建生成 lock，再 `cargo update -p ureq --precise 3.5.1` 钉住 ureq 版本（解析曾落到 3.5.2，按任务口径回钉 3.5.1）。
- lock 主要变动：
  - `ureq` 2.12.1 → 3.5.1（checksum `e7be7668…555d9d`）
  - `minifb` 0.27.0 → 0.29.0（checksum `d18dab18…34bd4`）
  - 传递依赖随解析更新：`ureq-proto 0.7.1`、`http 1.5.0`、`base64 0.23.1`、`cookie_store 0.22.1`、`web-time 1.1.0`、`wayland-*` 0.31/0.32 等；`ring 0.17.14` 仍在树。

---

## 5. 未验证项（如实标注）

1. **minifb 预览分支未在本机编译验证。**
   - 本机仅安装 `x86_64-unknown-linux-gnu` 一个 target。minifb 依赖声明在 `cfg(all(not(target_os = "linux"), not(target_env = "musl")))`；`preview_main` / `key_to_char` / `minifb_nav` 均为 `#[cfg(not(target_os = "linux"))]`。Linux 构建**根本不编译**这段代码，也不拉 minifb 的 Windows / X11 / Wayland 后端。
   - 尝试 `cargo check -p aether-compositor --target x86_64-pc-windows-gnu --locked`：target std 未安装，报 `error[E0463]: can't find crate for std`（提示用 `rustup target add x86_64-pc-windows-gnu` 下载）；`rustup target add` 等待用户确认超时，本次未执行。**该 target 未验证**。0.29 的 Windows 后端（winapi 0.3.9）链接、`set_target_fps` / `update_with_buffer` 真机上屏与输入行为，需在 Windows 主机补一次 `cargo check` / `cargo test`。
2. **ureq 3 的联机 HTTPS/TLS 链路未在本机实测。** 验证基于本地 TCP stub 假端点（`llm.rs` 内 `spawn_stub`）与编译期检查；rustls 证书校验、重定向跟随、gzip/brotli 解压、代理等路径未做外部联网实测。
3. **musl 静态编译（ISO rootfs）未覆盖。** 沿用既有跳过策略（ring 在 musl 下需 musl-gcc 交叉编译），本次未跑 musl 构建。

---

## 6. 回退方式

1. **版本号回退**：
   - `aetherd/Cargo.toml:15`：`ureq = { version = "3.5.1", features = ["json"] }` → `ureq = { version = "2", features = ["json"] }`
   - `aether-compositor/Cargo.toml:23`：`minifb = "0.29"` → `minifb = "0.27"`
2. **调用点还原**：
   - `updater.rs:139-146`：还原 `ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build()`；两处 `resp.into_body().into_reader()` 还原为 `resp.into_reader()`；`fetch_bytes` 的 reader 恢复 `mut`。
   - `llm.rs:59-95`：还原 `.set("Authorization", ..).timeout(Duration::from_secs(120)).send_json(body)` 与 `Err(ureq::Error::Status(code, r))` 错误臂（`r.into_string().unwrap_or_default()`）；`resp.into_body().read_to_string()` 还原为 `resp.into_string()`；`local_available` 还原 `.timeout(Duration::from_secs(2))`。
   - `app_main.rs:538-539`：还原 `#[allow(deprecated)] window.limit_update_rate(Some(std::time::Duration::from_micros(16_600)));`
3. **锁文件**：`cargo update -p ureq --precise 2.12.1 -p minifb --precise 0.27.0`，或直接用 `src3` 基线 `Cargo.lock` 覆盖。
4. 回退后重跑 `cargo test --workspace --locked` 与 `cargo clippy --workspace --all-targets --locked`，确认回到 854 基线。

---

## 7. 改动文件清单（相对项目根路径）

| 文件 | 状态 | 行数（src3 → deps） | 说明 |
|---|---|---|---|
| `aetherd/Cargo.toml` | 修改 | 36 → 36 | ureq 版本 `2` → `3.5.1`，`json` feature 保留 |
| `aetherd/src/updater.rs` | 修改 | 408 → 416 | Agent 构建、两处 reader 入口适配 |
| `aetherd/src/llm.rs` | 修改 | 290 → 301 | 请求头/超时/错误臂/响应体读取适配；**CRLF 行尾保留** |
| `aether-compositor/Cargo.toml` | 修改 | 27 → 27 | minifb 版本 `0.27` → `0.29` |
| `aether-compositor/src/app_main.rs` | 修改 | 2745 → 2746 | 限帧迁移到 `set_target_fps(60)` |
| `Cargo.lock` | 修改 | 1466 → 1595 | ureq/minifb 及传递依赖更新 |
| `docs/报告/依赖迁移（DEP-MIGRATION）-2026-11-21.md` | 新增 | — | 本报告 |

**红线遵守情况**：未改动 `/tmp/aether/src3`；未改 `docs/README.md`、`.github/`、`scripts/gates.py`；未删除功能；未放宽任何测试断言；minifb 未编译验证的部分已在 §5 如实标注。
