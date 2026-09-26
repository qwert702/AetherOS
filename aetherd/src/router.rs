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
    /// 本轮对话上下文里是否已含**敏感工具结果**（如 `read_file` 读到的文件内容）。
    ///
    /// 这是 P0-4b 的核心修正：此前用 `history_len`（消息条数）当"复杂度"信号，
    /// 而每轮工具调用会给消息表 +2 条，于是"任何多轮工具任务在第 3 轮必然上云"——
    /// 前两轮读到的文件内容会被整体发往云端端点。现在改为：**只看内容敏感度**，
    /// 一旦上下文里进了文件内容，本轮强制本地，无论对话多长。
    pub sensitive_context: bool,
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
///
/// 注意这里**不再包含"历史长度"**。多轮工具调用是"任务在推进"，
/// 不是"任务需要大模型"——把它当复杂度信号会稳定造成数据外泄。
const COMPLEX_HINTS: &[&str] = &["然后", "接着", "计划", "步骤", "批量", "所有窗口", "诊断", "为什么", "分析"];

pub fn route(task: &Task) -> Channel {
    // 1. 隐私模式：强制本地，即使本地不可用也回本地（宁可降级不外泄）
    if task.local_only {
        return Channel::Local;
    }
    // 2. 上下文含敏感工具结果（文件内容）：强制本地。
    //    优先级高于"复杂任务上云"——内容已经在本机了，就不能再送出去。
    if task.sensitive_context {
        return Channel::Local;
    }
    // 3. 含隐私内容：强制本地
    let lower = task.text.to_lowercase();
    if PRIVACY_HINTS.iter().any(|k| lower.contains(k)) {
        return Channel::Local;
    }
    // 4. 云端不可用：本地
    if !task.cloud_available {
        return Channel::Local;
    }
    // 5. 本地不可用：云端
    if !task.local_available {
        return Channel::Cloud;
    }
    // 6. 复杂任务上云：多步信号 or 文本较长（**不再看历史条数**）
    let complex = COMPLEX_HINTS.iter().any(|k| lower.contains(k));
    if complex || task.text.chars().count() > 120 {
        return Channel::Cloud;
    }
    // 7. 默认本地（快、免费、离线）
    Channel::Local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(text: &str) -> Task<'_> {
        Task {
            text,
            sensitive_context: false,
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

    /// P0-4b 核心：上下文里进了文件内容 → 强制本地，即使任务本身"复杂"。
    #[test]
    fn sensitive_context_forces_local_even_when_complex() {
        let mut t = task("分析一下这些内容，然后给出优化计划步骤");
        assert_eq!(route(&t), Channel::Cloud, "无敏感上下文时复杂任务应上云（对照组）");
        t.sensitive_context = true;
        assert_eq!(route(&t), Channel::Local, "含敏感工具结果时必须留在本地");
        // 即使本地不可用也不外泄——与隐私关键词同等强度
        t.local_available = false;
        assert_eq!(route(&t), Channel::Local, "宁可降级到不可用的本地，也不外泄文件内容");
    }

    /// 回归：多轮工具调用不再因为"消息条数变多"而上云。
    ///
    /// 旧实现用 `history_len >= 6` 当复杂度信号，而每轮工具调用给消息表 +2 条，
    /// 于是"两次 read_file 之后的第 3 轮"必然上云，把前两轮读到的文件内容整体带走。
    #[test]
    fn tool_rounds_alone_do_not_force_cloud() {
        // 一句 13 字、无隐私、无复杂信号的输入：无论上下文多长都必须留在本地
        assert_eq!(route(&task("帮我看一下机器上都有什么")), Channel::Local);
        // 云端通道本身没被禁用：真有复杂信号时仍然上云
        assert_eq!(route(&task("分析一下这台机器的磁盘占用")), Channel::Cloud);
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
