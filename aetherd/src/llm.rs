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

    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", endpoint.api_key))
        .timeout(std::time::Duration::from_secs(120))
        .send_json(body)
        .context(format!("LLM 请求失败: {url}"))?;
    let v: Value = resp.into_json()?;

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
