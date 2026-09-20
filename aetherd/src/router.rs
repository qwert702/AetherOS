//! 混合推理路由：本地（Ollama）与云端（GLM 等 OpenAI 兼容 API）之间
//! 按任务特征自动选择。隐私优先、离线兜底、复杂任务上云。

/// 可用的推理通道。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// 本地小模型：隐私任务、离线、高频低难度
    Local,
    /// 云端大模型：复杂推理、长上下文、多步规划
    Cloud,
}

impl Channel {
    /// IPC 通道标识（"local"/"cloud"）：随 ChatChunk 回传客户端，
    /// 合成器顶栏 AI 三态据此渲染（ui-design-handover 4.1）。
    pub fn label(self) -> &'static str {
        match self {
            Channel::Local => "local",
            Channel::Cloud => "cloud",
        }
    }
}

/// 路由决策依据。
#[derive(Clone, Debug)]
pub struct Task<'a> {
    pub text: &'a str,
    /// 会话历史条数（多轮任务倾向云端）
    pub history_len: usize,
    /// 用户标记本次会话"仅本地"（隐私模式）
    pub local_only: bool,
    /// 本地通道是否可用（Ollama 探活结果）
    pub local_available: bool,
    /// 是否配置了云端 API Key
    pub cloud_available: bool,
}

/// 含隐私关键词的请求绝不离开本机。
const PRIVACY_HINTS: &[&str] = &["密码", "密钥", "token", "身份证", "银行", "私人", "隐私"];

/// 复杂任务信号：多步指令、系统级操作、长文本。
const COMPLEX_HINTS: &[&str] = &["然后", "接着", "计划", "步骤", "批量", "所有窗口", "诊断", "为什么", "分析"];

pub fn route(task: &Task) -> Channel {
    // 1. 隐私模式：强制本地，即使本地不可用也回本地（宁可降级不外泄）
    if task.local_only {
        return Channel::Local;
    }
    // 2. 含隐私内容：强制本地
    let lower = task.text.to_lowercase();
    if PRIVACY_HINTS.iter().any(|k| lower.contains(k)) {
        return Channel::Local;
    }
    // 3. 云端不可用：本地
    if !task.cloud_available {
        return Channel::Local;
    }
    // 4. 本地不可用：云端
    if !task.local_available {
        return Channel::Cloud;
    }
    // 5. 复杂任务上云：多步信号 or 历史较长 or 文本较长
    let complex = COMPLEX_HINTS.iter().any(|k| lower.contains(k));
    if complex || task.history_len >= 6 || task.text.chars().count() > 120 {
        return Channel::Cloud;
    }
    // 6. 默认本地（快、免费、离线）
    Channel::Local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(text: &str) -> Task<'_> {
        Task {
            text,
            history_len: 0,
            local_only: false,
            local_available: true,
            cloud_available: true,
        }
    }

    #[test]
    fn simple_goes_local() {
        assert_eq!(route(&task("打开终端")), Channel::Local);
    }

    #[test]
    fn privacy_stays_local_even_if_cloud_better() {
        assert_eq!(route(&task("帮我生成一个强密码")), Channel::Local);
        let mut t = task("分析我的银行账单然后总结");
        t.local_available = false;
        // 宁可降级到不可用的本地，也不把隐私内容送云端
        assert_eq!(route(&t), Channel::Local);
    }

    #[test]
    fn multi_step_goes_cloud() {
        assert_eq!(route(&task("先清理磁盘，然后诊断为什么风扇一直转，接着给出优化计划")), Channel::Cloud);
    }

    #[test]
    fn long_history_goes_cloud() {
        let mut t = task("继续");
        t.history_len = 8;
        assert_eq!(route(&t), Channel::Cloud);
    }

    #[test]
    fn local_only_forces_local() {
        let mut t = task("随便什么复杂任务计划步骤批量分析");
        t.local_only = true;
        assert_eq!(route(&t), Channel::Local);
    }

    #[test]
    fn fallback_when_one_side_down() {
        let mut t = task("打开终端");
        t.local_available = false;
        assert_eq!(route(&t), Channel::Cloud);
        let mut t2 = task("打开终端");
        t2.cloud_available = false;
        assert_eq!(route(&t2), Channel::Local);
    }

    #[test]
    fn channel_labels_are_stable() {
        // IPC 通道标识是线协议的一部分：改动会破坏顶栏状态渲染，须显式回归
        assert_eq!(Channel::Local.label(), "local");
        assert_eq!(Channel::Cloud.label(), "cloud");
    }
}
