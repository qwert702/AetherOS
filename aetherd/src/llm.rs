//! LLM 客户端：OpenAI 兼容协议（GLM / Ollama /v1 均适用）+ 按通道路由。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一轮对话消息。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// 模型返回的工具调用（assistant 消息可能携带）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    /// 工具结果消息的调用 id（role=tool 时使用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 工具结果消息的名字（role=tool 时使用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// OpenAI 兼容端点配置。
#[derive(Clone)]
pub struct Endpoint {
    /// 例: http://127.0.0.1:11434/v1 （Ollama）或 https://open.bigmodel.cn/api/paas/v4 （GLM）
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// 请求一次补全。tools 传 OpenAI tools 数组；返回 assistant 消息。
pub fn complete(endpoint: &Endpoint, messages: &[Message], tools: Option<&Value>) -> Result<Message> {
    let url = format!("{}/chat/completions", endpoint.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({
        "model": endpoint.model,
        "messages": messages,
        "temperature": 0.4,
    });
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }

    let resp = match ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", endpoint.api_key))
        .timeout(std::time::Duration::from_secs(120))
        .send_json(body)
    {
        Ok(r) => r,
        // 关键：非 2xx 时**必须把服务器正文带出来**。
        // 旧实现在这里丢掉了正文，只剩 "status code 401"，而真正的原因
        // （Key 无效 / 模型名不存在 / 限流 / 网关 502）恰恰写在正文里 ——
        // "真实 LLM 协议不兼容"是本项目自评的头号架构风险，接入时不能没有这条线索。
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            anyhow::bail!(
                "LLM 请求失败: {url} → HTTP {code}；服务器返回: {}",
                body_snippet(&detail)
            );
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("LLM 请求失败: {url}"))),
    };
    let text = resp
        .into_string()
        .with_context(|| format!("读取 LLM 响应失败: {url}"))?;
    let v: Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "LLM 响应不是 JSON（可能被网关拦截）: {url}；正文开头: {}",
            body_snippet(&text)
        )
    })?;

    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .context("LLM 响应缺少 choices[0].message")?;
    Ok(Message {
        role: "assistant".into(),
        content: choice
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .into(),
        tool_calls: choice.get("tool_calls").cloned(),
        tool_call_id: None,
        name: None,
    })
}

/// 本地 Ollama 探活（/v1/models）。
pub fn local_available(base_url: &str) -> bool {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    ureq::get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .call()
        .is_ok()
}

/// 把服务器正文压成一行并限长，供报错信息使用。
///
/// 折叠换行 + 限长：错误页可能是多行 HTML，长度不可控；
/// 按**字符**而非字节截断，中文不会切在半个字符上。
fn body_snippet(body: &str) -> String {
    const MAX_CHARS: usize = 512;
    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_CHARS {
        return one_line;
    }
    let head: String = one_line.chars().take(MAX_CHARS).collect();
    format!("{head}…（正文已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// 读完一个 HTTP 请求（请求头 + Content-Length 正文）。
    ///
    /// 不读完就关连接，Windows 会回 RST —— 客户端拿到的是"连接被重置"而不是
    /// 我们写好的响应，测试会偶发失败。所以这里老老实实排空。
    fn drain_request(sock: &mut TcpStream) {
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        }
        let head_end = req
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
            .unwrap_or(req.len());
        let want: usize = String::from_utf8_lossy(&req[..head_end])
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                if k.eq_ignore_ascii_case("content-length") {
                    v.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let mut remaining = want.saturating_sub(req.len().saturating_sub(head_end));
        while remaining > 0 {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => remaining = remaining.saturating_sub(n),
            }
        }
    }

    /// 起一个只应答一次的本地假端点，返回它的 base_url（端口由系统分配）。
    fn spawn_stub(status_line: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let addr = listener.local_addr().expect("取本地端口");
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                drain_request(&mut sock);
                let resp = format!(
                    "{status_line}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        format!("http://{addr}/v1")
    }

    fn endpoint(base_url: String) -> Endpoint {
        Endpoint { base_url, api_key: "sk-test".into(), model: "test-model".into() }
    }

    fn user_msg(content: &str) -> Message {
        Message {
            role: "user".into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// 正常路径不能被这次改动破坏。
    #[test]
    fn successful_completion_parses_message() {
        let base = spawn_stub(
            "HTTP/1.1 200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"排成两列了"}}]}"#,
        );
        let msg = complete(&endpoint(base), &[user_msg("排成两列")], None).expect("应成功");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "排成两列了");
        assert!(msg.tool_calls.is_none());
    }

    /// 真端点报错时，正文才是唯一线索 —— 不能只剩一个状态码。
    #[test]
    fn http_error_keeps_server_body() {
        let base = spawn_stub(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"invalid api key"}}"#,
        );
        let err = complete(&endpoint(base), &[user_msg("hi")], None)
            .expect_err("401 应当报错");
        let msg = format!("{err:#}");
        assert!(msg.contains("401"), "应含状态码: {msg}");
        assert!(msg.contains("invalid api key"), "应带出服务器正文: {msg}");
    }

    /// 网关返回 HTML 错误页时，要能看出"不是 JSON"以及正文是什么。
    #[test]
    fn non_json_body_is_reported_with_snippet() {
        let base = spawn_stub(
            "HTTP/1.1 200 OK",
            "<html><body>502 Bad Gateway</body></html>",
        );
        let err = complete(&endpoint(base), &[user_msg("hi")], None)
            .expect_err("非 JSON 应当报错");
        let msg = format!("{err:#}");
        assert!(msg.contains("不是 JSON"), "应点明不是 JSON: {msg}");
        assert!(msg.contains("502 Bad Gateway"), "应带出正文片段: {msg}");
    }

    #[test]
    fn body_snippet_collapses_newlines_and_truncates() {
        assert_eq!(body_snippet("a\nb\r\n  c"), "a b c");
        let long = "中".repeat(600);
        let cut = body_snippet(&long);
        assert!(cut.ends_with("…（正文已截断）"), "超长应截断: {cut}");
        // 600 个中文字符按字节是 1800，若按字节截断会切出半个字符（变成 U+FFFD）
        assert!(!cut.contains('\u{FFFD}'), "截断必须落在字符边界上");
        assert_eq!(cut.chars().filter(|c| *c == '中').count(), 512);
    }

    #[test]
    fn body_snippet_keeps_short_body_intact() {
        assert_eq!(body_snippet("invalid api key"), "invalid api key");
        assert_eq!(body_snippet(""), "");
    }
}
