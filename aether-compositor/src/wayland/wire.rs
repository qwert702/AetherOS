//! Wayland 线协议编解码（3.1 spike 的第一块）。
//!
//! 消息格式（**全部小端**）：
//!
//! ```text
//! ┌──────────────┬────────────────────────────┐
//! │ object_id u32│  size u16  │  opcode u16   │   ← 8 字节头
//! ├──────────────┴────────────────────────────┤
//! │ 参数（按签名依次打包，整体 4 字节对齐）        │
//! └───────────────────────────────────────────┘
//! ```
//!
//! 注意 `size` 在头部里是**高 16 位**（`size<<16 | opcode`），这是 Wayland 为了
//! 让头和第一个参数共用一个 32 位字而做的打包 —— 不是笔误。
//!
//! **签名决定参数布局**：消息体里没有类型信息，接收方必须知道该
//! (interface, opcode) 对应的签名才能解析。所以 `decode` 要求传入签名。
//!
//! 为什么这块先做：它是**纯逻辑**，不碰 socket、不碰 fd，可以在开发机上完整单测。
//! Wayland 正确性的绝大部分都在这里 —— 参数打包错一个字节，后面全乱。

use std::fmt;

/// 定点数（24.8）的缩放因子。
const FIXED_SCALE: f32 = 256.0;

/// 参数值。
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i32),
    Uint(u32),
    /// 24.8 定点（Wayland 的 `f` 类型）
    Fixed(f32),
    /// 字符串（不含结尾 NUL —— 线格式里有，这里剥掉了）
    Str(String),
    /// 对象引用（`o`）
    Object(u32),
    /// 新对象 id（`n`）
    NewId(u32),
    Array(Vec<u8>),
    /// 文件描述符。**不编码进消息体** —— 经 `SCM_RIGHTS` 随 `sendmsg` 传。
    /// 放在 `Arg` 里是为了让签名解析的**位置**对得上（fd 在参数序列里占一个槽）。
    Fd(i32),
}

/// 一条 Wayland 消息。
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub object_id: u32,
    pub opcode: u16,
    pub args: Vec<Arg>,
}

/// 编解码错误。
#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    /// 头部不完整
    ShortHeader,
    /// 声明的长度小于头部、或超过缓冲区
    BadSize(usize),
    /// 参数区在解析中途用尽
    Truncated,
    /// 签名与消息体不匹配（长度对不上）
    SignatureMismatch { signature: String, at: usize },
    /// 未知的签名类型字符
    UnknownType(char),
    /// 字符串缺少结尾 NUL
    UnterminatedString,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::ShortHeader => write!(f, "消息头不足 8 字节"),
            WireError::BadSize(n) => write!(f, "非法的消息长度 {n}"),
            WireError::Truncated => write!(f, "参数区提前用尽"),
            WireError::SignatureMismatch { signature, at } => {
                write!(f, "签名 {signature:?} 在第 {at} 个参数处与消息体不匹配")
            }
            WireError::UnknownType(c) => write!(f, "未知的签名类型字符 {c:?}"),
            WireError::UnterminatedString => write!(f, "字符串缺少结尾 NUL"),
        }
    }
}

impl std::error::Error for WireError {}

/// 4 字节向上对齐。
#[inline]
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// 把一个参数写入 `out`（fd 不写 —— 它走 `SCM_RIGHTS`）。
fn put_arg(out: &mut Vec<u8>, arg: &Arg) {
    match arg {
        Arg::Int(v) => out.extend_from_slice(&v.to_le_bytes()),
        Arg::Uint(v) | Arg::Object(v) | Arg::NewId(v) => out.extend_from_slice(&v.to_le_bytes()),
        Arg::Fixed(v) => {
            let raw = (v * FIXED_SCALE).round() as i32;
            out.extend_from_slice(&raw.to_le_bytes());
        }
        Arg::Str(s) => {
            // 线格式：u32 长度（**含**结尾 NUL）+ 内容 + NUL + padding
            let with_nul = s.len() + 1;
            out.extend_from_slice(&(with_nul as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
            out.push(0);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        Arg::Array(bytes) => {
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        Arg::Fd(_) => {}
    }
}

impl Message {
    /// 按签名打包（含 8 字节头）。
    ///
    /// `signature` 只用于**校验参数个数**；实际布局由 `args` 里各值的类型决定。
    pub fn encode(&self, signature: &str) -> Vec<u8> {
        let mut body = Vec::new();
        for a in &self.args {
            put_arg(&mut body, a);
        }
        let size = 8 + body.len();
        debug_assert_eq!(size % 4, 0, "Wayland 消息必须 4 字节对齐");
        debug_assert_eq!(
            signature.chars().count(),
            self.args.len(),
            "签名 {signature:?} 与参数个数 {} 不符",
            self.args.len()
        );

        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(&self.object_id.to_le_bytes());
        out.extend_from_slice(&((size as u32) << 16 | self.opcode as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }
}

/// 缓冲区里"完整消息"的总长度（跨读重组用）。
///
/// 为什么需要它（2026-10-02 审计 L-16）：读缓冲是 16KB，而单条 wl 消息的 `size`
/// 是 16 位、以字节计（上限约 64KB）。此前服务端每读到多少就把整块交给
/// `Session::handle`，于是**一条大消息被 TCP 分段就必然解析失败**，连接被判协议错误
/// 后断开 —— 表现为"客户端随机连不上"，且只在传大消息时出现。
///
/// 按消息头的 `size` 逐条推进，返回完整前缀的长度；不完整的尾巴留在缓冲里等下一次读。
/// 头部坏掉（size < 8 或非 4 的倍数）时停止推进，把这一块留给 `decode` 去报协议错误。
pub fn complete_prefix_len(buf: &[u8]) -> usize {
    let mut off = 0;
    while off + 8 <= buf.len() {
        let word = u32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
        let size = (word >> 16) as usize;
        if size < 8 || size % 4 != 0 || off + size > buf.len() {
            break;
        }
        off += size;
    }
    off
}

/// 从字节流头部读出 (object_id, opcode, size)。不校验 size 是否越界。
pub fn peek_header(buf: &[u8]) -> Result<(u32, u16, usize), WireError> {    if buf.len() < 8 {
        return Err(WireError::ShortHeader);
    }
    let object_id = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let word = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let size = (word >> 16) as usize;
    let opcode = (word & 0xffff) as u16;
    Ok((object_id, opcode, size))
}

/// 解析一条消息。返回 `(消息, 消耗的字节数)`，便于在一个缓冲区里连续解析多条。
pub fn decode(buf: &[u8], signature: &str) -> Result<(Message, usize), WireError> {
    let (object_id, opcode, size) = peek_header(buf)?;
    if size < 8 || size % 4 != 0 {
        return Err(WireError::BadSize(size));
    }
    if buf.len() < size {
        return Err(WireError::Truncated);
    }
    let body = &buf[8..size];

    let mut pos = 0usize;
    let mut args = Vec::new();
    for (idx, ty) in signature.chars().enumerate() {
        // 用宏而不是闭包：闭包会**不可变借用** `pos`，与后面每个分支的
        // `pos += 4` 冲突（借用一直活到闭包最后一次使用）。
        // 展开成**表达式**（返回 Result）而不是直接 return —— 这样调用处的 `?`
        // 让分号成为必需的，避免 `unnecessary_trailing_semicolon`。
        macro_rules! need {
            ($n:expr) => {
                if pos + $n > body.len() {
                    Err(WireError::SignatureMismatch {
                        signature: signature.to_string(),
                        at: idx,
                    })
                } else {
                    Ok(())
                }
            };
        }
        match ty {
            'i' => {
                need!(4)?;
                let v = i32::from_le_bytes(body[pos..pos + 4].try_into().unwrap());
                pos += 4;
                args.push(Arg::Int(v));
            }
            'u' => {
                need!(4)?;
                let v = u32::from_le_bytes(body[pos..pos + 4].try_into().unwrap());
                pos += 4;
                args.push(Arg::Uint(v));
            }
            'o' => {
                need!(4)?;
                let v = u32::from_le_bytes(body[pos..pos + 4].try_into().unwrap());
                pos += 4;
                args.push(Arg::Object(v));
            }
            'n' => {
                need!(4)?;
                let v = u32::from_le_bytes(body[pos..pos + 4].try_into().unwrap());
                pos += 4;
                args.push(Arg::NewId(v));
            }
            'f' => {
                need!(4)?;
                let raw = i32::from_le_bytes(body[pos..pos + 4].try_into().unwrap());
                pos += 4;
                args.push(Arg::Fixed(raw as f32 / FIXED_SCALE));
            }
            's' => {
                need!(4)?;
                let len = u32::from_le_bytes(body[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                if len == 0 {
                    // 长度 0 是合法的"空字符串"（协议允许）
                    args.push(Arg::Str(String::new()));
                    continue;
                }
                need!(len)?;
                let raw = &body[pos..pos + len];
                if *raw.last().unwrap() != 0 {
                    return Err(WireError::UnterminatedString);
                }
                let s = String::from_utf8_lossy(&raw[..len - 1]).into_owned();
                pos += align4(len);
                args.push(Arg::Str(s));
            }
            'a' => {
                need!(4)?;
                let len = u32::from_le_bytes(body[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                need!(len)?;
                args.push(Arg::Array(body[pos..pos + len].to_vec()));
                pos += align4(len);
            }
            // fd 不占消息体：槽位保留，值由 SCM_RIGHTS 侧填（此处记 -1 占位）
            'h' => args.push(Arg::Fd(-1)),
            other => return Err(WireError::UnknownType(other)),
        }
    }

    // 参数用完必须恰好落在消息末尾 —— 多出来的字节说明签名不对
    if pos != body.len() {
        return Err(WireError::SignatureMismatch {
            signature: signature.to_string(),
            at: signature.chars().count(),
        });
    }
    Ok((Message { object_id, opcode, args }, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_packing_is_size_high_opcode_low() {
        // 这是最容易写反的一处：size 在高 16 位
        let m = Message { object_id: 7, opcode: 3, args: vec![] };
        let bytes = m.encode("");
        assert_eq!(bytes.len(), 8);
        let (id, op, size) = peek_header(&bytes).unwrap();
        assert_eq!((id, op, size), (7, 3, 8));
    }

    /// L-16 回归：跨读重组 —— 大消息被切开时必须等齐再解析，而不是报协议错误。
    #[test]
    fn complete_prefix_len_waits_for_whole_messages() {
        // 两条消息：一条 8 字节（空参数），一条带 8 字节参数 → 16 字节
        let a = Message { object_id: 2, opcode: 0, args: vec![] }.encode("");
        let b = Message { object_id: 2, opcode: 1, args: vec![Arg::Uint(1), Arg::Int(2)] }
            .encode("ui");
        assert_eq!(a.len(), 8);
        assert_eq!(b.len(), 16);

        // 完整的两条 → 全部可解析
        let both: Vec<u8> = [a.clone(), b.clone()].concat();
        assert_eq!(complete_prefix_len(&both), 24);

        // 只读到了第一条 + 第二条的前半 → 只能解析第一条
        let partial: Vec<u8> = [a.clone(), b[..6].to_vec()].concat();
        assert_eq!(complete_prefix_len(&partial), 8, "半条消息不能算完整");

        // 只读到第一条的一部分 → 一条都不完整
        assert_eq!(complete_prefix_len(&a[..5]), 0);

        // 头部坏掉（size 非 4 的倍数）→ 停在坏消息处，交给 decode 报错
        let mut bad = a.clone();
        bad[4..8].copy_from_slice(&((9u32) << 16).to_le_bytes()); // size=9
        assert_eq!(complete_prefix_len(&bad), 0);

        // 空缓冲 / 不足 8 字节
        assert_eq!(complete_prefix_len(&[]), 0);
        assert_eq!(complete_prefix_len(&[0u8; 7]), 0);
    }

    #[test]
    fn roundtrip_ints_and_objects() {
        let m = Message {
            object_id: 2,
            opcode: 1,
            args: vec![Arg::Uint(1), Arg::Int(-5), Arg::Object(0xdead_beef), Arg::NewId(9)],
        };
        let bytes = m.encode("uion");
        let (back, used) = decode(&bytes, "uion").unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(back, m);
    }

    #[test]
    fn roundtrip_string_including_padding() {
        // 长度 3 的字符串 → 线格式 4 字节（含 NUL），正好对齐；用 4 字节触发 padding
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Str("abcd".into())] };
        let bytes = m.encode("s");
        assert_eq!(bytes.len(), 8 + 4 + 8, "5 字节（含 NUL）应补齐到 8");
        let (back, _) = decode(&bytes, "s").unwrap();
        assert_eq!(back.args[0], Arg::Str("abcd".into()));
    }

    #[test]
    fn roundtrip_multibyte_string() {
        // 中文按 **字节** 计长，不是字符 —— 按字符算会错位
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Str("中文".into())] };
        let bytes = m.encode("s");
        let (back, _) = decode(&bytes, "s").unwrap();
        assert_eq!(back.args[0], Arg::Str("中文".into()));
    }

    #[test]
    fn empty_string_is_legal() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Str(String::new())] };
        let bytes = m.encode("s");
        let (back, _) = decode(&bytes, "s").unwrap();
        assert_eq!(back.args[0], Arg::Str(String::new()));
    }

    #[test]
    fn roundtrip_fixed_point() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Fixed(1.5), Arg::Fixed(-0.25)] };
        let bytes = m.encode("ff");
        let (back, _) = decode(&bytes, "ff").unwrap();
        assert_eq!(back.args[0], Arg::Fixed(1.5));
        assert_eq!(back.args[1], Arg::Fixed(-0.25));
    }

    #[test]
    fn roundtrip_array_with_padding() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Array(vec![1, 2, 3])] };
        let bytes = m.encode("a");
        let (back, _) = decode(&bytes, "a").unwrap();
        assert_eq!(back.args[0], Arg::Array(vec![1, 2, 3]));
    }

    #[test]
    fn multiple_messages_in_one_buffer() {
        let a = Message { object_id: 1, opcode: 0, args: vec![Arg::Uint(1)] };
        let b = Message { object_id: 2, opcode: 5, args: vec![Arg::Uint(2)] };
        let mut buf = a.encode("u");
        buf.extend_from_slice(&b.encode("u"));

        let (m1, n1) = decode(&buf, "u").unwrap();
        let (m2, n2) = decode(&buf[n1..], "u").unwrap();
        assert_eq!(m1, a);
        assert_eq!(m2, b);
        assert_eq!(n1 + n2, buf.len());
    }

    #[test]
    fn short_header_is_rejected() {
        assert_eq!(peek_header(&[1, 2, 3]), Err(WireError::ShortHeader));
    }

    #[test]
    fn size_smaller_than_header_is_rejected() {
        let mut bytes = Message { object_id: 1, opcode: 0, args: vec![] }.encode("");
        bytes[4..8].copy_from_slice(&((4u32) << 16).to_le_bytes());
        assert_eq!(decode(&bytes, ""), Err(WireError::BadSize(4)));
    }

    #[test]
    fn size_beyond_buffer_is_truncated() {
        let bytes = Message { object_id: 1, opcode: 0, args: vec![Arg::Uint(1)] }.encode("u");
        assert_eq!(decode(&bytes[..10], "u"), Err(WireError::Truncated));
    }

    /// 签名与消息体长度不符时必须报错，而不是解析出垃圾。
    #[test]
    fn signature_mismatch_is_detected() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Uint(1)] };
        let bytes = m.encode("u");
        // 用 "uu" 解析：第二个 u 没有数据
        assert!(matches!(
            decode(&bytes, "uu"),
            Err(WireError::SignatureMismatch { .. })
        ));
        // 用 "" 解析：还剩 4 字节没被消费
        assert!(matches!(
            decode(&bytes, ""),
            Err(WireError::SignatureMismatch { .. })
        ));
    }

    #[test]
    fn unknown_signature_char_is_rejected() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Uint(1)] };
        let bytes = m.encode("u");
        assert_eq!(decode(&bytes, "z"), Err(WireError::UnknownType('z')));
    }

    #[test]
    fn unterminated_string_is_rejected() {
        // 手工构造：长度 4 但最后一位不是 NUL
        let mut body = Vec::new();
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(b"abcd");
        let size = 8 + body.len();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&((size as u32) << 16).to_le_bytes());
        bytes.extend_from_slice(&body);
        assert_eq!(decode(&bytes, "s"), Err(WireError::UnterminatedString));
    }

    /// fd 不占消息体，但要在参数序列里占一个**位置** —— 否则后面的参数全错位。
    #[test]
    fn fd_occupies_a_slot_but_no_bytes() {
        let m = Message { object_id: 1, opcode: 0, args: vec![Arg::Fd(7), Arg::Uint(42)] };
        let bytes = m.encode("hu");
        assert_eq!(bytes.len(), 8 + 4, "fd 不该写进消息体");
        let (back, _) = decode(&bytes, "hu").unwrap();
        assert_eq!(back.args[1], Arg::Uint(42), "fd 之后的参数位置必须正确");
    }

    #[test]
    fn align4_rounds_up() {
        assert_eq!(align4(0), 0);
        assert_eq!(align4(1), 4);
        assert_eq!(align4(4), 4);
        assert_eq!(align4(5), 8);
    }
}
