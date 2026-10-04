//! 账户（§2.3）：**单用户 + 口令**的读写。
//!
//! ## 范围
//!
//! 项目自己的决定是「先做单用户 + 口令；多用户留 M3」。所以这里做两件事：
//! **看**（列出人类账户）与**改口令**。增删用户不在这里 —— 那是 M3 的事。
//!
//! ## 三条安全约定（改这个文件前请先读）
//!
//! 1. **口令只走 stdin**：`chpasswd` 的输入是我们直接写进它的标准输入的，
//!    既不进 argv（`ps` 里可见），也不经过 shell（没有注入面）；
//! 2. **口令不进审计**：调用方写审计时只记 `user`/`action`；
//!    另外 `tools::audit_args` 本身是**白名单制**、默认脱敏，是第二道保险；
//! 3. **旧口令的校验不在这里**：合成器有 `sha512crypt`（锁屏在用），aetherd 没有。
//!    也就是"先证明你是这个人，再让 aetherd 写新口令"。
//!
//! ## 为什么策略错误是"码"而不是句子
//!
//! 与 `DenyReason` / `PreviewError` / `DnsError` 同一套：这一层不该决定用户读到的措辞
//! （回执要按界面语言渲染，见 `server.rs` 的 `password_problem_text`）。

use aether_ipc::UserInfo;

// 口令策略**住在 `aether-ipc`**（与 `parse_dns_list` 同一个道理：界面与服务端共用一份）。
// 这里 re-export 而不是自己再写一份 —— 调用点照旧，但"规则从哪来"一目了然。
pub use aether_ipc::{check_new_password, is_valid_user_name, PasswordProblem};

/// 人类账户的 uid 起点（与 `useradd`/`adduser` 的默认一致）。
const FIRST_HUMAN_UID: u32 = 1000;
/// `nobody`/`nogroup` 的约定 uid。
///
/// 它 **>= 1000**，但不是人类账户 —— 只看"uid >= 1000"会把它列进账户页
/// （我第一版就是这么写的，被 `lists_only_human_users_and_skips_malformed_lines`
/// 当场抓到，因为那个样例里就有 `nobody` 那一行）。
const NOBODY_UID: u32 = 65534;

/// 从 `/etc/passwd` 文本里挑出**人类账户**（纯函数）。
///
/// 只看 uid 落在 `[1000, 65534)`：系统账户（root、daemon、sshd…）不该出现在"账户"页里，
/// 列出来只会让人误以为可以改它们的口令；而 `nobody`（65534）虽然 >= 1000，
/// 也是系统占位账户，同样要排除。
pub fn human_users(passwd: &str) -> Vec<UserInfo> {
    passwd
        .lines()
        .filter_map(parse_passwd_line)
        .filter(|u| (FIRST_HUMAN_UID..NOBODY_UID).contains(&u.uid))
        .collect()
}

/// 解析一行 `/etc/passwd`（7 个冒号分隔字段）。字段不够或 uid 不是数字就跳过。
fn parse_passwd_line(line: &str) -> Option<UserInfo> {
    let f: Vec<&str> = line.split(':').collect();
    if f.len() < 7 || f[0].is_empty() {
        return None;
    }
    let uid: u32 = f[2].parse().ok()?;
    Some(UserInfo {
        name: f[0].to_string(),
        uid,
        shell: f[6].to_string(),
    })
}

/// 读真实的 `/etc/passwd` 并列出人类账户。
pub fn list_users() -> Result<Vec<UserInfo>, String> {
    let text = std::fs::read_to_string("/etc/passwd")
        .map_err(|e| format!("读取 /etc/passwd 失败：{e}"))?;
    Ok(human_users(&text))
}

/// 写入新口令：经 `chpasswd` 的 **stdin**。
///
/// 为什么是 `chpasswd` 而不是自己改 `/etc/shadow`：shadow 的格式、锁定标记、
/// 到期字段都由系统工具维护，自己写容易写坏（而且 `sha512crypt` 只在合成器里有）。
/// 为什么用 stdin 而不是 `chpasswd user:pass`：命令行参数在 `ps` 里对同机用户可见。
pub fn set_password(user: &str, new_password: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !is_valid_user_name(user) {
        return Err(format!("用户名不合法：{user}"));
    }
    check_new_password(new_password).map_err(|p| format!("口令不合规（{}）", p.code()))?;

    let mut child = Command::new("chpasswd")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 chpasswd：{e}"))?;

    {
        let stdin = child.stdin.as_mut().ok_or("无法写入 chpasswd 的输入")?;
        // 直接写进子进程的标准输入：不经过 shell，因此没有注入面
        writeln!(stdin, "{user}:{new_password}").map_err(|e| format!("写入 chpasswd 失败：{e}"))?;
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("等待 chpasswd 失败：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        // 失败原因原样带出（chpasswd 会说明是哪个环节），但**不回显口令**
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() {
            format!("chpasswd 退出码 {:?}", out.status.code())
        } else {
            format!("chpasswd 失败：{err}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 `/etc/passwd` 的形状：系统账户在前，人类账户在后。
    const SAMPLE: &str = "\
root:x:0:0:root:/root:/bin/sh
daemon:x:1:1:daemon:/usr/sbin:/sbin/nologin
sshd:x:22:22:sshd:/var/run/sshd:/sbin/nologin
alice:x:1000:1000:Alice:/home/alice:/bin/sh
bob:x:1001:1001::/home/bob:/bin/sh
nobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin
malformed-line-without-fields
";

    #[test]
    fn lists_only_human_users_and_skips_malformed_lines() {
        let users = human_users(SAMPLE);
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        // 系统账户与 nobody 都不该出现；格式坏的行被跳过而不是 panic
        assert_eq!(
            names,
            vec!["alice", "bob"],
            "只应列出人类账户：系统账户、以及 uid=65534 的 nobody 都要排除"
        );
        assert_eq!(users[0].uid, 1000);
        assert_eq!(users[0].shell, "/bin/sh");
    }

    #[test]
    fn password_policy_rejects_the_four_dangerous_shapes() {
        // 空
        assert_eq!(check_new_password(""), Err(PasswordProblem::Empty));
        // 太短（7 位）
        assert_eq!(check_new_password("1234567"), Err(PasswordProblem::TooShort));
        // 刚好 8 位 → 通过
        assert!(check_new_password("12345678").is_ok());
        // 换行：会把 chpasswd 的一行拆成两行
        assert_eq!(
            check_new_password("abcd1234\nevil"),
            Err(PasswordProblem::ContainsNewline)
        );
        // 冒号：chpasswd 用第一个冒号分隔用户名与口令
        assert_eq!(
            check_new_password("abcd:1234"),
            Err(PasswordProblem::ContainsColon)
        );
        // 太长
        let long = "a".repeat(aether_ipc::MAX_PASSWORD_CHARS + 1);
        assert_eq!(check_new_password(&long), Err(PasswordProblem::TooLong));
        // **按字符数算**：8 个汉字应当通过（按字节会误判成 24 位）
        assert!(check_new_password("一二三四五六七八").is_ok());
    }

    #[test]
    fn user_name_whitelist_is_narrow_on_purpose() {
        for good in ["alice", "bob_2", "user-x", "_svc"] {
            assert!(is_valid_user_name(good), "{good} 应被接受");
        }
        for bad in ["", "Alice", "2alice", "-alice", "al ice", "a:b", "a/b", "用户"] {
            assert!(!is_valid_user_name(bad), "{bad} 应被拒绝");
        }
    }
}
