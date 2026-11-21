# Wayland SCM_RIGHTS 收包（G1）实现状态报告

- 日期：2026-11-21
- 工作包：`aether-compositor`
- 范围：`aether-compositor/src/wayland/**`（必要时 `main.rs` 的 mod 声明；本次未改动 `main.rs`）
- 状态口径：本报告严格区分 **代码已实现** / **Linux 实机已验证** / **Windows 已测**，三者不混写。

---

## 0. 交付位置与回灌状态（务必先读）

- 本次改动位于**云主机（vm, Linux）工作副本** `/tmp/aether/src3`，**尚未回灌**到 QYY 真实项目工作树：
  `D:\CBN-HT\Desktop\AI编程\除了dsh以外的项目\系统\电脑系统\Aether`（编写本报告时该设备离线）。
- 回灌载体（与代码同目录，见 §8）：
  - `wayland-fd-delta.tar.gz`：仅新增/修改的源码文件（相对项目根路径）。
  - `wayland-fd-delta.manifest.txt`：每个文件标注新增 / 修改。
  - `wayland-fd-delta.patch`：基于改动前快照的 unified diff（`diff -ruN`，未使用 git）。
- **QYY 恢复在线后**：先核对磁盘现状（离线前曾写入过一版未验证的初版 `fd.rs`，回灌必须以本增量包的修订版覆盖），再解压 delta 包覆盖对应路径，然后按 §7 重跑 Windows GNU 的 test 与 clippy。
- 本报告中"已实现"指代码已落盘并在 Linux 副本上编译通过；**不等于** Windows 已编译通过，也**不等于**真实 Wayland 客户端已端到端跑通。

### 范围外干预（不属于本增量包，回灌时不要覆盖）

- 共享副本上另一工作流正在拆分 `draw`。其 WIP 把 `fbdev.rs` 的 `blit_rects` 方法误贴进了 `mod tests` 模块，导致整个 crate 测试目标编译失败（`self` 参数只允许出现在关联函数中）。
- 为解除本地编译阻塞，本次仅把该方法**逐字**移回 `impl Fbdev` 块内，未改其逻辑。该文件 **不在** delta 包内，回灌时不要用本副本的 `fbdev.rs` 覆盖 QYY；两处 WIP 需在 draw 拆分工作流里对齐。
- 未修改 `aether-compositor/src/draw.rs` 与 `src/draw/` 下任何文件。

---

## 1. 已实现范围（代码层面）

### 1.1 fd 接收层（新增 `wayland/fd.rs`）

- Linux 分支通过 `recvmsg` 读取 `msghdr.msg_control`，按 `cmsghdr` 链表解析 `SCM_RIGHTS` ancillary data，把随附的 `i32` fd 按线上到达顺序取出。
- ancillary 解析 `parse_cmsgs` 抽成独立可测函数：手工拼一块 cmsg 缓冲即可单测，不必起 socketpair。
- CMSG 对齐采用手工 `cmsg_align`（`CMSG_ALIGN` 语义）算术，**不依赖** `CMSG_NXTHDR` / `CMSG_LEN` / `CMSG_SPACE` 辅助宏（glibc 的 `libc` 绑定未导出 `CMSG_NXTHDR`）。
- 单次收包 fd 上限 `MAX_FDS = 16`；超限返回 `FdError::TooManyFds`。
- 错误枚举 `FdError`：`UnsupportedPlatform` / `RecvFailed(errno)` / `TooManyFds(n)`，均实现 `Display`。
- 控制缓冲按 16 个 i32 的 CMSG_SPACE 预留，避免一条报文 fd 过多被内核截断后解析到残缺数据。
- 平台判断入口 `fds_supported()`（`cfg!(target_os = "linux")`）。

### 1.2 shm 映射层（修改 `wayland/shm.rs`）

- `map_pool_fd(fd, size)`：
  - **Linux**：`mmap(MAP_SHARED, PROT_READ)` → 把映射区拷进 `Vec<u8>` → `munmap` + `close(fd)`；成功/失败路径都关闭 fd。
  - **非 Linux**：返回 `ShmError::FdTransportUnsupported`，不触碰 fd、不 panic。
- 新增错误变体：`FdTransportUnsupported` / `MapFailed(errno)` / `BadPoolSize(size)`。
- pool 长度上限 `MAX_POOL_SIZE = 256 MiB`；0 字节或超限先关 fd 再拒绝。
- `close_fd(fd)`：Linux 真 `close`；非 Linux 空操作（避免平台相关类型泄漏到公共签名）。
- 像素读取 `read_pixels`（既有纯逻辑，未改其协议校验语义）继续负责 offset/stride/format → RGBA 转换。

### 1.3 会话分发层（修改 `wayland/session.rs`）

- 双入口共用 `handle_inner(buf, fds, strict_fds)`：
  - `handle(buf)`：测试 / 非 fd 路径，fd 槽保留 `-1` 占位，pool 记 `Pending`。
  - `handle_with_fds(buf, &mut fds)`：真实 socket 通道，严格模式；队列耗尽仍有 `h` 参数 → 协议错误（对端未按协议随附 fd）。
- `fill_fd_slots`：按参数出现顺序 FIFO 回填 `Arg::Fd`，非 fd 参数不受影响。
- `handle_shm`（`wl_shm.create_pool`，签名 `"nhi"`）：fd 取参数索引 1、size 取索引 2；size 为负或 `insert_object` 失败先 `close_fd`；fd < 0 记 `Pending`；fd ≥ 0 走 `map_pool_fd` → `Ready(Vec<u8>)`。
- `wl_registry.bind(wl_shm)` 后回两条 `format` 事件：`ARGB8888 = 0`、`XRGB8888 = 1`。
- 新增取值器 `arg_fd`。

### 1.4 服务端连接层（修改 `wayland/server.rs`，整体 `cfg(target_os = "linux")`）

- `handle_conn` 由普通 `read` 改为 `fd::recvmsg_fds`；维护连接级 `fd_queue: Vec<i32>`，收到的 fd 入队，由 `handle_with_fds` 消费。
- 协议错误仍按既有路径回 `wl_display.error` 后断开。
- 连接断开时关闭队列残余 fd（`SCM_RIGHTS` 传过来的是独立描述符副本，不关会泄漏），再把槽位置 `None`。

### 1.5 协议编解码（修改 `wayland/wire.rs`）

- `h`（fd）参数**不编码进消息体**，解码时保留槽位填 `Arg::Fd(-1)`，值由 fd 队列侧回填（既有语义，本次补测试钉死）。
- 新增两条编解码测试：fd 值不参与序列化；多 fd 槽位顺序保持。

### 1.6 最小 `wl_shm` 客户端路径（代码已打通）

`wl_registry.bind(wl_shm)` → 收到两条 `format` 事件 → `wl_shm.create_pool(fd, size)` → mmap 成 `Ready` → `wl_shm_pool.create_buffer(offset, w, h, stride, format)` → `wl_surface.attach(buffer, x, y)` → `commit` 时 `read_pixels` 读出 RGBA 写入 `SurfaceState.pixels`。

支持格式：`ARGB8888 (0)`、`XRGB8888 (1)`；其他格式返回 `UnsupportedFormat`。

---

## 2. 协议流程（socket → 上屏）

```text
UnixListener.accept
        │
        ▼
recvmsg(stream, &mut buf)                       ← fd.rs（Linux）
        │  msghdr.msg_control 里的 ancillary data
        ▼
parse_cmsgs(control, controllen)                ← 解析 SCM_RIGHTS，逐个 i32 fd
        │
        ▼
fd_queue (Vec<i32>, 连接级 FIFO)
        │
        ▼
Session::handle_with_fds(buf, &mut fd_queue)
        │  wire::decode 解码消息体（h 槽位先填 -1）
        ▼
fill_fd_slots: 按参数顺序从队列取真实 fd 回填 Arg::Fd
        │
        ▼
handle_shm → create_pool(new_id, fd, size)
        │  size 校验 → insert_object 失败/负 size 先 close_fd
        ▼
map_pool_fd(fd, size)                           ← shm.rs（Linux mmap / 非 Linux stub）
        │  mmap PROT_READ MAP_SHARED → Vec<u8> → munmap + close
        ▼
PoolData::Ready(Vec<u8>)                        ← 此前停在 Pending，现可提交
        │
        ▼
create_buffer → BufferInfo { pool_id, offset, width, height, stride, format }
        │
        ▼
wl_surface.attach(buffer) → SurfaceState.attached_buffer
        │
        ▼
commit → read_pixels(pool, info) → SurfaceState.pixels = Some(RGBA8888)
```

---

## 3. Linux / 非 Linux 分支差异

| 路径 | Linux（`target_os = "linux"`） | Windows / 其他平台 |
|---|---|---|
| `recvmsg_fds` | 真实 `recvmsg` + ancillary 解析 | 入口不编译（仅 Linux 下 `pub use imp::recvmsg_fds`）；无 Unix socket fd 传递语义 |
| `map_pool_fd` | `mmap(MAP_SHARED, PROT_READ)` 读出字节后解除映射并关 fd | 返回 `ShmError::FdTransportUnsupported`，不碰 fd、不 panic |
| `close_fd` | 真实 `close(fd)` | 空操作（stub 路径不可能进来真实 fd） |
| `server` 模块 | 整体 `cfg(target_os = "linux")` 编译 | 不编译；`WaylandServer` 重导出同样被 cfg 隔离 |
| 真实 fd 端到端测试 | `#[cfg(target_os = "linux")]` 编译运行 | 不编译；改跑 `#[cfg(not(target_os = "linux"))]` stub 错误测试 |
| 纯逻辑测试（编解码、几何、像素转换） | 跨平台运行 | 跨平台运行 |

依赖侧：`libc` 在 `aether-compositor/Cargo.toml` 中仅 `cfg(target_os = "linux")` 声明；非 Linux 目标不引入 `libc`。

---

## 4. 取舍与已知限制（代码现状，不是缺陷掩盖）

1. **非零拷贝**：当前 `Session` 需要 `Clone`（连接线程每批消息克隆一份同步给渲染主循环），而 mmap 映射区不能随 Clone 复制。`map_pool_fd` 读出字节后即 `munmap` + `close`。生产化阶段应改为持有映射区（`Arc` 包裹）做零拷贝，避免每帧整池拷贝。
2. **映射权限取 `PROT_READ`**：合成器只从 pool 读像素，不回写映射区；只读权限同时兼容客户端以 `O_RDWR` 打开的常规 pool 与只读 fd（只读 fd 上申请 `PROT_WRITE` 会 `mmap` 失败 `EACCES`）。
3. **格式范围**：仅 `ARGB8888` / `XRGB8888`；其余格式按 `UnsupportedFormat` 拒绝。
4. **`wl_shm_pool.resize` 未实现**：opcode 1 当前忽略（与改动前一致）。
5. **未覆盖**：多 buffer 双缓冲、damage/局部更新、`wl_subsurface`、真实 memfd/`/dev/shm` pool、多 surface 布局合成。

---

## 5. 新增测试清单（共 13 个）

| # | 测试名 | 文件 | 平台标注 | Linux vm 实机执行 |
|---|---|---|---|---|
| 1 | `unsupported_error_is_explicit_on_non_linux` | `wayland/fd.rs` | 跨平台（非 Linux 断言；Linux 下 early-return） | 已运行（无断言触发） |
| 2 | `parse_synthetic_scm_rights_buffer` | `wayland/fd.rs` | `#[cfg(target_os = "linux")]` | 已通过 |
| 3 | `socketpair_transfers_bytes_and_one_fd` | `wayland/fd.rs` | `#[cfg(target_os = "linux")]` | 已通过 |
| 4 | `zero_pool_size_is_rejected` | `wayland/shm.rs` | 跨平台 | 已通过 |
| 5 | `maps_real_file_fd_into_bytes` | `wayland/shm.rs` | `#[cfg(target_os = "linux")]` | 已通过 |
| 6 | `non_linux_fd_mapping_is_explicit_error` | `wayland/shm.rs` | `#[cfg(not(target_os = "linux"))]` | 不编译（Linux 下排除） |
| 7 | `bind_wl_shm_advertises_formats` | `wayland/session.rs` | 跨平台 | 已通过 |
| 8 | `fd_queue_fills_slots_in_fifo_order` | `wayland/session.rs` | 跨平台 | 已通过 |
| 9 | `create_pool_with_real_fd_maps_and_commits_pixels` | `wayland/session.rs` | `#[cfg(target_os = "linux")]` | 已通过 |
| 10 | `missing_fd_is_protocol_error` | `wayland/session.rs` | 跨平台（严格模式） | 已通过 |
| 11 | `real_fd_on_unsupported_platform_is_protocol_error` | `wayland/session.rs` | `#[cfg(not(target_os = "linux"))]` | 不编译（Linux 下排除） |
| 12 | `fd_value_is_not_serialized` | `wayland/wire.rs` | 跨平台 | 已通过 |
| 13 | `multiple_fds_keep_slots_in_order` | `wayland/wire.rs` | 跨平台 | 已通过 |

既有 `wire.rs` / `protocol.rs` 编解码断言未改动，继续通过。

---

## 6. 验证结果（真实命令输出摘要）

### 6.1 Linux 云主机（vm，已实机执行）

- 环境：`cargo 1.100.0 (3acc79c0c 2026-11-05)` / `rustc 1.100.0 (0ef1bc386 2026-11-09)`，`x86_64-unknown-linux-gnu`；工作副本 `/tmp/aether/src3`。
- 命令：

  ```bash
  export PATH="$HOME/.cargo/bin:$PATH"
  cd /tmp/aether/src3
  cargo test -p aether-compositor --locked
  cargo clippy -p aether-compositor --all-targets --locked
  ```

- `cargo test` 原始结果：

  ```text
  running 488 tests
  test result: ok. 488 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.02s
  ```

- `cargo clippy`：退出码 **0**，**0 error**。存量 warning（非本次改动引入，未处理）：
  - `aether-compositor/Cargo.toml:11` unused dependency `log`（workspace 配置导致）；
  - `wayland/wire.rs` 三处 `% 4` 建议改 `is_multiple_of`（改动前基线即存在）；
  - `wayland/protocol.rs:178` 常量默认 `'static` 生命周期（改动前基线即存在）；
  - `draw/`、`fbdev.rs` 内若干 dead_code / 参数过多 / 冗余转换 warning，属另一工作流 WIP，不在本次范围。
- 原始输出留存（vm 本地，**不在 delta 包内**）：`/tmp/aether/vm-test-raw.txt`、`/tmp/aether/vm-clippy-raw.txt`。

### 6.2 Windows GNU（本次改动后**未复跑**）

- 编写本报告时 QYY 离线，本次改动后的 Windows 编译 / 测试 / clippy **未在 Windows 上执行**。
- 改动前基线（历史记录，非本次复跑）：Windows `+stable-x86_64-pc-windows-gnu` 下 `cargo test -p aether-compositor --locked` 为 474 passed / 0 failed；clippy 退出码 0、0 error。
- 代码层审计（人工，非编译器实证）：`std::os::unix::*`、`libc::*`、`UnixStream` / `UnixListener`、`FromRawFd` / `IntoRawFd` 等平台相关标识均位于 `#[cfg(target_os = "linux")]` 分支或其 Linux-only 测试内；非 Linux 公共路径只调用 `map_pool_fd` / `close_fd` 的 stub 分支。**审计结论不能替代 Windows 编译结果**，需在 QYY 恢复后按 §7 实证。

### 6.3 未测项（以下均不得视为"已支持"）

- 真实 Wayland 客户端（标准 `wl_shm` 程序）连接本机合成器并真实上屏的端到端流程。
- 真实 memfd / `/dev/shm` pool fd 的传递与映射；测试以普通临时文件 fd 代替。
- Windows GNU 工具链下本次改动的编译、测试、clippy 结果。
- `wl_shm_pool.resize`、多 buffer 双缓冲、damage 局部更新、多 surface 合成。
- 高并发 / 长连接下的 fd 泄漏与描述符表压力测试。

---

## 7. 可复跑命令

### Linux（vm）

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /tmp/aether/src3
cargo test -p aether-compositor --locked
cargo clippy -p aether-compositor --all-targets --locked
```

### Windows（QYY 恢复后，仅跑 `-p aether-compositor`，不跑全量 workspace）

```powershell
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu test -p aether-compositor --locked
& "C:\Users\cbn\.cargo\bin\cargo.exe" +stable-x86_64-pc-windows-gnu clippy -p aether-compositor --all-targets --locked
```

---

## 8. 变更文件清单（相对项目根）

| 文件路径 | 状态 | 说明 |
|---|---|---|
| `aether-compositor/src/wayland/fd.rs` | 新增 | SCM_RIGHTS 收包层：`recvmsg`、ancillary 解析、错误枚举、平台判断、3 个测试 |
| `aether-compositor/src/wayland/mod.rs` | 修改 | 增加 `pub mod fd;` |
| `aether-compositor/src/wayland/shm.rs` | 修改 | `map_pool_fd` / `close_fd` / 新错误变体 / 3 个测试 / 模块文档 |
| `aether-compositor/src/wayland/session.rs` | 修改 | `handle_with_fds` / `handle_inner` / `fill_fd_slots` / `handle_shm` 重写 / format 事件 / `arg_fd` / 5 个测试 |
| `aether-compositor/src/wayland/server.rs` | 修改 | `handle_conn` 改 `recvmsg_fds` + 连接级 fd 队列 + 断开关闭残余 fd |
| `aether-compositor/src/wayland/wire.rs` | 修改 | 2 个 fd 编解码测试（编解码语义未改） |
| `docs/报告/Wayland fd 传递实现状态（WAYLAND-FD）-2026-11-21.md` | 新增 | 本报告 |

未改动：`wayland/object.rs`、`wayland/protocol.rs`、`main.rs`、`draw.rs`、`draw/`、`fbdev.rs`（见 §0 范围外干预说明）。
