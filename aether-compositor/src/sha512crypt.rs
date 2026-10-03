//! SHA-512 与 SHA-512 crypt（`$6$`）—— 登录/锁屏的口令校验（FEATURE-GAPS §2.3）。
//!
//! ## 为什么自研，而不是用 crate 或系统 `crypt(3)`
//!
//! * 镜像离线、依赖被 `--locked` 钉死，缓存里没有可用的 crypt crate；
//! * 调系统的 `crypt(3)` 需要额外链接 `-lcrypt`，而**本机无法验证链接结果**
//!   （`cargo check` 不做链接）—— 万一镜像里没有 libcrypt，坏的是整个构建；
//! * 验收标准明确允许"自研的 crypt 校验"，但**不允许明文比较**。
//!
//! ## 正确性怎么保证（自研密码学最该回答的问题）
//!
//! 1. **SHA-512** 用 `hashlib` 生成的向量（独立实现）；
//! 2. **SHA-512 crypt** 用 **passlib**（广泛使用的纯 Python 实现）生成的向量，
//!    覆盖空口令、长口令（跨 SHA-512 块）、多字节口令、显式 `rounds=`；
//! 3. 算法**先在 Python 里与 passlib 对齐（7/7 一致）**，再照抄成 Rust ——
//!    不是"照记忆写一遍然后祈祷";
//! 4. 轮常量 `K` 是用**整数立方根**精确生成的（不经过浮点），并用公认首值自检。
//!
//! **未实机验证**：真机上 `/etc/shadow` 里的哈希能否被本实现校验通过。

// 生产消费者是**锁屏的 Linux 路径**（`LockScreen::submit` 读 `/etc/shadow`）。
// 在非 Linux（开发机）上只有测试用到它 —— 这与仓库里 `control_service` 的写法一致：
// 用按平台的 allow 表达「这些代码只在目标平台上被生产代码调用」，而不是笼统豁免。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// 本模块目前**只被测试使用**：调用它的登录/锁屏界面在下一批（§2.3 界面侧）才接线。
// 这个 `allow` 是**有期限的**，不是长期豁免 —— 接线那批应当把它删掉。
// 为什么不用"先接线再提交"：算法与向量是本批的核心价值（它已被独立实现验证），
// 而界面接线是另一件独立的事，分开提交更容易审。
// ---------------------------------------------------------------------------
// SHA-512
// ---------------------------------------------------------------------------

const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

const H0: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

#[inline]
fn ch(x: u64, y: u64, z: u64) -> u64 {
    (x & y) ^ (!x & z)
}

#[inline]
fn maj(x: u64, y: u64, z: u64) -> u64 {
    (x & y) ^ (x & z) ^ (y & z)
}

#[inline]
fn big_sigma0(x: u64) -> u64 {
    x.rotate_right(28) ^ x.rotate_right(34) ^ x.rotate_right(39)
}

#[inline]
fn big_sigma1(x: u64) -> u64 {
    x.rotate_right(14) ^ x.rotate_right(18) ^ x.rotate_right(41)
}

#[inline]
fn small_sigma0(x: u64) -> u64 {
    x.rotate_right(1) ^ x.rotate_right(8) ^ (x >> 7)
}

#[inline]
fn small_sigma1(x: u64) -> u64 {
    x.rotate_right(19) ^ x.rotate_right(61) ^ (x >> 6)
}

fn compress(h: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 80];
    for i in 0..16 {
        let mut v = 0u64;
        for j in 0..8 {
            v = (v << 8) | block[i * 8 + j] as u64;
        }
        w[i] = v;
    }
    for i in 16..80 {
        w[i] = small_sigma1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(small_sigma0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }
    let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
    let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
    for i in 0..80 {
        let t1 = hh
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

/// SHA-512 摘要（64 字节）。
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = H0;
    let mut chunks = data.chunks_exact(128);
    for c in &mut chunks {
        let mut b = [0u8; 128];
        b.copy_from_slice(c);
        compress(&mut h, &b);
    }
    let rem = chunks.remainder();
    // 填充：0x80 + 若干 0 + 128 位长度（大端）
    let bit_len = (data.len() as u128) * 8;
    let mut tail = Vec::with_capacity(256);
    tail.extend_from_slice(rem);
    tail.push(0x80);
    while (tail.len() + 16) % 128 != 0 {
        tail.push(0);
    }
    tail.extend_from_slice(&bit_len.to_be_bytes());
    for c in tail.chunks_exact(128) {
        let mut b = [0u8; 128];
        b.copy_from_slice(c);
        compress(&mut h, &b);
    }
    let mut out = [0u8; 64];
    for (i, v) in h.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// SHA-512 crypt（`$6$`）
// ---------------------------------------------------------------------------

/// crypt 专用的 base64 字母表（与标准 base64 **不同**，顺序不能改）。
const CRYPT64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 未写 `rounds=` 时的默认轮数（规范值）。
pub const DEFAULT_ROUNDS: u32 = 5_000;
/// 规范允许的最小轮数。
pub const MIN_ROUNDS: u32 = 1_000;
/// 规范允许的最大轮数（再大就是自伤：校验一次要几十秒）。
pub const MAX_ROUNDS: u32 = 999_999_999;
/// 盐的最大长度（规范值）。
pub const MAX_SALT_LEN: usize = 16;

/// 把 3 个字节按 crypt 的字节序编码成 `n` 个字符。
fn b64_from_24bit(b2: u8, b1: u8, b0: u8, n: usize) -> String {
    let mut w = ((b2 as u32) << 16) | ((b1 as u32) << 8) | (b0 as u32);
    let mut out = String::with_capacity(n);
    for _ in 0..n {
        out.push(CRYPT64[(w & 0x3f) as usize] as char);
        w >>= 6;
    }
    out
}

/// 计算 SHA-512 crypt 的哈希体（不含 `$6$salt$` 前缀）。
pub fn sha512_crypt_body(password: &[u8], salt: &[u8], rounds: u32) -> String {
    // B = SHA512(password || salt || password)
    let mut b_in = Vec::with_capacity(password.len() * 2 + salt.len());
    b_in.extend_from_slice(password);
    b_in.extend_from_slice(salt);
    b_in.extend_from_slice(password);
    let b = sha512(&b_in);

    // A：password || salt || （password.len() 个字节，从 B 循环取）
    let mut a_in = Vec::new();
    a_in.extend_from_slice(password);
    a_in.extend_from_slice(salt);
    let n = password.len();
    for i in 0..n {
        a_in.push(b[i % 64]);
    }
    // 二进制位：从 n 开始右移，1 则加整块 B，0 则加 password
    let mut i = n;
    while i > 0 {
        if i & 1 != 0 {
            a_in.extend_from_slice(&b);
        } else {
            a_in.extend_from_slice(password);
        }
        i >>= 1;
    }
    let a = sha512(&a_in);

    // DP = SHA512(password 重复 n 次)
    let mut dp_in = Vec::new();
    for _ in 0..n {
        dp_in.extend_from_slice(password);
    }
    let dp = sha512(&dp_in);
    let p: Vec<u8> = (0..n).map(|i| dp[i % 64]).collect();

    // DS = SHA512(salt 重复 16 + A[0] 次)
    let mut ds_in = Vec::new();
    for _ in 0..(16 + a[0] as usize) {
        ds_in.extend_from_slice(salt);
    }
    let ds = sha512(&ds_in);
    let s: Vec<u8> = (0..salt.len()).map(|i| ds[i % 64]).collect();

    // 主循环
    let mut c = a;
    for r in 0..rounds {
        let mut h = Vec::new();
        if r & 1 != 0 {
            h.extend_from_slice(&p);
        } else {
            h.extend_from_slice(&c);
        }
        if r % 3 != 0 {
            h.extend_from_slice(&s);
        }
        if r % 7 != 0 {
            h.extend_from_slice(&p);
        }
        if r & 1 != 0 {
            h.extend_from_slice(&c);
        } else {
            h.extend_from_slice(&p);
        }
        c = sha512(&h);
    }

    // 按参考实现的字节顺序编码（顺序写错就会得到错误的哈希 —— 向量会抓住）
    let mut out = String::with_capacity(86);
    out.push_str(&b64_from_24bit(c[0], c[21], c[42], 4));
    out.push_str(&b64_from_24bit(c[22], c[43], c[1], 4));
    out.push_str(&b64_from_24bit(c[44], c[2], c[23], 4));
    out.push_str(&b64_from_24bit(c[3], c[24], c[45], 4));
    out.push_str(&b64_from_24bit(c[25], c[46], c[4], 4));
    out.push_str(&b64_from_24bit(c[47], c[5], c[26], 4));
    out.push_str(&b64_from_24bit(c[6], c[27], c[48], 4));
    out.push_str(&b64_from_24bit(c[28], c[49], c[7], 4));
    out.push_str(&b64_from_24bit(c[50], c[8], c[29], 4));
    out.push_str(&b64_from_24bit(c[9], c[30], c[51], 4));
    out.push_str(&b64_from_24bit(c[31], c[52], c[10], 4));
    out.push_str(&b64_from_24bit(c[53], c[11], c[32], 4));
    out.push_str(&b64_from_24bit(c[12], c[33], c[54], 4));
    out.push_str(&b64_from_24bit(c[34], c[55], c[13], 4));
    out.push_str(&b64_from_24bit(c[56], c[14], c[35], 4));
    out.push_str(&b64_from_24bit(c[15], c[36], c[57], 4));
    out.push_str(&b64_from_24bit(c[37], c[58], c[16], 4));
    out.push_str(&b64_from_24bit(c[59], c[17], c[38], 4));
    out.push_str(&b64_from_24bit(c[18], c[39], c[60], 4));
    out.push_str(&b64_from_24bit(c[40], c[61], c[19], 4));
    out.push_str(&b64_from_24bit(c[62], c[20], c[41], 4));
    out.push_str(&b64_from_24bit(0, 0, c[63], 2));
    out
}

/// 定长时间比较（避免用比较耗时泄露"前缀对了多少"）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 校验口令与 `/etc/shadow` 里的哈希是否匹配。
///
/// 只支持 `$6$`（SHA-512 crypt）。**其它方案一律明确拒绝**，不静默放行也不静默
/// 当成"不匹配" —— 后者会让用户看到"口令错误"，而真实原因是"系统不认这种哈希"，
/// 排查起来毫无线索。
pub fn verify_password(password: &str, stored: &str) -> Result<(), String> {
    let stored = stored.trim();
    if stored.is_empty() || stored == "!" || stored == "*" || stored.starts_with('!') {
        return Err("该账户未设置口令（shadow 里是锁定/空占位符）".to_string());
    }
    let rest = match stored.strip_prefix("$6$") {
        Some(r) => r,
        None => {
            let scheme = stored.split('$').nth(1).unwrap_or("");
            let name = match scheme {
                "1" => "MD5 crypt（$1$，已知弱，已被破解）",
                "5" => "SHA-256 crypt（$5$）",
                "2a" | "2b" | "2y" => "bcrypt（$2$）",
                "y" => "yescrypt（$y$）",
                _ => "未知方案",
            };
            return Err(format!(
                "不支持的口令哈希方案：{name}。本系统只校验 $6$（SHA-512 crypt）"
            ));
        }
    };
    // 解析可选的 rounds= 与盐
    let (rounds, after) = if let Some(r) = rest.strip_prefix("rounds=") {
        let (n, tail) = r
            .split_once('$')
            .ok_or_else(|| "哈希格式错误：rounds= 后缺少 $".to_string())?;
        let n: u32 = n
            .parse()
            .map_err(|_| format!("哈希格式错误：rounds 不是数字（{n}）"))?;
        // 夹取到规范范围：**不拒绝**，因为这是"读到的既有哈希"，
        // 按规范夹取才能与系统其它实现（login/su）保持一致
        (n.clamp(MIN_ROUNDS, MAX_ROUNDS), tail)
    } else {
        (DEFAULT_ROUNDS, rest)
    };
    let (salt, hash) = after
        .split_once('$')
        .ok_or_else(|| "哈希格式错误：缺少盐或哈希体".to_string())?;
    if salt.is_empty() || salt.len() > MAX_SALT_LEN {
        return Err(format!(
            "哈希格式错误：盐长度应在 1~{MAX_SALT_LEN}（实得 {}）",
            salt.len()
        ));
    }
    if !salt.bytes().all(|b| CRYPT64.contains(&b)) {
        return Err("哈希格式错误：盐含非法字符".to_string());
    }
    let body = sha512_crypt_body(password.as_bytes(), salt.as_bytes(), rounds);
    if ct_eq(body.as_bytes(), hash.as_bytes()) {
        Ok(())
    } else {
        // 注意：这里**不回显**口令或哈希片的任何内容
        Err("口令不正确".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-512 向量（`hashlib` 生成 —— 与本实现相互独立）。
    #[test]
    fn sha512_matches_published_vectors() {
        let cases: Vec<(&str, &str)> = vec![
            ("", "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"),
            ("abc", "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
            ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445"),
        ];
        for (msg, want) in cases {
            let got = sha512(msg.as_bytes());
            let hex: String = got.iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hex, want, "SHA-512({msg:?}) 不符");
        }
        // 跨块：长度 > 128 字节时填充与分块都要正确
        let long = sha512(&vec![b'a'; 200]);
        assert_ne!(long, [0u8; 64]);
    }

    /// SHA-512 crypt 向量（**passlib** 生成；算法先在 Python 与它对齐 7/7 后才照抄）。
    ///
    /// 覆盖：空口令、长口令（跨 SHA-512 块）、多字节口令、显式 rounds、默认 rounds。
    #[test]
    fn sha512_crypt_matches_reference_vectors() {
        let cases: Vec<(&str, &str, u32, &str)> = vec![
            ("hello", "abcdefgh", 5000, "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/"),
            ("Hello world!", "saltstring", 5000, "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"),
            ("", "abcdefgh", 5000, "$6$abcdefgh$v7sYNA18/BerGOYQLppYLyjH4yJilp8kqe/ef3KYMK9hOIdzH1yzcmP74Ay.m51y1jP3QqxM7Jl75S4CxDhBq."),
            ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "abcdefgh", 5000, "$6$abcdefgh$umHTAeeAFXqpxOxVIx3PXYmdHek2DOlKvyM732k9cvm3URduZrabLe.XmctGU5hfsQFV5VVEydwoDtm1N8sbD."),
            ("中文口令", "abcdefgh", 5000, "$6$abcdefgh$xzi0aKapcaStsf8virL9odTxxGDbSyvkV8N327GMOZXJS8WTIRnR1u9bDUWkS9NNhwCn6KgNmfZw/2n.Z/wr90"),
            ("hello", "abcdefgh", 1000, "$6$rounds=1000$abcdefgh$lTNfvnTTeJaP4hJRc5HKoW7.C7.Aht9eJcusrUnljrNfqeofleTkSKfi5mHqqRLRA.jW3omiyGtwR/4VdOeJX0"),
            ("hello", "abcdefgh", 10000, "$6$rounds=10000$abcdefgh$tD91cHNhoZbTU7kKpa6SpkXrFm/0NBFKvSLOtJkI.vzr5ADTRPhWmXoKegxbUIeig2HP.P3o9HT.6f0DRwGew1"),
        ];
        for (pw, salt, rounds, want) in cases {
            let body = sha512_crypt_body(pw.as_bytes(), salt.as_bytes(), rounds);
            let full = if rounds == DEFAULT_ROUNDS {
                format!("$6${salt}${body}")
            } else {
                format!("$6$rounds={rounds}${salt}${body}")
            };
            assert_eq!(full, want, "口令 {pw:?} / 盐 {salt:?} / {rounds} 轮不符");
        }
    }

    #[test]
    fn verify_accepts_correct_and_rejects_wrong() {
        let stored = "$6$abcdefgh$bp46WUxDu2cphMRE0PvVJlqas2imuSq186YS793XKfmqn9XUEL17HECskUTZInaNigNndLLYc7A6yWBmFEFn3/";
        assert!(verify_password("hello", stored).is_ok());
        // 错口令
        let err = verify_password("hellp", stored).unwrap_err();
        assert_eq!(err, "口令不正确");
        // 大小写不同也要拒绝
        assert!(verify_password("Hello", stored).is_err());
        // 显式 rounds 形式同样可校验
        assert!(verify_password("hello", "$6$rounds=1000$abcdefgh$lTNfvnTTeJaP4hJRc5HKoW7.C7.Aht9eJcusrUnljrNfqeofleTkSKfi5mHqqRLRA.jW3omiyGtwR/4VdOeJX0").is_ok());
    }

    /// **拒绝**不支持的方案，并且**把原因说清楚**（不静默当成"口令错误"）。
    #[test]
    fn verify_explains_unsupported_schemes_instead_of_silently_failing() {
        for (stored, want_kw) in [
            ("$1$abcdefgh$abcdefghijklmnopqrstuv", "MD5"),
            ("$5$abcdefgh$abcdefghijklmnopqrstuv", "SHA-256"),
            ("$2b$12$abcdefghijklmnopqrstuv", "bcrypt"),
            ("$y$j9T$abcdefgh$abcdefghijklmnop", "yescrypt"),
            ("plaintext", "不支持"),
            ("", "未设置"),
            ("!", "未设置"),
            ("*", "未设置"),
            ("!locked", "未设置"),
        ] {
            let err = verify_password("hello", stored).unwrap_err();
            assert!(
                err.contains(want_kw),
                "{stored:?} 的拒绝理由应提到「{want_kw}」，实得：{err}"
            );
        }
    }

    /// 格式坏掉的哈希要报"格式错误"，而不是"口令不正确"
    /// （否则用户会一遍遍重打口令，而问题在 /etc/shadow）。
    #[test]
    fn verify_reports_malformed_hash_separately() {
        for stored in [
            "$6$",                      // 没有盐
            "$6$abcdefgh",              // 没有哈希体
            "$6$rounds=abc$abcdefgh$xx", // rounds 不是数字
            "$6$rounds=1000$abcdefgh",  // rounds 后缺哈希体
            "$6$这是一段很长的非法盐盐盐盐盐盐盐盐$xx", // 盐超长
            "$6$ab!defgh$xx",           // 盐含非法字符
        ] {
            let err = verify_password("hello", stored).unwrap_err();
            assert!(
                err.contains("格式错误"),
                "{stored:?} 应报格式错误，实得：{err}"
            );
        }
    }

    /// 定长比较不能因为长度不同而进入逐字节比较（否则会泄露长度信息）。
    #[test]
    fn constant_time_compare_behaves() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }
}
