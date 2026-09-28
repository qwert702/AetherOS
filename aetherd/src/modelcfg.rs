//! 模型配置的持久化（0.6 首启配置）。
//!
//! 在此之前，aetherd 的模型端点**只能靠环境变量**配置 —— 意味着全新 ISO 首启后
//! 用户没有任何配置入口（除非自己写 init 脚本）。这个模块补上持久化：
//!
//! **优先级：环境变量 > 配置文件 > 内置默认。**
//! 环境变量优先是因为它更适合"临时覆盖"（调试、CI），而配置文件适合
//! "装好就不再动"。两者都存在时，显式的环境变量意图更强。
//!
//! 配置路径：Linux `/etc/aether/model.json`，开发机 `%TEMP%/aether-model.json`。
//! 写盘时在 Unix 下收紧到 **0600** —— 文件里有 API Key。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn is_false(b: &bool) -> bool {
    !*b
}

/// 落盘的模型配置。全部字段可选 —— 缺省表示"用默认值"。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_model: Option<String>,
    /// 云端 API Key。
    ///
    /// **明文存在 0600 文件里** —— 这是与 `ui.key` 同级别的诚实边界：
    /// 它挡不住"能读该文件的进程"，但把"谁能拿到 Key"从"任何本机进程"
    /// 收窄到"root"。真正的密钥管理（keyring / TPM）不在当前阶段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub local_only: bool,
}

/// 配置文件路径。
pub fn config_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/etc/aether/model.json")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::temp_dir().join("aether-model.json")
    }
}

impl ModelConfig {
    /// 从默认路径读；文件不存在或损坏都返回默认值（不报错）。
    ///
    /// 损坏时返回默认而不是报错，是因为**配置坏掉不该让 aetherd 起不来** ——
    /// 那会连带桌面一起挂掉。坏配置的表现应该是"AI 不可用"，而不是"系统不可用"。
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path();
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建目录 {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("无法写入 {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 含 API Key：与 ui.key 同样收紧到 0600
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// 是否已有可用的模型配置。
    ///
    /// 判据是"至少配了一个端点"：本地 URL 或云端 Key。只配了模型名不算 ——
    /// 那是给默认端点换名字，不是新增能力。
    pub fn is_configured(&self) -> bool {
        self.local_url.is_some() || self.api_key.is_some()
    }

    /// 脱敏摘要：**绝不回传 API Key 本身**，只说"配没配 + 末尾 4 位"。
    ///
    /// 这个摘要会经 IPC 回到合成器（可能显示在界面上），所以不能带 Key。
    pub fn summary(&self) -> serde_json::Value {
        let key_hint = match self.api_key.as_deref() {
            Some(k) if k.len() > 4 => format!("****{}", &k[k.len() - 4..]),
            Some(_) => "****".to_string(),
            None => "(未配置)".to_string(),
        };
        serde_json::json!({
            "configured": self.is_configured(),
            "local_url": self.local_url.as_deref().unwrap_or("(默认 http://127.0.0.1:11434/v1)"),
            "local_model": self.local_model.as_deref().unwrap_or("(默认 qwen2.5:7b)"),
            "cloud_base": self.cloud_base.as_deref().unwrap_or("(默认 GLM)"),
            "cloud_model": self.cloud_model.as_deref().unwrap_or("(默认 glm-4-flash)"),
            "api_key": key_hint,
            "local_only": self.local_only,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aether_modelcfg_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        d
    }

    #[test]
    fn save_then_load_roundtrip() {
        let d = tmp("roundtrip");
        let p = d.join("model.json");
        let cfg = ModelConfig {
            local_url: Some("http://127.0.0.1:11434/v1".into()),
            local_model: Some("qwen2.5:7b".into()),
            api_key: Some("sk-test-123456".into()),
            cloud_base: Some("https://example.com/v1".into()),
            ..Default::default()
        };
        cfg.save_to(&p).expect("应能写盘");

        let back = ModelConfig::load_from(&p);
        assert_eq!(back.local_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
        assert_eq!(back.api_key.as_deref(), Some("sk-test-123456"));
        assert_eq!(back.cloud_base.as_deref(), Some("https://example.com/v1"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 配置坏掉不该让 aetherd 起不来 —— 表现为"AI 不可用"，而不是"系统不可用"。
    #[test]
    fn corrupt_file_falls_back_to_default() {
        let d = tmp("corrupt");
        let p = d.join("model.json");
        std::fs::write(&p, b"{ this is not json").unwrap();
        let cfg = ModelConfig::load_from(&p);
        assert!(!cfg.is_configured());
        assert!(cfg.api_key.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_falls_back_to_default() {
        let d = tmp("missing");
        let cfg = ModelConfig::load_from(&d.join("nope.json"));
        assert!(!cfg.is_configured());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn configured_requires_an_endpoint() {
        // 只配模型名不算"已配置" —— 那是给默认端点换名字，不是新增能力
        let only_model = ModelConfig { local_model: Some("x".into()), ..Default::default() };
        assert!(!only_model.is_configured());

        let with_local = ModelConfig { local_url: Some("http://x/v1".into()), ..Default::default() };
        assert!(with_local.is_configured());

        let with_key = ModelConfig { api_key: Some("sk-x".into()), ..Default::default() };
        assert!(with_key.is_configured());
    }

    /// 摘要会经 IPC 回到合成器（可能上屏），**不能带 Key**。
    #[test]
    fn summary_never_leaks_api_key() {
        let cfg = ModelConfig {
            api_key: Some("sk-super-secret-value".into()),
            ..Default::default()
        };
        let s = cfg.summary().to_string();
        assert!(!s.contains("sk-super-secret-value"), "摘要泄露了完整 Key: {s}");
        assert!(s.contains("alue"), "应保留末 4 位作为提示: {s}");
    }

    #[test]
    fn summary_handles_short_and_absent_key() {
        let short = ModelConfig { api_key: Some("ab".into()), ..Default::default() };
        assert!(!short.summary().to_string().contains("\"ab\""), "短 Key 也不该原样出现");
        let none = ModelConfig::default();
        assert!(none.summary().to_string().contains("未配置"));
    }

    /// 空字段不落盘（配置文件保持可读，不塞一堆 null）。
    #[test]
    fn empty_fields_are_omitted() {
        let d = tmp("omit");
        let p = d.join("model.json");
        ModelConfig { local_url: Some("http://x/v1".into()), ..Default::default() }
            .save_to(&p)
            .unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(!raw.contains("api_key"), "未配置的字段不该写进文件: {raw}");
        assert!(!raw.contains("local_only"), "false 的布尔值不该写进文件: {raw}");
        assert!(raw.contains("local_url"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
