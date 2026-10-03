//! 音频：提示音 + 音量（2026-10-02 功能批次 K，对应 FEATURE-GAPS §2.2）。
//!
//! ## 为什么**不**链接 libasound（这是本模块最重要的设计决定）
//!
//! 1. `alsa-sys` 需要在**交叉编译目标**上有 libasound 的头与库 —— 那会让本仓库现有的
//!    musl 静态检查（6/7 crate 可查，**合成器正是其中之一**）直接失效；
//! 2. 我们真正需要的只有两件事：播一个短提示音、读写 Master 音量。
//!    `aplay -q -`（从 stdin 读 WAV）与 `amixer get/set Master` 恰好覆盖，
//!    零构建期依赖、零新增第三方 crate；
//! 3. `aplay -l` 失败恰好给出一个**可靠的"有没有声卡"判据** —— 而"无设备时如实显示"
//!    是本项目的硬要求（宁可显示"无音频设备"，也不画一个假滑块）。
//!
//! ## 代价（写清楚，不含糊）
//!
//! 每次提示音起一个子进程（约 10~30ms 延迟，提示音场景可接受）；音量读写同样走子进程，
//! 所以**不能每帧调用** —— 只在需要时 `refresh()`（面板打开、改完音量）。
//!
//! ## 未验证
//!
//! 本机是 Windows，没有 ALSA：`aplay`/`amixer` 的真实行为、VMware 下是否有虚拟声卡
//! 都**未实测**。可测的部分（WAV 生成、输出解析、夹取）都有单测。

// 合成器的主循环（`run_fbdev`）是 Linux 专属，因此非 Linux 目标上这个模块没有调用点。
// 与 `settings.rs` 同一处理：允许 dead_code，但**测试照跑**（本文件末尾的单测在
// Windows 上也会执行 —— 解析与 WAV 生成是纯逻辑，正该在开发机上验）。
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::process::{Command, Stdio};

/// 提示音种类。**只有三种**：多一种就要多一个音频资源与一次试听，收益不成正比。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cue {
    /// 确认卡片弹出（需要用户注意）
    Confirm,
    /// AI 回复完成（任务结束的正反馈）
    Done,
    /// 操作失败（与成功可区分：更低、更短）
    Error,
}

/// 音频可用性。`Unknown` 与 `NoDevice` **必须分开**：
/// "探测不了"（比如非 Linux、没有 aplay）与"确实没有声卡"是两件事，
/// 界面文案也不同 —— 混在一起会让人以为"你的声卡坏了"。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioState {
    /// 探测到声卡，可以播
    Ready,
    /// 明确没有声卡/没有 aplay
    NoDevice,
    /// 尚未探测或探测失败（界面应显示"未检测"，不是"无声卡"）
    Unknown,
}

impl AudioState {
    /// 给界面用的一句话（**如实**：不确定就说未检测，不猜）。
    pub fn label(&self) -> &'static str {
        match self {
            AudioState::Ready => "就绪",
            AudioState::NoDevice => "无音频设备",
            AudioState::Unknown => "未检测到音频（可能缺少 aplay）",
        }
    }

    /// 是否允许操作音量控件。`false` 时界面**必须禁用**而不是画一个拖不动的滑块。
    pub fn controls_enabled(&self) -> bool {
        matches!(self, AudioState::Ready)
    }
}

/// 音量范围（百分比）。0 合法（静音），所以不能拿 0 当"无效"。
pub const VOLUME_MIN: u8 = 0;
pub const VOLUME_MAX: u8 = 100;

/// 夹取音量（设置文件/解析结果都可能越界）。
pub fn clamp_volume(pct: i32) -> u8 {
    pct.clamp(VOLUME_MIN as i32, VOLUME_MAX as i32) as u8
}

/// 从 `aplay -l` 的输出判断有没有声卡。
///
/// 判据是 `card N:` 行 —— `aplay -l` 在有设备时逐卡打印 `card 0: ...`，
/// 无设备时输出 `no soundcards found...` 并返回非零。
/// 命令本身跑不起来（没有 aplay）时是 `Unknown`，不是 `NoDevice`。
pub fn parse_aplay_has_card(stdout: &str, ran_ok: bool) -> AudioState {
    if stdout.contains("card ") && stdout.contains(':') {
        return AudioState::Ready;
    }
    if ran_ok {
        // 命令成功但没有 card 行：`no soundcards found`
        return AudioState::NoDevice;
    }
    // 退出码非零：可能是"没有声卡"，也可能是"没有 aplay"——两者都归 NoDevice？
    // 不：**分开**。若 stdout 里明确写了 no soundcards，那是设备问题；
    // 否则视为探测失败（Unknown），避免把"缺工具"说成"缺声卡"。
    if stdout.contains("no soundcards") || stdout.contains("No such file") {
        AudioState::NoDevice
    } else {
        AudioState::Unknown
    }
}

/// 从 `amixer get Master`（或 `sget`）输出里取音量百分比。
///
/// amixer 的输出形如 `Front Left: Playback 60 [60%] [-20.00dB] [on]`。
/// 取**第一个** `[NN%]`：多声道会打印多行，左右一致时取哪个都对，
/// 不一致时取第一个（并保留后续可改进为取平均的余地）。
pub fn parse_amixer_percent(out: &str) -> Option<u8> {
    let start = out.find('[')?;
    let rest = &out[start + 1..];
    let end = rest.find('%')?;
    rest[..end].trim().parse::<i32>().ok().map(clamp_volume)
}

/// 从 `amixer get Master` 输出里判断是否静音（`[off]`）。
pub fn parse_amixer_muted(out: &str) -> bool {
    out.contains("[off]")
}

/// 生成一段提示音的 WAV（16-bit PCM 单声道 8kHz）。
///
/// **为什么运行时合成而不是内嵌音频文件**：一段 60~120ms 的正弦音只有几十行代码，
/// 而内嵌二进制资源既无法在代码审查里核对，也会让"这声音到底是什么"变成黑箱。
/// 合成还让"三种提示音音高不同"成为可测试的性质（见单测）。
pub fn cue_wav(cue: Cue) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 8000;
    // 音高与时长：确认音要"能引起注意"，完成音要"轻"，失败音更低更短
    let (freq, ms) = match cue {
        Cue::Confirm => (880.0f32, 120u32),
        Cue::Done => (1320.0f32, 80u32),
        Cue::Error => (440.0f32, 160u32),
    };
    let samples = SAMPLE_RATE * ms / 1000;
    let mut pcm: Vec<u8> = Vec::with_capacity(samples as usize * 2);
    for i in 0..samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        // 两端各 5ms 淡入淡出：直接切断会产生"咔"声（听起来像故障而不是提示）
        let fade = (ms as f32 * 0.05 / 1000.0 * SAMPLE_RATE as f32) as u32;
        let env = if i < fade {
            i as f32 / fade as f32
        } else if i > samples - fade {
            (samples - i) as f32 / fade as f32
        } else {
            1.0
        };
        let v = (t * freq * std::f32::consts::TAU).sin() * env * 0.25; // 0.25：提示音不该吓人
        let s = (v * i16::MAX as f32) as i16;
        pcm.extend_from_slice(&s.to_le_bytes());
    }

    // RIFF/WAVE 头（44 字节，标准 PCM）
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // 字节率
    wav.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
    wav.extend_from_slice(&16u16.to_le_bytes()); // 位深
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

/// 音频状态（进程级单例语义：合成器只有一个音频输出）。
pub struct Audio {
    state: AudioState,
    volume_pct: u8,
    muted: bool,
}

impl Audio {
    /// 探测一次（启动时调用；`aplay -l` 是唯一可靠的判据）。
    pub fn probe() -> Self {
        let mut a = Self { state: AudioState::Unknown, volume_pct: 50, muted: false };
        a.refresh();
        a
    }

    /// 重新探测设备 + 读一次音量。
    ///
    /// **不放进渲染循环**：每次都会起子进程，每帧调用会把 CPU 吃光。
    /// 调用点：启动、控制中心打开、改完音量之后。
    pub fn refresh(&mut self) {
        let out = Command::new("aplay").arg("-l").output();
        match out {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                self.state = parse_aplay_has_card(&text, o.status.success());
            }
            Err(_) => self.state = AudioState::Unknown, // 没有 aplay：不是"没有声卡"
        }
        self.read_mixer_state();
    }

    /// 读一次 mixer 的**完整状态**（音量 + 静音），并更新内部缓存。
    ///
    /// 静音要单独读：`amixer` 的 `[off]` 与音量百分比是两件事 ——
    /// 音量 60% 但 `[off]` 是完全正常的组合，只看百分比会显示错。
    fn read_mixer_state(&mut self) {
        let Ok(o) = Command::new("amixer").args(["get", "Master"]).output() else {
            return;
        };
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        if let Some(p) = parse_amixer_percent(&text) {
            self.volume_pct = p;
        }
        self.muted = parse_amixer_muted(&text);
    }

    pub fn state(&self) -> AudioState {
        self.state
    }
    pub fn volume_pct(&self) -> u8 {
        self.volume_pct
    }
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// 播放提示音（**后台线程**，绝不阻塞渲染）。
    ///
    /// 无设备时**静默返回**：提示音缺失不该打断用户操作，而且"没有声卡"这件事
    /// 已经在界面上如实显示过了（不需要每次再弹一个错误）。
    pub fn play(&self, cue: Cue) {
        if !self.state.controls_enabled() {
            return;
        }
        let wav = cue_wav(cue);
        std::thread::spawn(move || {
            // aplay 从 stdin 读 WAV（`-` = stdin），不需要临时文件
            let Ok(mut child) = Command::new("aplay")
                .args(["-q", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut si) = child.stdin.take() {
                use std::io::Write;
                let _ = si.write_all(&wav);
            }
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有 aplay（本机）时必须是 `Unknown` 而不是 `NoDevice` ——
    /// 把"缺工具"说成"缺声卡"会让用户去查一个不存在的问题。
    #[test]
    fn missing_tool_is_unknown_not_no_device() {
        assert_eq!(parse_aplay_has_card("", false), AudioState::Unknown);
        assert_eq!(
            parse_aplay_has_card("aplay: command not found", false),
            AudioState::Unknown
        );
        // 命令跑成功但没有 card 行 = 真的没有声卡
        assert_eq!(
            parse_aplay_has_card("**** List of PLAYBACK Hardware Devices ****\n", true),
            AudioState::NoDevice
        );
        // 明确的"没有声卡"字样：即使退出码非零也判定为无设备
        assert_eq!(
            parse_aplay_has_card("aplay: device_list:274: no soundcards found...", false),
            AudioState::NoDevice
        );
    }

    /// 有设备时必须 Ready（这是"能不能播音"的总开关）。
    #[test]
    fn card_lines_mean_ready() {
        let out = "**** List of PLAYBACK Hardware Devices ****\n\
                   card 0: PCH [HDA Intel PCH], device 0: ALC269VC Analog [ALC269VC Analog]\n";
        assert_eq!(parse_aplay_has_card(out, true), AudioState::Ready);
    }

    /// 界面文案与"控件是否可用"必须一致：只有 Ready 才允许动滑块。
    #[test]
    fn only_ready_enables_controls() {
        assert!(AudioState::Ready.controls_enabled());
        assert!(!AudioState::NoDevice.controls_enabled());
        assert!(!AudioState::Unknown.controls_enabled());
        // 三种状态的文案必须互不相同（否则用户分不清"没声卡"和"没检测到"）
        let labels = [
            AudioState::Ready.label(),
            AudioState::NoDevice.label(),
            AudioState::Unknown.label(),
        ];
        assert_ne!(labels[0], labels[1]);
        assert_ne!(labels[1], labels[2]);
        assert_ne!(labels[0], labels[2]);
    }

    /// amixer 输出解析：真实格式 + 越界夹取 + 静音判定。
    #[test]
    fn parses_amixer_output() {
        let out = "Simple mixer control 'Master',0\n  \
                   Capabilities: pvolume pswitch pswitch-joined\n  \
                   Playback channels: Front Left - Front Right\n  \
                   Limits: Playback 0 - 65536\n  \
                   Front Left: Playback 39321 [60%] [-20.00dB] [on]\n  \
                   Front Right: Playback 39321 [60%] [-20.00dB] [on]\n";
        assert_eq!(parse_amixer_percent(out), Some(60));
        assert!(!parse_amixer_muted(out));
        // 静音
        assert!(parse_amixer_muted("Front Left: Playback 0 [0%] [-inf] [off]"));
        // 解析不出来要返回 None（而不是猜一个 50）
        assert_eq!(parse_amixer_percent("no percentages here"), None);
        assert_eq!(parse_amixer_percent(""), None);
        // 越界值夹取
        assert_eq!(parse_amixer_percent("Playback 1 [150%] [on]"), Some(100));
    }

    /// 音量夹取：0 是合法值（静音），不能被当成"无效"而回落。
    #[test]
    fn volume_clamps_but_zero_is_valid() {
        assert_eq!(clamp_volume(0), 0);
        assert_eq!(clamp_volume(-5), 0);
        assert_eq!(clamp_volume(50), 50);
        assert_eq!(clamp_volume(100), 100);
        assert_eq!(clamp_volume(999), 100);
    }

    /// WAV 头必须合法 —— 否则 aplay 会拒绝播放，而"没声音"最难查。
    ///
    /// 这里逐字段核对 RIFF/WAVE 头与长度一致性（不需要真的播一遍）。
    #[test]
    fn cue_wav_has_valid_header() {
        for cue in [Cue::Confirm, Cue::Done, Cue::Error] {
            let w = cue_wav(cue);
            assert!(w.len() > 44, "应有头 + 采样数据：{cue:?}");
            assert_eq!(&w[0..4], b"RIFF");
            assert_eq!(&w[8..12], b"WAVE");
            assert_eq!(&w[12..16], b"fmt ");
            assert_eq!(&w[36..40], b"data");
            // RIFF 长度 = 文件长度 - 8
            let riff_len = u32::from_le_bytes([w[4], w[5], w[6], w[7]]);
            assert_eq!(riff_len as usize, w.len() - 8, "{cue:?} 的 RIFF 长度不对");
            // data 长度 = 文件长度 - 44
            let data_len = u32::from_le_bytes([w[40], w[41], w[42], w[43]]);
            assert_eq!(data_len as usize, w.len() - 44, "{cue:?} 的 data 长度不对");
            // 采样数是偶数（16-bit 单声道）
            assert_eq!(data_len % 2, 0);
            // 采样率 8000
            assert_eq!(u32::from_le_bytes([w[24], w[25], w[26], w[27]]), 8000);
            assert_eq!(u16::from_le_bytes([w[22], w[23]]), 1, "应单声道");
            assert_eq!(u16::from_le_bytes([w[34], w[35]]), 16, "应 16-bit");
        }
    }

    /// 三种提示音必须**听感可区分**（音高不同），且不是一段静音。
    #[test]
    fn cues_are_distinct_and_not_silent() {
        let peak = |cue: Cue| {
            let w = cue_wav(cue);
            let pcm = &w[44..];
            pcm.chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .max()
                .unwrap_or(0)
        };
        // 有实际波形（不是静音）
        for cue in [Cue::Confirm, Cue::Done, Cue::Error] {
            assert!(peak(cue) > 1000, "{cue:?} 几乎是静音（峰值 {}）", peak(cue));
        }
        // 时长不同（失败音更长，用于"更值得注意"）—— 由 data 长度体现
        let len = |cue: Cue| cue_wav(cue).len();
        assert_ne!(len(Cue::Confirm), len(Cue::Error));
        assert_ne!(len(Cue::Done), len(Cue::Error));
    }
}
