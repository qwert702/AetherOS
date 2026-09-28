//! 最小可用中文输入法（2.3）：拼音 → 候选 → 上屏。
//!
//! **范围刻意划小**：只做「打拼音出候选、选词上屏」这条主链路。
//! 不做：云词库、模糊音、整句联想、用户词频学习、自定义短语。
//! 那些是输入法产品的战场，不是这个阶段该投入的地方 —— 而没有这条主链路，
//! 中文用户连一句「打开终端」都打不出来。
//!
//! **词表自建**（见 `DICT`）：几十条常用词，体积可忽略、无许可问题。
//! 这是文档里要求的「先定数据来源与许可」的落地 —— 开源拼音表的体积评估
//! 留待真正需要全量词库时再做。
//!
//! **开关是显式的**（`Ctrl+Space`），不是"打字母就自动进拼音"。原因：
//! AI 指令条里大量输入英文（`open terminal`、路径、命令），自动进入会让
//! 每一次英文输入都先弹一排候选。中文用户按一次 `Ctrl+Space` 的成本，
//! 远低于每个英文单词都被打断的成本。

/// 自建拼音词表：`(拼音, 候选词列表)`。候选按常用度排序。
///
/// 只收常用词与演示路径上的词。**不追求覆盖度** —— 打不出来的词会
/// 保留拼音原样上屏（见 `commit`），不会丢字。
const DICT: &[(&str, &[&str])] = &[
    ("ni", &["你", "尼", "泥"]),
    ("hao", &["好", "号", "耗"]),
    ("nihao", &["你好"]),
    ("wo", &["我", "握", "窝"]),
    ("de", &["的", "得", "地"]),
    ("le", &["了", "乐"]),
    ("shi", &["是", "事", "时", "十"]),
    ("bu", &["不", "部", "步"]),
    ("zai", &["在", "再", "载"]),
    ("you", &["有", "又", "友", "由"]),
    ("zhongwen", &["中文"]),
    ("zhong", &["中", "种", "重", "终"]),
    ("wen", &["文", "问", "闻"]),
    ("shuru", &["输入"]),
    ("shurufa", &["输入法"]),
    ("diannao", &["电脑"]),
    ("xitong", &["系统"]),
    ("wenjian", &["文件"]),
    ("wenjianjia", &["文件夹"]),
    ("mulu", &["目录"]),
    ("chuangkou", &["窗口"]),
    ("zhuomian", &["桌面"]),
    ("zhongduan", &["终端"]),
    ("mingling", &["命令"]),
    ("yunxing", &["运行"]),
    ("chengxu", &["程序"]),
    ("yingyong", &["应用"]),
    ("shezhi", &["设置"]),
    ("peizhi", &["配置"]),
    ("moxing", &["模型"]),
    ("zhineng", &["智能"]),
    ("renzhi", &["人智"]),
    ("rengong", &["人工"]),
    ("jiqi", &["机器"]),
    ("xuexi", &["学习"]),
    ("bangzhu", &["帮助"]),
    ("dakai", &["打开"]),
    ("guanbi", &["关闭"]),
    ("chakan", &["查看"]),
    ("shiyong", &["使用"]),
    ("anzhuang", &["安装"]),
    ("xiazai", &["下载"]),
    ("shangchuan", &["上传"]),
    ("baocun", &["保存"]),
    ("shanchu", &["删除"]),
    ("chongmingming", &["重命名"]),
    ("xinjian", &["新建"]),
    ("fuzhi", &["复制"]),
    ("zhantie", &["粘贴"]),
    ("jianqie", &["剪切"]),
    ("sousuo", &["搜索"]),
    ("chazhao", &["查找"]),
    ("xuanze", &["选择"]),
    ("quxiao", &["取消"]),
    ("queding", &["确定"]),
    ("queren", &["确认"]),
    ("fanhui", &["返回"]),
    ("shangyi", &["上一"]),
    ("xiayi", &["下一"]),
    ("xiangqing", &["详情"]),
    ("bangben", &["版本"]),
    ("gengxin", &["更新"]),
    ("rizhi", &["日志"]),
    ("cuowu", &["错误"]),
    ("jinggao", &["警告"]),
    ("chenggong", &["成功"]),
    ("shibai", &["失败"]),
    ("wancheng", &["完成"]),
    ("kaishi", &["开始"]),
    ("jieshu", &["结束"]),
    ("dengdai", &["等待"]),
    ("lianjie", &["连接"]),
    ("duankai", &["断开"]),
    ("wangluo", &["网络"]),
    ("liulanqi", &["浏览器"]),
    ("yinyue", &["音乐"]),
    ("tupian", &["图片"]),
    ("wenzhang", &["文章"]),
    ("riqi", &["日期"]),
    ("shijian", &["时间"]),
    ("jintian", &["今天"]),
    ("mingtian", &["明天"]),
    ("zuotian", &["昨天"]),
    ("xianzai", &["现在"]),
    ("zenme", &["怎么"]),
    ("weishenme", &["为什么"]),
    ("shenme", &["什么"]),
    ("keyi", &["可以"]),
    ("xuyao", &["需要"]),
    ("qing", &["请", "清", "轻"]),
    ("xiexie", &["谢谢"]),
    ("zaijian", &["再见"]),
    ("duibuqi", &["对不起"]),
    ("meiguanxi", &["没关系"]),
    ("anquan", &["安全"]),
    ("quanxian", &["权限"]),
    ("yinsi", &["隐私"]),
    ("mima", &["密码"]),
    ("zhanghao", &["账号"]),
];

/// 候选数量上限（候选框放得下、数字键选得到）。
pub const MAX_CANDIDATES: usize = 6;

/// 输入法状态。
#[derive(Clone, Debug, Default)]
pub struct Ime {
    /// 中英开关（`Ctrl+Space` 切换）。**默认关** —— 见模块头注释。
    enabled: bool,
    /// 已输入的拼音串（小写 ASCII 字母）
    buffer: String,
    /// 当前候选（按常用度排序，最多 `MAX_CANDIDATES` 个）
    candidates: Vec<String>,
    /// 当前选中索引
    selected: usize,
}

impl Ime {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 切换中英。切换时**丢弃未完成的拼音**（半截状态留着只会让人困惑）。
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.reset();
    }

    /// 是否正在拼字（有未完成的拼音串）。
    pub fn composing(&self) -> bool {
        !self.buffer.is_empty()
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// 候选框是否该显示。
    pub fn show_candidates(&self) -> bool {
        self.composing() && !self.candidates.is_empty()
    }

    /// 输入一个字符。返回 `Some(文本)` 表示直接上屏该文本；
    /// 返回 `None` 表示被 IME 吃掉（进入拼音串，或什么都不做）。
    pub fn push(&mut self, ch: char) -> Option<String> {
        if !self.enabled {
            return Some(ch.to_string());
        }
        // 只有 ASCII 字母参与拼音。数字留给"选第 N 个候选"，标点直接上屏。
        if !ch.is_ascii_alphabetic() {
            return Some(ch.to_string());
        }
        self.buffer.push(ch.to_ascii_lowercase());
        self.refresh();
        None
    }

    /// 退格。返回 `true` 表示被 IME 消费（删掉拼音串的一个字母）。
    pub fn backspace(&mut self) -> bool {
        if !self.composing() {
            return false;
        }
        self.buffer.pop();
        self.refresh();
        true
    }

    /// 上下移动候选选择。
    pub fn move_selection(&mut self, delta: isize) {
        if self.candidates.is_empty() {
            return;
        }
        let n = self.candidates.len() as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
    }

    /// 选第 `idx` 个候选（0 基）。数字键用。
    pub fn select_index(&mut self, idx: usize) -> Option<String> {
        if idx >= self.candidates.len() {
            return None;
        }
        self.selected = idx;
        self.commit()
    }

    /// 提交当前选中项，返回上屏文本。
    ///
    /// **没有候选时不丢字**：把拼音原样上屏。用户打了一串没收录的词，
    /// 得到的应该是那串字母（可以自己改），而不是"按键被吃掉、什么都没发生"。
    pub fn commit(&mut self) -> Option<String> {
        if !self.composing() {
            return None;
        }
        let out = self
            .candidates
            .get(self.selected)
            .cloned()
            .unwrap_or_else(|| self.buffer.clone());
        self.reset();
        Some(out)
    }

    /// 放弃本次输入（Esc）。
    pub fn cancel(&mut self) {
        self.reset();
    }

    /// 清空未完成的输入（切换开关、提交之后）。
    fn reset(&mut self) {
        self.buffer.clear();
        self.candidates.clear();
        self.selected = 0;
    }

    /// 按当前拼音串刷新候选。
    ///
    /// 查表用**前缀匹配**而不是精确匹配：`zhong` 应该能出「中文」（`zhongwen`
    /// 的词），否则用户打到一半看不到任何反馈，以为输入法坏了。
    fn refresh(&mut self) {
        self.candidates.clear();
        self.selected = 0;
        if self.buffer.is_empty() {
            return;
        }
        // 1. 精确匹配优先（打全了就先给准确的那个）
        for (py, words) in DICT {
            if *py == self.buffer {
                for w in *words {
                    self.candidates.push((*w).to_string());
                }
                break;
            }
        }
        // 2. 前缀匹配补充（打一半也有反馈）
        if self.candidates.len() < MAX_CANDIDATES {
            for (py, words) in DICT {
                if py.starts_with(self.buffer.as_str()) && *py != self.buffer {
                    for w in *words {
                        let s = (*w).to_string();
                        if !self.candidates.contains(&s) {
                            self.candidates.push(s);
                        }
                        if self.candidates.len() >= MAX_CANDIDATES {
                            break;
                        }
                    }
                }
                if self.candidates.len() >= MAX_CANDIDATES {
                    break;
                }
            }
        }
        self.candidates.truncate(MAX_CANDIDATES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Ime {
        let mut i = Ime::default();
        i.toggle();
        i
    }

    #[test]
    fn disabled_passes_chars_through() {
        let mut i = Ime::default();
        assert_eq!(i.push('a'), Some("a".to_string()));
        assert!(!i.composing());
    }

    #[test]
    fn enabled_consumes_letters() {
        let mut i = on();
        assert_eq!(i.push('n'), None);
        assert_eq!(i.push('i'), None);
        assert_eq!(i.buffer(), "ni");
        assert!(i.composing());
    }

    /// 验收标准里的那条：打 `nihao` 出候选，选词上屏「你好」。
    #[test]
    fn nihao_commits_to_ni_hao() {
        let mut i = on();
        for c in "nihao".chars() {
            assert_eq!(i.push(c), None);
        }
        assert!(i.candidates().contains(&"你好".to_string()), "候选: {:?}", i.candidates());
        assert_eq!(i.commit(), Some("你好".to_string()));
        assert!(!i.composing(), "提交后应回到干净状态");
    }

    #[test]
    fn digits_and_punctuation_pass_through() {
        let mut i = on();
        i.push('n');
        assert_eq!(i.push('1'), Some("1".to_string()), "数字留给选词/直接上屏");
        assert_eq!(i.push('，'), Some("，".to_string()));
        assert_eq!(i.buffer(), "n", "标点不该进拼音串");
    }

    #[test]
    fn backspace_removes_letters_only() {
        let mut i = on();
        for c in "nih".chars() {
            i.push(c);
        }
        assert!(i.backspace());
        assert_eq!(i.buffer(), "ni");
        assert!(i.backspace());
        assert!(i.backspace());
        assert_eq!(i.buffer(), "");
        assert!(!i.backspace(), "空串上退格应返回 false（交给上层删正文）");
    }

    #[test]
    fn uppercase_is_lowered() {
        let mut i = on();
        i.push('N');
        i.push('I');
        assert_eq!(i.buffer(), "ni");
    }

    /// 打一半也要有反馈 —— 否则用户以为输入法坏了。
    #[test]
    fn prefix_matching_gives_feedback() {
        let mut i = on();
        for c in "zhong".chars() {
            i.push(c);
        }
        assert!(!i.candidates().is_empty(), "「zhong」应有候选");
        assert!(i.candidates().contains(&"中".to_string()));
    }

    /// 没收录的词不能丢字：拼音原样上屏。
    #[test]
    fn unknown_pinyin_commits_verbatim() {
        let mut i = on();
        for c in "zzzzzz".chars() {
            i.push(c);
        }
        assert_eq!(i.commit(), Some("zzzzzz".to_string()), "打不出来的词应原样上屏");
    }

    #[test]
    fn selection_wraps_around() {
        let mut i = on();
        i.push('s');
        i.push('h');
        i.push('i');
        let n = i.candidates().len();
        assert!(n >= 2, "「shi」应有多个候选: {:?}", i.candidates());
        i.move_selection(-1);
        assert_eq!(i.selected(), n - 1, "上移应回绕到末尾");
        i.move_selection(1);
        assert_eq!(i.selected(), 0, "下移应回绕到开头");
    }

    #[test]
    fn select_index_commits_that_candidate() {
        let mut i = on();
        for c in "shi".chars() {
            i.push(c);
        }
        let second = i.candidates().get(1).cloned().expect("应有第二个候选");
        assert_eq!(i.select_index(1), Some(second));
        assert!(!i.composing());
    }

    #[test]
    fn select_out_of_range_is_noop() {
        let mut i = on();
        i.push('n');
        assert_eq!(i.select_index(99), None);
        assert!(i.composing(), "越界选择不该清掉输入");
    }

    #[test]
    fn cancel_discards_without_output() {
        let mut i = on();
        for c in "nihao".chars() {
            i.push(c);
        }
        i.cancel();
        assert!(!i.composing());
        assert_eq!(i.commit(), None, "取消后不该再吐出东西");
    }

    #[test]
    fn toggle_clears_half_typed_state() {
        let mut i = on();
        for c in "nih".chars() {
            i.push(c);
        }
        i.toggle(); // 关掉
        assert!(!i.composing(), "切换开关应丢弃半截拼音");
        assert!(!i.enabled());
        // 再打开应能正常工作
        i.toggle();
        for c in "nihao".chars() {
            i.push(c);
        }
        assert_eq!(i.commit(), Some("你好".to_string()));
    }

    #[test]
    fn candidates_are_capped() {
        let mut i = on();
        for c in "z".chars() {
            i.push(c);
        }
        assert!(i.candidates().len() <= MAX_CANDIDATES);
    }
}
