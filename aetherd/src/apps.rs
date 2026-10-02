//! 应用（外部程序）的安装与卸载。
//!
//! ## 为什么要做这个
//!
//! 镜像里没有包管理器，Buildroot 的模型是构建期定死、运行期不改系统。但"能不能跑外部程序"
//! 实测门槛很低 —— 静态链接能跑；简单的动态程序也能跑（2026-09-28 用 chroot 进构建输出实测：
//! Ubuntu 24.04 编的 hello 直接跑通，因为镜像的 glibc 2.38 向后兼容）。
//! 真正会挂的是**缺共享库**（宿主 `/usr/bin/ls` 拷进去报 `libselinux.so.1: cannot open`）。
//!
//! 所以缺的**不是 ABI 兼容，是安装机制**：装到哪儿（要持久）、谁放得进去、装完怎么被找到、怎么卸。
//!
//! ## 设计约束（都来自"这系统不止自己用，还要给别人用"）
//!
//! 1. **装之前就要能判断能不能跑** —— 见 [`preflight`]。别人给的程序装上了跑不起来，
//!    比装不上更糟：他只会觉得"这系统坏了"，而不会想到是缺一个 `.so`。
//! 2. **id 必须是安全的路径段** —— 清单里的 `id` 会变成目录名，含 `/` 或 `..` 就是目录穿越。
//! 3. **包里不许有符号链接** —— 符号链接能指向系统任意位置，是逃逸面。
//! 4. **只往 `<apps_root>/<id>/` 写**，不动系统区。写白名单也只放这一个子目录
//!    （`/var/log/aether` 也在 `/var` 里，放开整个 `/var` 等于允许 AI 灭证）。
//!
//! ## 布局
//!
//! ```text
//! /var/apps/<id>/
//!   app.json     清单（必须）
//!   bin/hello    可执行文件（entry 指向它）
//!   lib/         可选：自带的共享库，启动时通过 LD_LIBRARY_PATH 生效
//! ```
//!
//! 终端可用性靠生成**包装脚本**（`link_all`）：把 `<id>` 放进 `/usr/local/bin`，
//! 脚本里设好 `LD_LIBRARY_PATH` 再 exec。系统区在内存盘里，重启会还原，
//! 所以这件事必须每次开机重做 —— `/init` 调 `aetherd app link`。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 应用安装根目录（Linux）。
#[cfg(target_os = "linux")]
pub const DEFAULT_DIR: &str = "/var/apps";

/// 清单文件名（固定在包根）。
pub const MANIFEST_NAME: &str = "app.json";

/// 计算一段字节的 SHA-256，返回小写十六进制（与 `sha256sum` 输出同格式）。
///
/// 为什么需要它（2026-10-02 审计 L-9）：`scripts/serve-apps.py` 一直在回
/// `X-SHA256` 头，但**文档化的 guest 安装流程从来没用过它** —— 也就是说
/// "宿主 → guest 拉包"这条链路在传输被篡改时毫无察觉。
///
/// 生产入口是流式的 [`sha256_file`]；这个按字节版本是它的**测试基准**
/// （拿公开测试向量对账），因此只在测试下编译。
#[cfg(test)]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut s = String::with_capacity(64);
    for b in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 计算文件的 SHA-256（分块读，大包不吃内存）。
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("打不开文件：{}", path.display()))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let digest = ctx.finish();
    let mut s = String::with_capacity(64);
    for b in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    Ok(s)
}

/// 预检最多读这么多字节 —— 只解析文件头，不需要读完。
const MAX_PREFLIGHT_BYTES: u64 = 4 * 1024 * 1024;

/// 应用根目录。
///
/// 可用 `AETHER_APPS_DIR` 覆盖。为什么需要这个开关：测试要能在任意机器上跑
/// （Linux 下 `/var/apps` 要 root 才建得出来，而开发/CI 用户通常不是 root），
/// 部署时也可能想把应用放到另一块盘。**aether-init 不注入该变量**，系统内走默认值。
pub fn default_root() -> PathBuf {
    if let Some(p) = std::env::var_os("AETHER_APPS_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(DEFAULT_DIR)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("aether-apps")
    }
}

/// 应用清单。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// 目录名，也是终端里的命令名。只允许 `[a-z0-9]` 与 `-` `_` `.`，且首字符为字母数字。
    pub id: String,
    /// 显示名（可含中文，给 Dock 和 AI 用）。
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// 包内**相对**路径，指向可执行文件。
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub desc: String,
}

/// 已安装的一个应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub bytes: u64,
}

/// `id` 是否安全（能直接当目录名用）。
///
/// 这条是安全边界，不是风格检查：`id` 来自外部给的清单，会被拼进路径。
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// 从包目录读清单。读不出、缺字段、id 非法都在这里拒掉。
pub fn load_manifest(pkg: &Path) -> Result<Manifest> {
    let path = pkg.join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("读不到清单 {}", path.display()))?;
    let m: Manifest =
        serde_json::from_str(&text).with_context(|| format!("清单不是合法 JSON: {}", path.display()))?;
    if !valid_id(&m.id) {
        bail!(
            "非法的应用 id「{}」：只允许字母数字与 - _ . 且以字母数字开头（它会被当成目录名）",
            m.id
        );
    }
    if m.name.trim().is_empty() {
        bail!("清单缺少 name");
    }
    if m.entry.trim().is_empty() {
        bail!("清单缺少 entry（指向可执行文件的包内相对路径）");
    }
    Ok(m)
}

/// 校验 entry：必须是**包内**的相对路径，不能借 `..` 或绝对路径跑出去。
pub fn resolve_entry(pkg: &Path, entry: &str) -> Result<PathBuf> {
    let p = Path::new(entry);
    if p.is_absolute() {
        bail!("entry 必须是包内相对路径（实得绝对路径 {entry}）");
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!("entry 不许含 ..（{entry}）");
    }
    let full = pkg.join(p);
    let md = std::fs::metadata(&full).with_context(|| format!("entry 指向的文件不存在: {entry}"))?;
    if !md.is_file() {
        bail!("entry 指向的不是普通文件: {entry}");
    }
    Ok(full)
}

/// 递归复制目录，返回总字节数。
///
/// **拒绝符号链接**：符号链接可以指向 `/etc/shadow` 之类的系统路径，
/// 装完之后那个链接就躺在应用目录里，等于把逃逸面装进系统。
fn copy_tree(src: &Path, dst: &Path) -> Result<u64> {
    let mut total = 0u64;
    std::fs::create_dir_all(dst).with_context(|| format!("建目录失败 {}", dst.display()))?;
    for e in std::fs::read_dir(src).with_context(|| format!("读目录失败 {}", src.display()))? {
        let e = e?;
        let ty = e.file_type()?;
        let to = dst.join(e.file_name());
        if ty.is_symlink() {
            bail!("包里含符号链接（{}），拒绝安装", e.path().display());
        }
        if ty.is_dir() {
            total += copy_tree(&e.path(), &to)?;
        } else if ty.is_file() {
            std::fs::copy(e.path(), &to)
                .with_context(|| format!("复制失败 {}", e.path().display()))?;
            total += e.metadata()?.len();
        }
        // 其它类型（设备节点、FIFO 等）忽略
    }
    Ok(total)
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// 安装一个应用（源是**目录**）。返回安装结果，其中 `preflight.detail` 是要给用户看的一句话。
pub fn install(root: &Path, src: &Path) -> Result<(Installed, Preflight)> {
    let src = std::fs::canonicalize(src)
        .with_context(|| format!("源目录不存在: {}", src.display()))?;
    if !src.is_dir() {
        bail!("安装源必须是目录（现在只支持目录形式的包）: {}", src.display());
    }
    let manifest = load_manifest(&src)?;
    let entry = resolve_entry(&src, &manifest.entry)?;

    // **预检在复制之前做**：别先装进去再说跑不起来。
    let pre = preflight(&src, &entry);
    if let PreflightKind::Unusable = pre.kind {
        bail!("这个包不能用：{}\n（没有改动系统）", pre.detail);
    }

    let dest = root.join(&manifest.id);
    if dest.exists() {
        bail!(
            "「{}」已经装过了（{}）。要重装请先 `aetherd app remove {}`",
            manifest.id,
            dest.display(),
            manifest.id
        );
    }
    std::fs::create_dir_all(root).with_context(|| format!("建目录失败 {}", root.display()))?;
    let bytes = match copy_tree(&src, &dest) {
        Ok(n) => n,
        Err(e) => {
            // 半途失败不留垃圾：否则下次会撞"已经装过了"
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
    };
    make_executable(&dest.join(&manifest.entry))?;
    Ok((
        Installed { manifest, dir: dest, bytes },
        pre,
    ))
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let md = std::fs::metadata(p)?;
    let mut perm = md.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(p, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<()> {
    Ok(())
}

/// 列出已装应用（按 id 排序）。读不出清单的目录会被跳过 —— 一个坏目录不该让整个列表消失。
pub fn list(root: &Path) -> Vec<Installed> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Installed> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let dir = e.path();
            let manifest = load_manifest(&dir).ok()?;
            Some(Installed { manifest, bytes: dir_size(&dir), dir })
        })
        .collect();
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

/// 找一个已装应用。
pub fn find(root: &Path, id: &str) -> Option<Installed> {
    if !valid_id(id) {
        return None;
    }
    let dir = root.join(id);
    if !dir.is_dir() {
        return None;
    }
    let manifest = load_manifest(&dir).ok()?;
    Some(Installed { manifest, bytes: dir_size(&dir), dir })
}

/// 卸载：把安装目录移到回收站（而不是直接删）—— 删错了还能捞回来。
pub fn remove(root: &Path, id: &str) -> Result<PathBuf> {
    remove_into(root, id, &crate::trash::default_root())
}

/// 同 [`remove`]，但显式指定回收站根。
///
/// 为什么不直接用全局的 `trash::default_root()`：那是进程级共享状态，测试并行跑时会
/// 两个用例同时往同一个目录里塞条目、抢同一个序号（2026-09-28 实测：并发下
/// `install_list_remove_roundtrip` 偶发失败）。把回收站根变成参数，测试就能各用各的。
pub fn remove_into(root: &Path, id: &str, trash_root: &Path) -> Result<PathBuf> {
    let Some(app) = find(root, id) else {
        bail!("没有装「{id}」（或它的清单坏了，用 app_list 看看）");
    };
    std::fs::create_dir_all(trash_root).ok();
    let name = crate::trash::trash(trash_root, &app.dir)
        .with_context(|| format!("移入回收站失败（{id}）"))?;
    Ok(trash_root.join(name))
}

/// 生成的包装脚本内容（终端里敲 `<id>` 就能用）。
///
/// 为什么要包装脚本而不是直接软链：应用可能自带 `lib/` 与 `share/terminfo`，
/// 需要 `LD_LIBRARY_PATH` / `TERMINFO_DIRS` 才能跑起来；软链没法带环境变量。
///
/// ⚠️ 这个脚本是给**目标系统**（Linux）执行的，所以路径分隔符和换行都必须固定成
/// POSIX 形式 —— 开发机是 Windows，`Path::join` 会给出反斜杠。测试
/// `wrapper_sets_library_path_and_passes_args` 就是抓这个的。
pub fn wrapper_script(app: &Installed) -> String {
    let entry = posix(&app.dir.join(&app.manifest.entry));
    let lib = posix(&app.dir.join("lib"));
    // 包内自带的终端条目优先，后面接系统目录：全屏程序（htop/vim/less）找不到 terminfo
    // 会直接报 "Error opening terminal"。见 SYSTEM_TERMINFO_DIRS 的说明。
    let terminfo = format!(
        "{}:{}",
        posix(&app.dir.join("share/terminfo")),
        SYSTEM_TERMINFO_DIRS.join(":")
    );
    let mut fixed_args = String::new();
    for a in &app.manifest.args {
        fixed_args.push_str(&shell_quote(a));
        fixed_args.push(' ');
    }
    format!(
        "#!/bin/sh\n\
         # 由 `aetherd app link` 生成（AetherOS 已装应用包装脚本），请勿手改。\n\
         # 系统区在内存盘里，重启会还原 —— 每次开机重新生成。\n\
         LD_LIBRARY_PATH={lib}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\n\
         export LD_LIBRARY_PATH\n\
         TERMINFO_DIRS={ti}${{TERMINFO_DIRS:+:$TERMINFO_DIRS}}\n\
         export TERMINFO_DIRS\n\
         exec {entry} {fixed_args}\"$@\"\n",
        lib = shell_quote(&lib),
        ti = shell_quote(&terminfo),
        entry = shell_quote(&entry),
        fixed_args = fixed_args,
    )
}

/// 路径 → POSIX 形式（Windows 上的 `\` 换成 `/`）。
fn posix(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 把已装应用挂进 `bindir`（默认由 `/init` 调）。返回写入的命令名。
///
/// 先清掉上次生成的脚本再重建：否则卸载过的应用会留下一个指向空目录的死脚本，
/// 用户敲进去只会看到"文件不存在"。
pub fn link_all(root: &Path, bindir: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(bindir)
        .with_context(|| format!("建目录失败 {}", bindir.display()))?;
    let apps = list(root);

    // 清理：bindir 里所有带本工具标记的脚本
    if let Ok(rd) = std::fs::read_dir(bindir) {
        for e in rd.flatten() {
            let p = e.path();
            let Ok(head) = std::fs::read_to_string(&p) else { continue };
            if head.contains("由 `aetherd app link` 生成") {
                let _ = std::fs::remove_file(&p);
            }
        }
    }

    let mut names = Vec::new();
    for app in &apps {
        let target = bindir.join(&app.manifest.id);
        std::fs::write(&target, wrapper_script(app))
            .with_context(|| format!("写包装脚本失败 {}", target.display()))?;
        make_executable(&target)?;
        names.push(app.manifest.id.clone());
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// 安装前预检
// ---------------------------------------------------------------------------

/// 预检结论的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightKind {
    /// 应该能跑
    Runnable,
    /// 能装上，但很可能跑不起来（例如依赖库只在系统里缺、又不自带）
    Risky,
    /// 装都别装（不是可执行文件、格式都不对）
    Unusable,
}

/// 预检结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    pub kind: PreflightKind,
    /// 给用户看的一句话（装成功也要展示 —— 他得知道这个东西能不能跑）
    pub detail: String,
}

/// 系统里找共享库的目录。镜像里没有 `ld.so.cache`，所以就是这几条固定路径。
pub const LIB_DIRS: [&str; 4] = ["/lib", "/usr/lib", "/lib64", "/usr/lib64"];

/// 解析出来的 ELF 关键信息。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ElfInfo {
    /// PT_INTERP：动态加载器的路径。为 None 说明是静态链接。
    pub interp: Option<String>,
    /// DT_NEEDED：需要的共享库名字。
    pub needed: Vec<String>,
}

/// 从字节里解析 ELF64（小端）的 PT_INTERP 与 DT_NEEDED。
///
/// 纯函数、不碰文件系统 —— 这样可以在开发机上用构造出来的字节做确定性测试。
/// 返回 `None` 表示这不是一个能看懂的 ELF64 可执行文件。
///
/// 索引一律走 `.get()`：这个函数处理的是**外部给的、可能损坏的文件**，
/// 一次越界 panic 就是 aetherd 挂掉（`essential` 服务，重启桌面临时消失）。
pub fn parse_elf(bytes: &[u8]) -> Option<ElfInfo> {
    // e_ident：魔数 + class(2=64bit) + data(1=小端)
    if bytes.len() < 64 || bytes.get(0..4)? != b"\x7fELF" {
        return None;
    }
    if *bytes.get(4)? != 2 || *bytes.get(5)? != 1 {
        return None;
    }
    let u16at = |o: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(o..o + 2)?.try_into().ok()?))
    };
    let u32at = |o: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(o..o + 4)?.try_into().ok()?))
    };
    let u64at = |o: usize| -> Option<u64> {
        Some(u64::from_le_bytes(bytes.get(o..o + 8)?.try_into().ok()?))
    };

    let e_type = u16at(16)?;
    // ET_EXEC(2) / ET_DYN(3) 才是有意义的可执行文件
    if e_type != 2 && e_type != 3 {
        return None;
    }
    let e_phoff = u64at(0x20)? as usize;
    let e_phentsize = u16at(0x36)? as usize;
    let e_phnum = u16at(0x38)? as usize;
    if e_phentsize < 56 {
        return None;
    }

    let mut interp: Option<String> = None;
    let mut dyn_off: Option<(usize, usize)> = None; // (offset, filesz)
    let mut loads: Vec<(u64, u64, usize, u64)> = Vec::new(); // (vaddr, memsz, offset, filesz)

    for i in 0..e_phnum.min(128) {
        let base = e_phoff.checked_add(i.checked_mul(e_phentsize)?)?;
        let p_type = u32at(base)?;
        let p_offset = u64at(base + 8)? as usize;
        let p_vaddr = u64at(base + 16)?;
        let p_filesz = u64at(base + 32)?;
        match p_type {
            1 => loads.push((p_vaddr, p_filesz, p_offset, p_filesz)), // PT_LOAD
            2 => dyn_off = Some((p_offset, p_filesz as usize)),       // PT_DYNAMIC
            3 => {
                // PT_INTERP
                let sz = (p_filesz as usize).min(4096);
                let raw = bytes.get(p_offset..p_offset.checked_add(sz)?)?;
                let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                interp = Some(String::from_utf8_lossy(&raw[..end]).into_owned());
            }
            _ => {}
        }
    }

    let mut needed = Vec::new();
    if let Some((off, sz)) = dyn_off {
        let mut strtab_vaddr: Option<u64> = None;
        let mut strsz: u64 = 0;
        let mut needed_offs: Vec<u64> = Vec::new();
        let count = (sz / 16).min(4096);
        for i in 0..count {
            let b = off.checked_add(i * 16)?;
            let d_tag = u64at(b)?;
            let d_val = u64at(b + 8)?;
            match d_tag {
                0 => break,          // DT_NULL
                1 => needed_offs.push(d_val), // DT_NEEDED
                5 => strtab_vaddr = Some(d_val), // DT_STRTAB
                10 => strsz = d_val,  // DT_STRSZ
                _ => {}
            }
        }
        if let Some(strtab) = strtab_vaddr {
            // vaddr → 文件偏移：找覆盖它的 PT_LOAD
            let file_off = loads.iter().find_map(|(vaddr, memsz, off, filesz)| {
                if strtab >= *vaddr && strtab < vaddr.saturating_add(*memsz) {
                    let delta = strtab - vaddr;
                    if delta < *filesz {
                        return Some(*off + delta as usize);
                    }
                }
                None
            });
            if let Some(base) = file_off {
                let limit = if strsz > 0 { strsz as usize } else { 4096 };
                let tbl = bytes.get(base..base.saturating_add(limit).min(bytes.len()));
                if let Some(tbl) = tbl {
                    for o in needed_offs {
                        let o = o as usize;
                        let Some(rest) = tbl.get(o..) else { continue };
                        let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
                        if end == 0 {
                            continue;
                        }
                        needed.push(String::from_utf8_lossy(&rest[..end]).into_owned());
                    }
                }
            }
        }
    }

    Some(ElfInfo { interp, needed })
}

/// 读文件并预检。`pkg` 是包根（用来找 `lib/` 与 `share/terminfo`）。
pub fn preflight(pkg: &Path, entry: &Path) -> Preflight {
    let head = match read_head(entry) {
        Ok(b) => b,
        Err(e) => {
            return Preflight {
                kind: PreflightKind::Unusable,
                detail: format!("读不了可执行文件 {}：{e}", entry.display()),
            }
        }
    };

    // `#!` 脚本：只要有解释器就能跑
    if head.starts_with(b"#!") {
        let line = head.split(|b| *b == b'\n').next().unwrap_or(&head);
        let interp = String::from_utf8_lossy(&line[2..]).trim().to_string();
        let exe = interp.split_whitespace().next().unwrap_or("");
        if exe.is_empty() {
            return Preflight {
                kind: PreflightKind::Unusable,
                detail: "脚本的 #! 行是空的".into(),
            };
        }
        return if Path::new(exe).exists() {
            Preflight {
                kind: PreflightKind::Runnable,
                detail: format!("Shell 脚本，解释器 {exe} 在系统里"),
            }
        } else {
            Preflight {
                kind: PreflightKind::Risky,
                detail: format!("Shell 脚本，但解释器 {exe} 不在系统里，跑不起来"),
            }
        };
    }

    let Some(elf) = parse_elf(&head) else {
        return Preflight {
            kind: PreflightKind::Unusable,
            detail: format!(
                "{} 既不是 ELF 可执行文件也不是 #! 脚本 —— 可能是别的平台（Windows/macOS）的\
                 程序，或者只是个数据文件",
                entry.display()
            ),
        };
    };

    if elf.interp.is_none() && elf.needed.is_empty() {
        return Preflight {
            kind: PreflightKind::Runnable,
            detail: "静态链接的可执行文件，不依赖任何共享库".into(),
        };
    }

    // 包里带了 glibc 家族 → 必然起不来，而且比"缺库"更糟（缺库是缺，这个是有害）。
    // 放在依赖闭包之前判：这种包根本不该被放行。
    let bad_glibc = bundled_glibc_family(pkg);
    if !bad_glibc.is_empty() {
        return Preflight {
            kind: PreflightKind::Risky,
            detail: format!(
                "包里带了 **glibc 家族**的库（{}）—— 不能这么做：加载器会去用包里那份 libc，\
                 与系统那份撞 GLIBC_PRIVATE 私有符号，程序直接起不来\
                 （实测报 `undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE`）。\
                 把这些文件从包的 lib/ 里删掉即可；用 scripts/mkapp.py 打包不会出现这种情况。",
                bad_glibc.join("、")
            ),
        };
    }

    // 依赖闭包：动态程序真正会挂的地方不是"第一层缺库"，而是"链上某一环缺库"。
    // 例：htop → libncursesw → libtinfo；只查第一层会把"装了还是跑不起来"判成"能跑"。
    let (missing, needs_curses) = walk_dependencies(pkg, entry);
    if !missing.is_empty() {
        return Preflight {
            kind: PreflightKind::Risky,
            detail: format!(
                "动态链接，**缺 {} 个共享库**：{}。装上也跑不起来 —— \
                 要么让作者改成静态链接（`gcc -static`），要么把缺的 .so 一起放进包的 lib/ 目录",
                missing.len(),
                missing.join("、")
            ),
        };
    }

    // curses 程序还必须有 terminfo 条目。缺了会直接报 "Error opening terminal: <TERM>"，
    // 而且比缺库更难查：库都在、进程能启动、就是起不来。2026-09-29 实测：镜像原本
    // 没有 terminfo，而 compositor 给 PTY 设的 TERM 是 xterm-256color（pty.rs），
    // 于是所有全屏程序都跑不起来。
    if needs_curses && !terminfo_available(pkg, &SYSTEM_TERMINFO_DIRS) {
        return Preflight {
            kind: PreflightKind::Risky,
            detail: "动态链接，共享库都齐了，但**没有该程序需要的终端条目（terminfo）**：\
                     全屏程序（htop / vim / less 这类）启动时会直接报 \
                     \"Error opening terminal\"。系统 terminfo 目录为空、包里也没有 \
                     share/terminfo —— 用打包器（scripts/mkapp.py）加 --terminfo 重新打一个，\
                     它会把条目放进包的 share/terminfo。"
                .into(),
        };
    }

    let how = if elf.needed.is_empty() {
        "依赖都在系统里"
    } else if needs_curses {
        "需要的共享库都在（系统里或包内 lib/），terminfo 也找得到"
    } else {
        "需要的共享库都在（系统里或包内 lib/）"
    };
    Preflight { kind: PreflightKind::Runnable, detail: format!("动态链接，{how}") }
}

fn read_head(p: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(p)?;
    let mut buf = Vec::new();
    f.take(MAX_PREFLIGHT_BYTES).read_to_end(&mut buf)?;
    Ok(buf)
}

/// 系统 terminfo 目录。curses 程序靠它把 `TERM` 映射成终端能力表。
pub const SYSTEM_TERMINFO_DIRS: [&str; 2] = ["/usr/share/terminfo", "/etc/terminfo"];

/// 依赖闭包最多遍历多少个文件（防病态递归把预检拖死）。
const MAX_DEP_FILES: usize = 64;

/// 这个名字是不是 curses 类库（用它们的程序需要 terminfo）。
fn is_curses_lib(name: &str) -> bool {
    name.starts_with("libncurses")
        || name.starts_with("libtinfo")
        || name.starts_with("libtermcap")
        || name.starts_with("libcurses")
}

/// 包里带了 glibc 家族的库吗？返回文件名列表。
///
/// 为什么必须拦：这套库**只能**让程序起不来。加载器一旦通过 `LD_LIBRARY_PATH` 用到包里
/// 那份 libc，就会与系统那份撞 GLIBC_PRIVATE 私有符号。2026-09-29 实测报错：
/// `undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE`。
/// 打包器（scripts/mkapp.py）本来就不会收它们，但包可能是手搓的 —— 对照实验证实
/// 没有这道检查时预检会放行一个注定崩掉的包。
pub fn bundled_glibc_family(pkg: &Path) -> Vec<String> {
    const FAMILY_PREFIX: [&str; 8] = [
        "libc.so", "libm.so", "libpthread.so", "libdl.so", "librt.so",
        "libutil.so", "libcrypt.so", "ld-linux",
    ];
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(pkg.join("lib")) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if FAMILY_PREFIX.iter().any(|p| name.starts_with(p)) {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// 在「包内 lib/ + 系统库目录」里找这个共享库。找不到返回 `None`。
///
/// 库名来自 ELF 的 `DT_NEEDED`（外部输入）：**只允许是文件名**，含路径分隔符的一律不认，
/// 否则 `../../etc/passwd` 这种名字会让预检跑去查包外的路径。
pub fn find_lib(lib: &str, app_lib: &Path) -> Option<PathBuf> {
    if lib.is_empty() || lib.contains('/') || lib.contains('\\') {
        return None;
    }
    let local = app_lib.join(lib);
    if local.is_file() {
        return Some(local);
    }
    LIB_DIRS
        .iter()
        .map(|d| Path::new(d).join(lib))
        .find(|p| p.is_file())
}

/// 递归走依赖闭包，返回（缺失的库名，是否依赖 curses 库）。
///
/// 每一层都往下读：系统库自己也可能依赖别的库，只看入口那一层会漏。
fn walk_dependencies(pkg: &Path, entry: &Path) -> (Vec<String>, bool) {
    let app_lib = pkg.join("lib");
    let mut queue = vec![entry.to_path_buf()];
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut curses = false;

    while let Some(path) = queue.pop() {
        if seen.len() >= MAX_DEP_FILES {
            break;
        }
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let Ok(head) = read_head(&path) else { continue };
        let Some(elf) = parse_elf(&head) else { continue };
        for name in &elf.needed {
            if is_curses_lib(name) {
                curses = true;
            }
            match find_lib(name, &app_lib) {
                Some(found) => {
                    let k = std::fs::canonicalize(&found).unwrap_or(found);
                    if !seen.contains(&k) {
                        queue.push(k);
                    }
                }
                None => {
                    if !missing.contains(name) {
                        missing.push(name.clone());
                    }
                }
            }
        }
    }
    (missing, curses)
}

/// curses 程序的 terminfo 找不找得到：包内 `share/terminfo` 或任一系统目录有内容即可。
///
/// `system_dirs` 做成参数是为了能在开发机上测（Windows 没有 `/usr/share/terminfo`）。
pub fn terminfo_available(pkg: &Path, system_dirs: &[&str]) -> bool {
    fn has_entries(d: &Path) -> bool {
        std::fs::read_dir(d)
            .map(|rd| rd.flatten().any(|e| e.path().exists()))
            .unwrap_or(false)
    }
    let local = pkg.join("share").join("terminfo");
    if local.is_dir() && has_entries(&local) {
        return true;
    }
    system_dirs
        .iter()
        .any(|d| Path::new(d).is_dir() && has_entries(Path::new(d)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-9：SHA-256 必须与 `sha256sum` 逐字节一致 —— 用公开测试向量钉住。
    ///
    /// 校验值写错比没有校验更糟：用户会以为"验过了"。所以这里钉死标准向量，
    /// 而不是"自己算一遍再跟自己对"。
    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 输出形态：小写、定长 64
        let h = sha256_hex(b"x");
        assert_eq!(h.len(), 64, "SHA-256 十六进制必须是 64 字符：{h}");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "必须是小写十六进制：{h}"
        );
    }

    /// 文件哈希要与"同一份字节的哈希"一致，且能跨内部读缓冲边界。
    #[test]
    fn sha256_file_matches_byte_hash() {
        let dir = tmp("sha-file");
        let p = dir.join("x.bin");
        let data = vec![0xA5u8; 200 * 1024]; // 跨 64KiB 分块
        std::fs::write(&p, &data).unwrap();
        assert_eq!(sha256_file(&p).unwrap(), sha256_hex(&data));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("aether-apps-test-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_pkg(dir: &Path, id: &str, entry_body: &[u8]) -> PathBuf {
        let pkg = dir.join("pkg");
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        std::fs::write(
            pkg.join(MANIFEST_NAME),
            format!(
                r#"{{"id":"{id}","name":"示例","version":"1","entry":"bin/hello","desc":"测试"}}"#
            ),
        )
        .unwrap();
        std::fs::write(pkg.join("bin/hello"), entry_body).unwrap();
        pkg
    }

    #[test]
    fn id_must_be_a_safe_path_segment() {
        for bad in ["", "..", "../etc", "a/b", "/abs", ".hidden", "有中文", "a b", "x\"y"] {
            assert!(!valid_id(bad), "{bad:?} 不该被接受");
        }
        for good in ["hello", "a", "my-app_2", "app.v1"] {
            assert!(valid_id(good), "{good:?} 应该被接受");
        }
    }

    #[test]
    fn entry_cannot_escape_the_package() {
        let d = tmp("escape");
        std::fs::create_dir_all(d.join("pkg")).unwrap();
        for bad in ["/etc/passwd", "../outside", "bin/../../etc"] {
            let e = resolve_entry(&d.join("pkg"), bad);
            assert!(e.is_err(), "entry {bad:?} 不该被接受");
        }
    }

    #[test]
    fn install_list_remove_roundtrip() {
        let d = tmp("roundtrip");
        let root = d.join("apps");
        let pkg = write_pkg(&d, "hello", b"#!/bin/sh\necho hi\n");

        let (installed, pre) = install(&root, &pkg).expect("安装应成功");
        assert_eq!(installed.manifest.id, "hello");
        assert!(installed.dir.join("bin/hello").is_file());
        assert!(pre.kind != PreflightKind::Unusable);

        // 重复安装必须被挡住（否则会静默覆盖掉用户装的东西）
        let err = install(&root, &pkg).expect_err("重复安装应被拒绝");
        assert!(err.to_string().contains("已经装过"), "实得: {err}");

        let items = list(&root);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].manifest.id, "hello");
        assert!(find(&root, "hello").is_some());

        // 卸载走回收站，不是直接删
        let trashed = remove_into(&root, "hello", &d.join("trash")).expect("卸载应成功");
        assert!(list(&root).is_empty());
        assert!(trashed.exists(), "卸载应该把它挪进回收站而不是删掉: {}", trashed.display());
        remove_into(&root, "hello", &d.join("trash")).expect_err("再卸一次应该报没有装");
    }

    #[test]
    fn install_refuses_symlinks_inside_the_package() {
        #[cfg(unix)]
        {
            let d = tmp("symlink");
            let root = d.join("apps");
            let pkg = write_pkg(&d, "evil", b"#!/bin/sh\n");
            std::os::unix::fs::symlink("/etc/passwd", pkg.join("bin/leak")).unwrap();
            let err = install(&root, &pkg).expect_err("含符号链接的包应被拒绝");
            assert!(err.to_string().contains("符号链接"), "实得: {err}");
            // 失败的安装不该留下半个目录
            assert!(!root.join("evil").exists(), "半途失败必须清干净");
        }
    }

    #[test]
    fn wrapper_sets_library_path_and_passes_args() {
        let app = Installed {
            manifest: Manifest {
                id: "hello".into(),
                name: "示例".into(),
                version: "1".into(),
                entry: "bin/hello".into(),
                args: vec!["--greet".into()],
                desc: String::new(),
            },
            dir: PathBuf::from("/var/apps/hello"),
            bytes: 0,
        };
        let s = wrapper_script(&app);
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("LD_LIBRARY_PATH='/var/apps/hello/lib'"), "实得:\n{s}");
        assert!(s.contains("exec '/var/apps/hello/bin/hello' '--greet' \"$@\""), "实得:\n{s}");
        // 脚本是给目标系统（Linux）跑的：不能有 CR，也不能有反斜杠路径
        assert!(!s.contains('\r'), "包装脚本不能有 CR（shebang 会失效）:\n{s}");
        assert!(!s.contains('\\'), "包装脚本里不能出现 Windows 路径分隔符:\n{s}");
    }

    #[test]
    fn link_all_creates_wrappers_and_cleans_up_stale_ones() {
        let d = tmp("link");
        let root = d.join("apps");
        let bindir = d.join("bin");
        let pkg = write_pkg(&d, "hello", b"#!/bin/sh\necho hi\n");
        install(&root, &pkg).expect("安装应成功");

        let names = link_all(&root, &bindir).expect("挂载应成功");
        assert_eq!(names, vec!["hello"]);
        let w = bindir.join("hello");
        assert!(w.is_file(), "应该生成包装脚本");
        let script = std::fs::read_to_string(&w).unwrap();
        let expected = posix(&root.join("hello").join("bin/hello"));
        assert!(
            script.contains(&expected),
            "脚本要指向安装目录里的 entry（期望含 {expected}）:\n{script}"
        );

        // 卸载后再挂载：上次生成的脚本必须被清掉，否则用户敲进去只有"文件不存在"
        remove_into(&root, "hello", &d.join("trash")).expect("卸载应成功");
        let names = link_all(&root, &bindir).expect("再挂载应成功");
        assert!(names.is_empty());
        assert!(!w.exists(), "陈旧的包装脚本必须被清理掉");

        // 用户自己在 bindir 里放的文件不能被误删
        let mine = bindir.join("my-own-tool");
        std::fs::write(&mine, "#!/bin/sh\necho mine\n").unwrap();
        link_all(&root, &bindir).unwrap();
        assert!(mine.exists(), "不该动非本工具生成的文件");
    }

    // ---- ELF 解析：用构造出来的字节测，跨平台都能跑 ----

    /// 造一个最小可用的 ELF64：PT_LOAD + PT_INTERP + PT_DYNAMIC(两个 DT_NEEDED)。
    fn synth_elf(interp: Option<&str>, needed: &[&str]) -> Vec<u8> {
        const PHOFF: usize = 64;
        const PHENT: usize = 56;
        const DYN_OFF: usize = 0x100;
        const LOAD_OFF: usize = 0x100;
        const LOAD_VADDR: u64 = 0x100;
        const STRTAB_VADDR: u64 = 0x180;

        let nph = if interp.is_some() { 3 } else { 2 };
        let mut b = vec![0u8; 0x400];
        // e_ident
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2; // 64 位
        b[5] = 1; // 小端
        b[16..18].copy_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
        b[0x20..0x28].copy_from_slice(&(PHOFF as u64).to_le_bytes());
        b[0x36..0x38].copy_from_slice(&(PHENT as u16).to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&(nph as u16).to_le_bytes());

        // 字符串表：先留一个 \0，再依次放库名
        let mut strtab = vec![0u8];
        let mut offs = Vec::new();
        for n in needed {
            offs.push(strtab.len() as u64);
            strtab.extend_from_slice(n.as_bytes());
            strtab.push(0);
        }
        b[STRTAB_VADDR as usize..STRTAB_VADDR as usize + strtab.len()]
            .copy_from_slice(&strtab);

        // .dynamic：DT_NEEDED × n、DT_STRTAB、DT_STRSZ、DT_NULL
        let mut items: Vec<(i64, u64)> = offs.iter().map(|o| (1i64, *o)).collect();
        items.push((5, STRTAB_VADDR));
        items.push((10, strtab.len() as u64));
        items.push((0, 0));
        for (i, (tag, val)) in items.iter().enumerate() {
            let base = DYN_OFF + i * 16;
            b[base..base + 8].copy_from_slice(&(*tag as u64).to_le_bytes());
            b[base + 8..base + 16].copy_from_slice(&val.to_le_bytes());
        }
        put_ph(&mut b, 0, 1, LOAD_OFF as u64, LOAD_VADDR, 0x200); // PT_LOAD 覆盖 dynamic+strtab
        let mut idx = 1;
        if let Some(s) = interp {
            let io = 0x300usize;
            b[io..io + s.len()].copy_from_slice(s.as_bytes());
            b[io + s.len()] = 0;
            // 注意 PT_LOAD 已占一位，PT_INTERP 在它之后
            put_ph(&mut b, idx, 3, io as u64, 0, (s.len() + 1) as u64);
            idx += 1;
        }
        put_ph(&mut b, idx, 2, DYN_OFF as u64, 0, (items.len() * 16) as u64); // PT_DYNAMIC
        b
    }

    /// 写一条 program header（写成自由函数而不是闭包：闭包会长期可变借用整个 `b`，
    /// 后面就再也写不了别的字段了）。
    fn put_ph(b: &mut [u8], i: usize, ty: u32, off: u64, vaddr: u64, filesz: u64) {
        const PHOFF: usize = 64;
        const PHENT: usize = 56;
        let base = PHOFF + i * PHENT;
        b[base..base + 4].copy_from_slice(&ty.to_le_bytes());
        b[base + 8..base + 16].copy_from_slice(&off.to_le_bytes());
        b[base + 16..base + 24].copy_from_slice(&vaddr.to_le_bytes());
        b[base + 32..base + 40].copy_from_slice(&filesz.to_le_bytes());
    }

    #[test]
    fn parses_interp_and_needed_from_a_dynamic_elf() {
        let bytes = synth_elf(Some("/lib64/ld-linux-x86-64.so.2"), &["libc.so.6", "libm.so.6"]);
        let e = parse_elf(&bytes).expect("应该解析成功");
        assert_eq!(e.interp.as_deref(), Some("/lib64/ld-linux-x86-64.so.2"));
        assert_eq!(e.needed, vec!["libc.so.6", "libm.so.6"]);
    }

    #[test]
    fn static_elf_has_no_interp_and_no_needed() {
        let bytes = synth_elf(None, &[]);
        let e = parse_elf(&bytes).expect("应该解析成功");
        assert_eq!(e.interp, None);
        assert!(e.needed.is_empty());
    }

    #[test]
    fn garbage_and_truncated_input_never_panics() {
        assert!(parse_elf(b"").is_none());
        assert!(parse_elf(b"not an elf at all").is_none());
        let full = synth_elf(Some("/lib64/ld-linux-x86-64.so.2"), &["libc.so.6"]);
        // 从头到尾逐长度截断 —— 每个前缀都不许 panic
        for n in 0..full.len() {
            let _ = parse_elf(&full[..n]);
        }
        // 头部自称有 100 个 program header，但文件很短
        let mut evil = full.clone();
        evil[0x38..0x3a].copy_from_slice(&100u16.to_le_bytes());
        let _ = parse_elf(&evil);
    }

    #[test]
    fn preflight_reports_missing_libs_for_a_package_without_them() {
        let d = tmp("preflight");
        let pkg = write_pkg(&d, "dyn", &synth_elf(Some("/lib64/ld-linux-x86-64.so.2"), &["libdefinitely-not-here.so"]));
        let pre = preflight(&pkg, &pkg.join("bin/hello"));
        assert_eq!(pre.kind, PreflightKind::Risky);
        assert!(pre.detail.contains("libdefinitely-not-here.so"), "实得: {}", pre.detail);
    }

    #[test]
    fn preflight_says_runnable_when_the_package_ships_the_lib() {
        let d = tmp("preflight-ships");
        let pkg = write_pkg(&d, "dyn", &synth_elf(Some("/lib64/ld-linux-x86-64.so.2"), &["libmine.so"]));
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(pkg.join("lib/libmine.so"), b"fake").unwrap();
        let pre = preflight(&pkg, &pkg.join("bin/hello"));
        assert_eq!(pre.kind, PreflightKind::Runnable, "{}", pre.detail);
    }

    #[test]
    fn preflight_rejects_a_non_executable_file() {
        let d = tmp("preflight-bad");
        let pkg = write_pkg(&d, "data", b"this is just text, not a program");
        let pre = preflight(&pkg, &pkg.join("bin/hello"));
        assert_eq!(pre.kind, PreflightKind::Unusable);
        assert!(pre.detail.contains("不是 ELF"), "实得: {}", pre.detail);
    }

    #[test]
    fn preflight_recognizes_shebang_scripts() {
        let d = tmp("preflight-sh");
        let pkg = write_pkg(&d, "sh", b"#!/bin/sh\necho hi\n");
        let pre = preflight(&pkg, &pkg.join("bin/hello"));
        #[cfg(unix)]
        assert_eq!(pre.kind, PreflightKind::Runnable, "{}", pre.detail);
        #[cfg(not(unix))]
        assert_eq!(pre.kind, PreflightKind::Risky, "{}", pre.detail);
    }

    #[test]
    fn manifest_must_carry_id_name_entry() {
        let d = tmp("manifest");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(MANIFEST_NAME);
        std::fs::write(&p, r#"{"id":"ok","name":"n","entry":"e"}"#).unwrap();
        assert_eq!(load_manifest(&d).unwrap().id, "ok");
        std::fs::write(&p, r#"{"id":"../evil","name":"n","entry":"e"}"#).unwrap();
        assert!(load_manifest(&d).is_err(), "越界 id 必须被拒");
        std::fs::write(&p, r#"{"id":"ok","name":"n"}"#).unwrap();
        assert!(load_manifest(&d).is_err(), "缺 entry 必须被拒");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(load_manifest(&d).is_err(), "坏 JSON 必须被拒");
    }

    // ---- 依赖闭包与 terminfo（2026-09-29 新增：为"能跑市面软件"服务）----

    /// 造一个包：entry 是合成 ELF（动态，依赖 `entry_needs`），`lib/` 里放若干合成库。
    fn write_elf_pkg(
        dir: &Path,
        id: &str,
        entry_needs: &[&str],
        libs: &[(&str, &[&str])],
    ) -> PathBuf {
        let interp = Some("/lib64/ld-linux-x86-64.so.2");
        let pkg = dir.join("pkg");
        std::fs::create_dir_all(pkg.join("bin")).unwrap();
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(
            pkg.join(MANIFEST_NAME),
            format!(r#"{{"id":"{id}","name":"示例","version":"1","entry":"bin/x"}}"#),
        )
        .unwrap();
        std::fs::write(pkg.join("bin/x"), synth_elf(interp, entry_needs)).unwrap();
        for (name, needs) in libs {
            std::fs::write(pkg.join("lib").join(name), synth_elf(interp, needs)).unwrap();
        }
        pkg
    }

    #[test]
    fn dependency_closure_is_recursive() {
        let d = tmp("dep-closure");
        // entry → libA → libB，而 libB 既不在包里也不在系统路径里。
        // 只查第一层会得出"能跑"（libA 在包里），实际上装上去一定挂。
        let pkg = write_elf_pkg(&d, "deep", &["libA.so.1"], &[("libA.so.1", &["libB.so.1"])]);
        let (missing, _) = walk_dependencies(&pkg, &pkg.join("bin/x"));
        assert_eq!(missing, vec!["libB.so.1"], "递归一层才发现的缺失库必须被报出来");
    }

    #[test]
    fn package_local_lib_satisfies_dependency() {
        let d = tmp("dep-local");
        let pkg = write_elf_pkg(&d, "local", &["libA.so.1"], &[("libA.so.1", &[])]);
        let (missing, _) = walk_dependencies(&pkg, &pkg.join("bin/x"));
        assert!(missing.is_empty(), "包内自带的库应算已满足，实得缺失 {missing:?}");
        assert!(find_lib("libA.so.1", &pkg.join("lib")).is_some());
    }

    #[test]
    fn lib_names_with_path_separators_are_rejected() {
        // 库名来自 ELF 的 DT_NEEDED（外部输入）：不能借它去查包外路径
        let d = tmp("lib-name-traversal");
        let pkg = d.join("pkg");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(pkg.join("lib/libok.so"), b"x").unwrap();
        assert!(find_lib("libok.so", &pkg.join("lib")).is_some(), "正常库名要能找到");
        for bad in ["../libok.so", "../../etc/shadow", "/etc/passwd", "a/b.so", "", "x\\y.so"] {
            assert!(find_lib(bad, &pkg.join("lib")).is_none(), "{bad:?} 不该被接受");
        }
    }

    #[test]
    fn curses_dependency_is_detected() {
        let d = tmp("dep-curses");
        // libncursesw 在包里（不缺库），但程序是 curses 类 → 需要 terminfo
        let pkg = write_elf_pkg(
            &d,
            "curses",
            &["libncursesw.so.6"],
            &[("libncursesw.so.6", &["libtinfo.so.6"]), ("libtinfo.so.6", &[])],
        );
        let (missing, curses) = walk_dependencies(&pkg, &pkg.join("bin/x"));
        assert!(missing.is_empty(), "实得缺失 {missing:?}");
        assert!(curses, "依赖 libncursesw + libtinfo 应被认作 curses 程序");
    }

    #[test]
    fn curses_lib_names_are_classified() {
        for n in ["libncursesw.so.6", "libncurses.so.5", "libtinfo.so.6", "libtermcap.so.2"] {
            assert!(is_curses_lib(n), "{n} 应被认作 curses 库");
        }
        for n in ["libc.so.6", "libz.so.1", "libssl.so.3", "libm.so.6"] {
            assert!(!is_curses_lib(n), "{n} 不该被认作 curses 库");
        }
    }

    #[test]
    fn terminfo_lookup_prefers_package_then_system() {
        let d = tmp("terminfo");
        let pkg = d.join("pkg");
        let sysdir = d.join("sys-term");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::create_dir_all(sysdir.join("x")).unwrap();
        std::fs::write(sysdir.join("x").join("xterm-256color"), b"x").unwrap();
        let none = d.join("does-not-exist");
        let none_s = [none.to_str().unwrap()];

        // 包内没有 → 用系统目录
        assert!(terminfo_available(&pkg, &[sysdir.to_str().unwrap()]));
        // 系统目录也没有 → 判缺
        assert!(!terminfo_available(&pkg, &none_s));
        // 包内 share/terminfo 有内容 → 即使系统没有也算有（这正是打包器要带的东西）
        std::fs::create_dir_all(pkg.join("share/terminfo/x")).unwrap();
        std::fs::write(pkg.join("share/terminfo/x/xterm-256color"), b"x").unwrap();
        assert!(terminfo_available(&pkg, &none_s), "包内条目应被认到");
    }

    #[test]
    fn preflight_handles_curses_program_without_terminfo() {
        let d = tmp("pf-terminfo");
        // 库都在包里，唯一可能的问题是 terminfo。而系统上有没有 terminfo 取决于平台：
        // 开发机（Windows）没有，镜像里现在有了 —— 所以断言随平台分流，别写死。
        let pkg = write_elf_pkg(
            &d,
            "curses2",
            &["libncursesw.so.6"],
            &[("libncursesw.so.6", &[])],
        );
        let sys_has = SYSTEM_TERMINFO_DIRS.iter().any(|p| Path::new(p).is_dir());

        let pre = preflight(&pkg, &pkg.join("bin/x"));
        if sys_has {
            assert_eq!(pre.kind, PreflightKind::Runnable, "{}", pre.detail);
        } else {
            assert_eq!(pre.kind, PreflightKind::Risky, "{}", pre.detail);
            assert!(pre.detail.contains("terminfo"), "应指出是 terminfo 的问题: {}", pre.detail);
        }

        // 包里带上 terminfo 之后，任何平台都必须是 Runnable
        std::fs::create_dir_all(pkg.join("share/terminfo/x")).unwrap();
        std::fs::write(pkg.join("share/terminfo/x/xterm-256color"), b"x").unwrap();
        let pre = preflight(&pkg, &pkg.join("bin/x"));
        assert_eq!(pre.kind, PreflightKind::Runnable, "{}", pre.detail);
    }

    #[test]
    fn preflight_flags_bundled_glibc_family() {
        // 对照实验发现的缺口：手搓一个带宿主 libc.so.6 的包，旧预检会放行（而它必然崩）
        let d = tmp("pf-glibc-in-pkg");
        let pkg = write_elf_pkg(&d, "bad", &["libc.so.6"], &[("libc.so.6", &[])]);
        assert_eq!(bundled_glibc_family(&pkg), vec!["libc.so.6"], "包内 libc 必须被认出来");
        let pre = preflight(&pkg, &pkg.join("bin/x"));
        assert_eq!(pre.kind, PreflightKind::Risky, "{}", pre.detail);
        assert!(pre.detail.contains("GLIBC_PRIVATE"), "要说清为什么不行: {}", pre.detail);
    }

    #[test]
    fn wrapper_sets_terminfo_dirs_too() {
        // 全屏程序靠它找到终端条目；包内优先，后面接系统目录
        let app = Installed {
            manifest: Manifest {
                id: "htop".into(),
                name: "htop".into(),
                version: "1".into(),
                entry: "bin/htop".into(),
                args: vec![],
                desc: String::new(),
            },
            dir: PathBuf::from("/var/apps/htop"),
            bytes: 0,
        };
        let s = wrapper_script(&app);
        assert!(
            s.contains("TERMINFO_DIRS='/var/apps/htop/share/terminfo:/usr/share/terminfo"),
            "包装脚本要带上 terminfo 搜索路径:\n{s}"
        );
        assert!(s.contains("export TERMINFO_DIRS"), "必须 export 出去:\n{s}");
        assert!(!s.contains('\r') && !s.contains('\\'), "仍是给 Linux 跑的脚本:\n{s}");
    }
}
