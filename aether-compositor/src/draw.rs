//! Aether 桌面绘制逻辑 —— "Essence" 视觉语言。
//!
//! 设计原则（Apple 级质感，但不是复刻）：
//! - 中性深灰磨砂面板，绝不用高饱和大色块
//! - 发丝级白色描边（hairline）+ 大而柔的多层投影
//! - 连续圆角、克制的单一青色点缀、粗细分明的字重层级
//! - 壁纸是低对比的深色柔光渐变，负责衬托而非抢戏
//!
//! 纯软件光栅化，绘制函数将来同时服务于主机预览后端与 smithay 后端。

use crate::text::{draw_text, strings, TextRenderer};
use self::theme::{color, elevation, font, metric, radius, state};

mod content;
pub use content::*;


mod icons;
pub use icons::*;

mod overlays;
pub use overlays::*;


mod files;
pub use files::*;

mod desk;
pub use desk::*;


mod primitives;
pub use primitives::*;

mod wallpaper;
pub use wallpaper::*;

mod damage;
pub use damage::*;

mod settings;
pub use settings::draw_settings_content;


/// "Essence" 设计令牌 —— 全系统视觉的单一事实来源（对应
/// docs/归档/界面设计方案（UI-DESIGN-PLAN）-2026-09-19.md §3：色板 / 字阶 / 圆角与阴影 / 交互态）。
///
/// 双模：**明亮（默认，产品主视觉）** 与 深空（备选，`--theme dark`）。
/// 颜色一律走函数取值，调用点形如 `color::surface_1()`；模式在进程启动时定一次。
///
/// 层级模型（从底到顶）：壁纸 → INSET（底座：菜单栏/Dock/侧栏/终端）
/// → SURFACE_1（窗口）→ SURFACE_2（内容"纸面"）→ SURFACE_3（输入框/浮层）。
///
/// 两种模式的**明度方向一致**：内容面比窗口体更亮（"抬起来"），
/// 底座比窗口体更暗（"沉下去"）。深色模式里"更亮"是加白，明亮模式里是加黑，
/// 方向不变、语义不变——这样所有调用点不必区分模式。
pub mod theme;

/// 控制中心的行布局：**高度从行数推导**，不写死数字。
///
/// 为什么（2026-10-02 功能批次 J-2）：`PANEL_H` 原先是手写常量，旁边一句注释
/// "加一行就要同步这里"。那种约定迟早会忘 —— 忘了的后果是最后一行**画在面板外**
/// （看得见、点不到，或直接被裁掉），而且只在特定分辨率下才明显。
/// 现在行数与行高是唯一事实来源，高度是算出来的：加行自动变高。
pub mod control_layout;



// ---------------------------------------------------------------------------
// 桌面状态
// ---------------------------------------------------------------------------

pub struct Renderer {
    bg: Vec<u32>,
    frames_since_bg: u32,
    bg_w: usize,
    bg_h: usize,
    /// 壁纸分帧生成进度：已生成的行数（= h 表示整屏就绪）。
    ///
    /// 壁纸整屏生成一次要 0.4–0.6 秒，压在一帧里就是开机后一次明显卡顿。
    /// 改成每帧生成 `BG_ROWS_PER_FRAME` 行后，单帧只付 1/N 的成本，
    /// 而绘制目标是独立的 `bg` 缓冲，未完成前屏幕上看不到横向接缝。
    bg_row: usize,
    /// 壁纸 + 全部窗口投影的合成层。
    ///
    /// 投影只取决于窗口几何与激活态，鼠标移动时完全不变；而它每帧要跑
    /// 5 层 × 全窗口面积的 SDF，是 `draw_window` 的主要成本。把它烘焙进
    /// 这一层后，稳态每帧只剩一次内存拷贝。
    shadow_layer: Vec<u32>,
    /// 烘焙层对应的窗口几何指纹（变了才重算）
    shadow_key: u64,
    /// 菜单栏各菜单标签的命中区（每帧更新）
    pub menubar_menus: Vec<Rect>,
    /// Dock 图标命中区
    pub dock_icons: Vec<Rect>,
    /// 已装应用（`(id, 显示名)`），按 id 排序。由 main 扫描 `/var/apps` 后填进来。
    ///
    /// 为什么放在 Renderer 上而不是当参数传：`draw_dock` 的调用链已经很深，
    /// 而且这份数据是"每帧画一次"的稳定状态，不是逐帧变化的参数。
    pub installed_apps: Vec<(String, String)>,
    /// 用户设置（P3）：启动时从 `/var/lib/aether/settings.json` 读入，改动即写回
    pub settings: crate::settings::Settings,
    /// 设置窗口当前页（左栏选中项）
    pub settings_page: usize,
    /// 「网络」页的 DNS 输入框状态（§2.4）。**不放进 `Settings`**：
    /// 那个结构会被序列化落盘，而这个只是编辑期的临时状态（含光标与未提交文本）。
    pub dns: crate::settings::DnsField,
    /// §2.3：改口令表单（三个**遮显**输入框 + 焦点 + 回执）。
    ///
    /// 为什么放在 `Renderer` 而不是 `Settings`：`Settings` 会被**写到磁盘**
    /// （`settings.json`）—— 口令绝不能进那里。
    pub pass_form: crate::settings::PassForm,
    /// §2.3：人类账户列表（只读，来自 aetherd 的 `ListUsers`）。
    pub users: Vec<aether_ipc::UserInfo>,
    /// 是否已经读过一次（区分"正在读取"与"确实没有账户"）。
    pub users_loaded: bool,
    /// §3.7：检测到的外设（只读，来自 aetherd 的 `ListPeripherals`）。
    pub peripherals: Vec<aether_ipc::PeripheralWire>,
    /// 是否已经读过一次。
    pub peripherals_loaded: bool,
    /// 锁屏状态（§2.3）。放在 Renderer 里是因为"锁"影响**每一帧的绘制**
    /// （整屏遮罩），而不只是某个页面。
    pub lock: crate::lock::LockScreen,
    /// 命令面板（§5.5）。跨帧存在（查询、结果、选中项），所以要放在 Renderer 上。
    pub palette: crate::palette::Palette,
    /// 「工具权限」页的视图状态（§5.3）。列表来自 aetherd（异步取），状态跨帧保留。
    pub policy: crate::policyview::PolicyView,
    /// 首次启动向导（§5.8）。状态跨帧保留（走到第几步、还开不开）。
    pub setup: crate::setup::Setup,
    /// 「应用商店」页的状态（§5.11）。目录扫描只在进页面/点重扫时做，不每帧做。
    pub store: crate::store::StoreView,
    /// 「时钟与时区」页的对时/系统时区状态（§3.3）。
    pub time: crate::timeview::TimeView,
    /// 「快照与回滚」页的状态（§5.6）。
    pub snapshots: crate::snapview::SnapView,
    /// 「系统更新」页的状态（§3.1）。
    pub update: crate::updateview::UpdateView,
    /// §5.2 时间线的搜索词与编辑态。放在 Renderer 上：绘制要读它、
    /// 事件循环要写它，两者都拿得到 Renderer。
    pub audit_query: String,
    /// 搜索框是否正在接收键盘输入（与 DNS 编辑态同一套做法）。
    ///
    /// 读它在 **Linux 的输入循环**里；非 Linux 上只有测试写它 —— 按平台 allow。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub audit_editing: bool,
    /// 首启向导选项按钮的命中区（§5.8 鼠标点选）：(矩形, 选项下标)。
    pub setup_hits: Vec<(Rect, usize)>,
    /// 命令面板可见行的命中区（§5.5 鼠标点选）：(矩形, 条目下标)。
    /// 每帧重算 —— 面板的候选会随输入变化，"上一帧的命中区"没有意义。
    pub palette_hits: Vec<(Rect, usize)>,
    /// 显示输出（§3.8）：**只读**，没有交互状态，所以直接存列表而不是单开视图模块。
    pub displays: Vec<aether_ipc::DisplayWire>,
    /// 合成器驱动的输出下标（由 aetherd 告知，约定见 `aetherd::display::DRIVEN_INDEX`）。
    pub driven_index: u32,
    /// 是否已经拉过一次（决定"正在读取"还是"没有输出"）
    pub displays_loaded: bool,
    /// 可移动设备（§3.7）：列表 + 正在操作的设备 + 上一次的结果。
    ///
    /// 这里有交互状态（进行中 / 结果），但只有三个字段 —— 单开一个视图模块
    /// 反而更绕（对比 §5.6/§3.1 那两个有多步确认与两步状态机的页）。
    pub removables: Vec<aether_ipc::RemovableWire>,
    pub removables_loaded: bool,
    pub removable_pending: Option<String>,
    pub removable_message: Option<String>,
    /// 当前虚拟桌面数量（§5.4）。绘制要用它显示"N 个桌面"——
    /// 桌面状态住在事件循环的 `spaces` 里（那里才拿得到窗口），所以这里只镜像一个数字。
    pub space_count: usize,
    /// 设置面板本帧登记的可交互项（绘制期登记、事件循环消费）
    pub settings_hits: Vec<(Rect, crate::settings::SettingsHit)>,
    /// 控制中心是否展开（P4）。状态放渲染器里：主循环直接读写，**不必动 `UiState`**
    /// （它有 3 个构造点，为一个小面板改结构体不划算）。
    pub control_open: bool,
    /// 顶栏"状态簇"命中区（= 现有 AI 状态区，不额外加视觉元素）
    pub status_cluster: Rect,
    /// 控制中心面板矩形（用于区分"点面板内空白"与"点别处关闭"）
    pub control_panel: Rect,
    /// 控制中心的滚动偏移（像素）。100% 下内容装得下 → 恒为 0，画面不变。
    pub control_scroll: i32,
    /// 控制中心**内容实际高度**（绘制时量出），用于把滚动量夹住。
    pub control_content_h: i32,
    /// 背景缓存需要重建（主题切换等）。惰性标记：下一帧 `prepare_background` 自动处理，
    /// 免得为切个主题把 `w/h` 传遍所有调用点。
    pub bg_dirty: bool,
    /// 性能 HUD 文本（§5.10）。`None` = 不显示。
    ///
    /// **由主循环每帧喂进来**（而不是从 settings 现算）：帧时间与内存占用
    /// 只有主循环知道，绘制层不该去读 `/proc`（那会让"画一帧"带上 IO）。
    pub perf_text: Option<String>,
    /// 通知历史（§3.2）。放在 Renderer 上而不是主循环局部变量：
    /// **两个事件循环（fbdev 与预览）都要写它**，局部变量就得声明两份，
    /// 而"只给其中一个接线"正是会产生静默不一致的那类改动。
    pub notifs: crate::notifications::Notifications,
    /// 桌面图标本帧的命中区（矩形 → 应用序号）。默认桌面**不开窗口**，图标是主要入口。
    pub desktop_icons: Vec<(Rect, usize)>,
    /// 安装向导的关闭按钮（2026-10-01：用户反馈"向导点开就关不掉"）。
    /// Esc 一直能关，但**界面上没有可见入口** —— 用户看不到就等同于关不掉。
    pub installer_close: Rect,
    /// 控制中心本帧登记的可交互项
    pub control_hits: Vec<(Rect, ControlHit)>,
    /// 中文输入法当前状态（面板要显示开关；真值在事件循环的 `ime` 里，主循环负责同步）
    pub ime_on: bool,
    /// 当前展开的下拉菜单：(各项命中区, 文案)
    pub dropdown: Option<(Vec<Rect>, Vec<&'static str>)>,
    /// 安装向导磁盘行的命中区 (矩形, 设备名, 容量MB)
    pub installer_rows: Vec<(Rect, String, u64)>,
    /// 文件管理器图标网格的命中区 (矩形, 条目索引)，每帧重建
    pub file_cells: Vec<(Rect, usize)>,
    /// 文件管理器侧栏"上级目录"按钮的命中区；无上级时为空矩形
    pub file_up: Rect,
    /// 侧栏"位置"命中区：(矩形, 侧栏索引)
    pub sidebar_hits: Vec<(Rect, usize)>,
    /// 音乐列表行矩形（**只作几何取证**：`--dump-layout` 用它，
    /// 让 `layout-check.py` 能查「行与行是否重叠」；本批**不**接点击行为）。
    pub music_hits: Vec<(Rect, usize)>,
    /// 面包屑路径段命中区：(矩形, 目标路径)
    pub crumb_hits: Vec<(Rect, String)>,
    /// 窗口红绿灯命中区：(窗口索引, 关闭按钮, 最大化按钮)。
    ///
    /// 此前红绿灯**只画不响应** —— 开了 8 个窗口后没有任何关闭手段，用户会直接卡住。
    /// 每帧重建，与窗口几何保持同步。
    pub window_lights: Vec<(usize, Rect, Rect, Rect)>,
    /// "开始安装"按钮命中区
    pub installer_button: Rect,
    /// 权限确认弹窗的按钮命中区
    pub confirm_buttons: Vec<(Rect, ConfirmButton)>,
    /// 权限确认弹窗的回显输入框命中区
    pub confirm_echo: Rect,
    /// 本帧脏矩形登记（G3）：渲染/事件路径把变化区域记进来，上屏只拷贝这些区域。
    /// 每帧 `render_frame` 开始清空；主题/壁纸/尺寸变化时记整屏。
    pub damage: Damage,
}

impl Renderer {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            bg: vec![0; w * h],
            frames_since_bg: u32::MAX,
            bg_w: w,
            bg_h: h,
            bg_row: 0,
            shadow_layer: Vec::new(),
            shadow_key: 0,
            menubar_menus: Vec::new(),
            dock_icons: Vec::new(),
            installed_apps: Vec::new(),
            settings: crate::settings::Settings::default(),
            settings_page: crate::settings::first_enabled_page(),
            dns: Default::default(),
            pass_form: crate::settings::PassForm::default(),
            users: Vec::new(),
            users_loaded: false,
            peripherals: Vec::new(),
            peripherals_loaded: false,
        lock: Default::default(),
        palette: Default::default(),
        policy: Default::default(),
        setup: Default::default(),
        store: Default::default(),
        time: Default::default(),
        snapshots: Default::default(),
        update: Default::default(),
        audit_query: String::new(),
        audit_editing: false,
        setup_hits: Vec::new(),
        palette_hits: Vec::new(),
        displays: Vec::new(),
        driven_index: 0,
        displays_loaded: false,
        removables: Vec::new(),
        removables_loaded: false,
        removable_pending: None,
        removable_message: None,
        space_count: crate::spaces::Spaces::<Win>::COUNT,
            settings_hits: Vec::new(),
            control_open: false,
            status_cluster: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_panel: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_scroll: 0,
            control_content_h: 0,
            bg_dirty: false,
            perf_text: None,
            notifs: crate::notifications::Notifications::new(),
            desktop_icons: Vec::new(),
            installer_close: Rect { x: 0, y: 0, w: 0, h: 0 },
            control_hits: Vec::new(),
            ime_on: false,
            dropdown: None,
            installer_rows: Vec::new(),
            file_cells: Vec::new(),
            file_up: Rect { x: 0, y: 0, w: 0, h: 0 },
            window_lights: Vec::new(),
            sidebar_hits: Vec::new(),
            music_hits: Vec::new(),
            crumb_hits: Vec::new(),
            installer_button: Rect { x: 0, y: 0, w: 0, h: 0 },
            confirm_buttons: Vec::new(),
            confirm_echo: Rect { x: 0, y: 0, w: 0, h: 0 },
            damage: Damage::empty(),
        }
    }

    /// 登记本帧的一个脏矩形（窗口移动/缩放、光标、菜单弹出等）。
    pub fn mark_damage(&mut self, r: Rect, w: usize, h: usize) {
        self.damage.mark(r, Rect { x: 0, y: 0, w: w as i32, h: h as i32 });
    }

    /// 取走本帧脏区并清空（供上屏路径消费）。
    // G3 保留 API：局部上屏接线到 fbdev_app 实时循环后再消费，当前不接入（见报告 §取舍）。
    #[allow(dead_code)]
    pub fn take_damage(&mut self) -> Damage {
        std::mem::take(&mut self.damage)
    }

    /// 性能计时开关（`AETHER_RENDER_TIMING=1` 启用；环境变量只读一次）。
    fn timing_enabled() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var("AETHER_RENDER_TIMING").is_ok())
    }

    /// 一次性把壁纸补全（单帧渲染与基准测试用）。
    ///
    /// 交互路径靠分帧逐步生成，而 `--shot` 只渲染一帧 —— 必须先补全，
    /// 否则截出来的图只有顶上若干行是壁纸，其余是空白。
    #[cfg_attr(target_os = "linux", allow(dead_code))] // 仅 --shot/--bench 走查路径使用
    /// 主题切换后**必须**调用：重建背景缓存并作废"背景+投影"合成层。
    ///
    /// 不重建的后果是壁纸停留在旧主题（缓存按行分帧生成，最多要 1800 帧才刷完）。
    /// P0 把壁纸改成静态后，整屏只是一次 lerp/行 —— 直接一次填满比拼帧快得多。
    pub fn invalidate_background(&mut self, w: usize, h: usize, t: f32) {
        if self.bg.len() < w * h {
            self.bg = vec![0u32; w * h];
        }
        draw_background(&mut self.bg, w, h, t, self.settings.wallpaper, self.settings.dark_mode);
        self.bg_row = h; // 背景已完整
        self.shadow_key = 0; // 合成层作废 → 下一帧重新烘焙（与 prepare_background 同一手法）
    }

    /// 整屏烘焙背景 + 投影（**走查/预览路径专用**；Linux 的 fbdev 循环在 `render_frame` 里
    /// 按 `BG_ROWS_PER_FRAME` 分帧生成，不走这里）。
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn prepare_background(&mut self, w: usize, h: usize, t: f32) {
        // 主题切换等让缓存作废的改动：这里统一补一次（调用方只需置 `bg_dirty`）
        if self.bg_dirty {
            self.invalidate_background(w, h, t);
            self.bg_dirty = false;
        }
        if self.bg.len() != w * h {
            self.bg = vec![0; w * h];
        }
        draw_background(&mut self.bg, w, h, t, self.settings.wallpaper, self.settings.dark_mode);
        self.bg_w = w;
        self.bg_h = h;
        self.bg_row = h;
        self.frames_since_bg = 0;
        self.shadow_key = 0;
    }

    /// 渲染一帧。背景每 BG_REFRESH_FRAMES 帧才重算一次（柔光漂移是
    /// 4 秒级的变化，逐帧重算纯属浪费），其余帧只是一次内存拷贝。
    pub fn render_frame(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        t: f32,
        desktop: &Desktop,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        // 每帧清一次裁剪区间：裁剪是「当帧」的语义，不跨帧泄漏（附录 DD/DI）。
        if let Some(t) = tr {
            t.set_clip(None);
        }
        let timing = Self::timing_enabled();
        // 计时关闭时 tmark 只写不读（宏体不展开），显式放行
        #[allow(unused_assignments)]
        let mut tmark = std::time::Instant::now();
        macro_rules! mark {
            ($name:literal) => {
                if timing {
                    eprintln!(
                        "[render] {:<10} {:>7.2} ms",
                        $name,
                        tmark.elapsed().as_secs_f64() * 1000.0
                    );
                    tmark = std::time::Instant::now();
                }
            };
        }

        // G3：每帧清脏区；背景/尺寸失效属于整屏重绘，记全量脏区。
        self.damage.clear();
        if self.bg_dirty || self.bg_w != w || self.bg_h != h {
            self.damage = Damage::full_screen(w, h);
        }
        // 壁纸分帧生成：每帧只生成一段行带，整屏 0.4–0.6s 的成本摊到 ~16 帧。
        //
        // 为什么必须分：这不是"优化"，是**卡顿**问题——压在单帧里实测首帧 561ms，
        // 开机后第一眼就是半秒死机（VM 上更久）。分帧后单帧只多 ~35ms/16 ≈ 2ms 级，
        // 且因为画进的是独立的 bg 缓冲，未完成时屏幕上是上一版完整壁纸，不会有接缝。
        //
        // 漂移是**分钟级**的变化，但生成一次要 ~0.4s：240 帧在 10fps 下是 24 秒、
        // 在 30fps 下只剩 8 秒 —— 帧率提上来之后必须把间隔一起提上去，否则
        // "优化帧率"反而让周期性卡顿变密。1800 帧 @30fps ≈ 60 秒。
        const BG_REFRESH_FRAMES: u32 = 1800;
        let size_changed = self.bg_w != w || self.bg_h != h || self.bg.len() != w * h;
        // 主题切换：**整屏重建**，不分帧。P0 后壁纸是静态的，整屏只是一次 lerp/行；
        // 而分帧会在半秒里露出"上半新主题、下半旧主题"的横向接缝。
        if self.bg_dirty {
            self.invalidate_background(w, h, t);
            self.bg_dirty = false;
        }
        if size_changed || self.frames_since_bg > BG_REFRESH_FRAMES {
            if self.bg.len() != w * h {
                self.bg = vec![0; w * h];
            }
            self.bg_w = w;
            self.bg_h = h;
            self.bg_row = 0;
            self.frames_since_bg = 0;
        }
        if self.bg_row < h {
            let y1 = (self.bg_row + BG_ROWS_PER_FRAME).min(h);
            draw_background_rows(
                &mut self.bg,
                w,
                h,
                t,
                self.bg_row,
                y1,
                self.settings.wallpaper,
                self.settings.dark_mode,
            );
            self.bg_row = y1;
            self.frames_since_bg = 0;
        }
        mark!("bg_gen");
        self.frames_since_bg = self.frames_since_bg.saturating_add(1);

        // 投影烘焙：只取决于窗口几何与激活态，鼠标移动时完全不变。
        // 未变化时这一层直接复用，稳态每帧只剩一次内存拷贝。
        //
        // 壁纸**生成中**时每帧跟随一次（只拷不烘）——否则屏幕会停在第 1 帧那张
        // "一条壁纸 + 一片黑"的画面上直到铺满，反而比原来更难看。
        let skey = desktop_key(desktop);
        let bg_incomplete = self.bg_row < h;
        if self.shadow_layer.len() != w * h || self.shadow_key != skey || bg_incomplete {
            if self.shadow_layer.len() != w * h {
                self.shadow_layer = vec![0; w * h];
            }
            self.shadow_layer.copy_from_slice(&self.bg);
            if bg_incomplete {
                self.shadow_key = 0; // 铺满后再烘一次（含投影）
            } else {
                for (i, win) in desktop.wins.iter().enumerate() {
                    let active = i == desktop.active;
                    shadow(
                        &mut self.shadow_layer,
                        w,
                        h,
                        win.rect,
                        radius::LG,
                        if active { elevation::elev_1() } else { elevation::elev_1_dim() },
                    );
                }
                self.shadow_key = skey;
            }
        }
        buf.copy_from_slice(&self.shadow_layer);
        mark!("bg_copy");

        if let Some(z) = ui.snap {
            rounded_rect(buf, w, h, z, radius::LG, color::accent(), 0.08);
            rounded_outline(buf, w, h, z, radius::LG, color::accent(), 0.5);
        }
        mark!("snap");

        let fv = FileView {
            cwd: &desktop.cwd,
            entries: &desktop.entries,
            selected: desktop.selected,
            error: desktop.dir_error.as_deref(),
            scroll: desktop.scroll,
            sidebar: &desktop.sidebar,
        };
        self.file_cells.clear();
        self.window_lights.clear();
        self.sidebar_hits.clear();
        self.music_hits.clear();
        self.crumb_hits.clear();
        self.settings_hits.clear();
        for (i, win) in desktop.wins.iter().enumerate() {
            // G3：窗口本帧被绘制 → 其矩形计入脏区（移动/缩放时旧位由事件循环补登）。
            self.mark_damage(win.rect, w, h);
            // 让带区带上**窗口身份**（判据 ⑪ 用它分组，附录 GH）。
            if let Some(t) = tr {
                // 浮动窗口（置顶/被拖动）**本来就会盖住别的窗口**，给它独立命名空间：
                // 它**内部**的重叠照查，跨窗口不比（附录 GI）。
                let id = if win.floating { 1000 + i as u32 } else { i as u32 };
                t.set_win_id(Some(id));
            }
            draw_window(
                buf, w, h, win, i == desktop.active,
                PageCtx {
                    mouse: ui.mouse,
                    tr,
                    settings: self.settings,
                    dns: &self.dns,
                    pass: &self.pass_form,
                    users: &self.users,
                    users_loaded: self.users_loaded,
                    peripherals: &self.peripherals,
                    peripherals_loaded: self.peripherals_loaded,
            audit_query: &self.audit_query,
                    policy: &self.policy,
                    store: &self.store,
                    time: &self.time,
                    snapshots: &self.snapshots,
                    update: &self.update,
                    displays: &self.displays,
                    driven_index: self.driven_index,
                    displays_loaded: self.displays_loaded,
                    removables: &self.removables,
                    removables_loaded: self.removables_loaded,
                    removable_pending: self.removable_pending.as_deref(),
                    removable_message: self.removable_message.as_deref(),
                },
                t, &fv,
                &mut self.file_cells, &mut self.file_up, i, &mut self.window_lights,
                &mut self.sidebar_hits, &mut self.crumb_hits, &mut self.music_hits,
                self.settings_page, &mut self.settings_hits,
            );
        }
        // 循环外的浮层（AI 气泡、命令面板、提示条）不该继承窗口身份 ——
        // 它们**本来就该盖住**别的文字，判据按窗口分组时要跳过（附录 GI）。
        if let Some(t) = tr {
            t.set_win_id(None);
        }
        mark!("windows");

        // G3：持久 UI 条带每帧重画 —— 登记顶部菜单栏与底部 Dock/AI 条带，
        // 作为脏区超集的一部分（局部上屏时这两条必须拷到）。
        self.mark_damage(
            Rect { x: 0, y: 0, w: w as i32, h: metric::MENUBAR_H },
            w,
            h,
        );
        self.mark_damage(
            Rect { x: 0, y: h as i32 - metric::BOTTOM_DOCK, w: w as i32, h: metric::BOTTOM_DOCK },
            w,
            h,
        );
        // 模态/浮层打开时背景被压暗、整屏构图都可能变 → 直接记全量脏区。
        if self.control_open
            || ui.open_menu.is_some()
            || self.setup.open
            || self.palette.open
            || ui.show_installer
            || ui.confirm.is_some()
            || ui.snap.is_some()
        {
            self.damage = Damage::full_screen(w, h);
        }

        // 桌面图标（默认桌面无窗口，它们是主要入口）—— 画在壁纸之上、窗口之下
        self.draw_desktop_icons(buf, w, h, ui, tr);
        mark!("deskicons");
        self.draw_menubar(buf, w, h, ui, tr);
        mark!("menubar");
        // 控制中心（P4）：画在顶栏之上、其它窗口之下 —— 弹出面板的常规层级
        if !self.control_open {
            // 关掉就归零：下次打开不会停在上次滚到的位置
            self.control_scroll = 0;
        }
        if self.control_open {
            self.draw_control_center(buf, w, h, ui, tr);
            // 控制中心滚起来后内容会越过面板上边界（`tr.draw` 不裁剪）——
            // 把菜单栏那一条**重画一遍**盖住它。`draw_menubar` 是幂等的。
            // **只在真的滚了时做**：100% 下滚动量恒为 0，基线画面不变。
            if self.control_scroll > 0 {
                self.draw_menubar(buf, w, h, ui, tr);
            }
            mark!("control");
        }
        if ui.open_menu.is_some() {
            self.draw_dropdown(buf, w, h, ui, tr);
        }
        // 首次启动向导（§5.8）：**盖在命令面板之上**（开机第一次进来，
        // 向导是当前唯一该做的事），但仍在锁屏之下（锁屏必须最后画，见文件末尾注释）。
        if self.setup.open {
            self.setup_hits.clear();
        draw_setup(
            buf,
            w,
            h,
            &self.setup,
            &self.settings,
            tr,
            ui.mouse,
            &mut self.setup_hits,
        );
            mark!("setup");
        }
        // 命令面板（§5.5）：在普通浮层之上、锁屏之下。
        if self.palette.open {
            self.palette_hits.clear();
        draw_palette(buf, w, h, &self.palette, tr, &mut self.palette_hits);
            mark!("palette");
        }
        if let Some((reply, kind, age)) = ui.ai_reply {
            draw_reply(buf, w, h, reply, kind, age, tr);
        }
        self.draw_ai_bar(buf, w, h, ui, t, tr);
        mark!("ai");

        let open_titles: Vec<&str> = desktop.wins.iter().map(|x| x.title.as_str()).collect();
        self.draw_dock(buf, w, h, &open_titles, ui, tr);
        mark!("dock");

        // 安装向导浮在最上层（Toast 之下）
        if let Some(inst) = &ui.installer {
            self.draw_installer(buf, w, h, inst, ui.mouse, ui.mouse_down, tr);
        }
        mark!("installer");

        // 权限确认是模态：盖在安装向导之上（安装向导的"开始安装"也会走它）
        match &ui.confirm {
            Some(c) => self.draw_confirm(buf, w, h, c, ui.mouse, ui.mouse_down, t, tr),
            None => {
                self.confirm_buttons.clear();
                self.confirm_echo = Rect { x: 0, y: 0, w: 0, h: 0 };
            }
        }
        mark!("confirm");

        if let Some(text) = self.perf_text.as_deref() {
            draw_perf_hud(buf, w, h, text, tr);
        }
        if let Some((msg, age)) = ui.toast {
            draw_toast(buf, w, h, msg, age, tr);
        }
        mark!("toast");

        // 锁屏遮罩：**最后一个画**（盖住 toast、回复气泡、控制中心、甚至安装向导）。
        // 理由不是"层级美学"，而是**隐私**：锁屏若留着通知可见，
        // 「张三给你发了消息」就直接外泄了 —— 边界会漏在最容易被忽略的地方。
        // 解锁后本块不执行，下一帧自然恢复（无残留、无需重绘全屏）。
        if self.lock.is_locked() {
            draw_lock_screen(buf, w, h, &self.lock, tr);
            mark!("lock");
        }
        // 计时关闭时最后一次赋值不会被读到，显式消费以免 dead-code 警告
        let _ = tmark;
    }
}

// ---------------------------------------------------------------------------
// 壁纸：低对比柔光渐变
// ---------------------------------------------------------------------------

/// 极光带的横截面：Lorentzian 平方（廉价除法，形态与高斯接近，
/// 但尾部更宽——正是极光边缘自然消散的样子）。
// 2026-09-29 P0：band_profile / grain / AuroraBand / wash 随"极光壁纸"一并移除，
// 静态壁纸见 draw_background_rows。


// ---------------------------------------------------------------------------
// 菜单栏：发丝底 + 品牌 + 菜单（可点击）+ 搜索胶囊 + 电池 + 时钟
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_menubar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let bar = Rect { x: 0, y: 0, w: w as i32, h: metric::MENUBAR_H };
        fill_rect(buf, w, h, bar, color::inset(), 0.62);
        fill_rect(buf, w, h, Rect { x: 0, y: metric::MENUBAR_H - 1, w: w as i32, h: 1 }, color::hairline(), 0.10);

        let Some(tr) = tr else { return };

        // 品牌：三角徽标（P0：单色 accent，去"青→紫"渐变）
        let (bx, by) = (12i32, 8i32);
        let accent = color::accent();
        for row in 0..16 {
            let half = row / 2;
            let rgb = accent;
            for col in 0..(half + 1) {
                let px = bx + half - col;
                let py = by + 15 - row;
                if px >= 0 && py >= 0 {
                    blend_pixel(buf, py as usize * w + px as usize, rgb, 0.95);
                }
            }
        }
        let brand_x = 32.0;
        tr.draw_bold(buf, w, h, brand_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), strings::BRAND, font::BODY, color::text(), 0.98);
        let mut mx = brand_x + tr.measure_bold(strings::BRAND, font::BODY) + 18.0;

        // 菜单标签（登记命中区；悬停/打开用交互态令牌）
        self.menubar_menus.clear();
        // §3.5：菜单名按语言取（`menu_label` 取不到英文会退回中文，不会空白）
        for i in 0..strings::MENUS.len() {
            let menu = strings::menu_label(i, crate::i18n::lang());
            let mw = tr.measure(menu, font::BODY);
            let hit = Rect { x: mx as i32 - 8, y: 0, w: mw as i32 + 16, h: metric::MENUBAR_H };
            let hovered = hit.contains(ui.mouse.0, ui.mouse.1);
            let opened = ui.open_menu == Some(i);
            if opened || hovered {
                let a = if opened { state::hover_strong() } else { state::hover() };
                rounded_rect(buf, w, h, Rect { x: hit.x + 2, y: 4, w: hit.w - 4, h: metric::MENUBAR_H - 8 }, radius::SM, color::hairline(), a);
            }
            draw_text(tr, buf, w, h, mx, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), menu, font::BODY, color::text(), if opened { 1.0 } else { 0.92 });
            self.menubar_menus.push(hit);
            mx += mw + 16.0;
        }

        // 右侧：电池、AI 状态、搜索胶囊、时钟
        // 2026-09-29 P3：时钟改由用户设置驱动（时区偏移 / 24 或 12 小时制 / 是否显示秒），
        // 不再只有"硬编码 +8"一种可能。
        let clock = crate::text::clock_fmt(
            crate::text::now_utc_secs(),
            self.settings.tz_offset_min,
            self.settings.clock_24h,
            self.settings.clock_seconds,
        );
        let clock_w = tr.measure_bold(&clock, font::BODY);
        let clock_x = w as f32 - 16.0 - clock_w;
        tr.draw_bold(buf, w, h, clock_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::BODY), &clock, font::BODY, color::text(), 0.95);

        // 2026-09-29 P2：**删掉"搜索"胶囊**。它只显示"搜索 K"，点下去弹一句 toast ——
        // 一个纯粹的假控件（真正的启动器/搜索是 P4 控制中心的活）。顶栏宁缺毋滥：
        // 现在右侧只剩 AI 状态与时钟，两个都是真实信息。
        // 命令栏（下方）本来就恒聚焦，不需要"点搜索再输入"这一层假仪式。

        // AI 状态指示（§4 三态：本地青 / 云端紫 / 离线灰）
        //
        // 2026-09-30 P4：**这块同时是控制中心的入口**（状态簇）。刻意不额外塞一个图标按钮 ——
        // "状态"与"操作"放同一处正是 Win11/macOS 状态簇的语义，也少一个视觉元素。
        // 走 i18n：`label_en()` 早就写好了，此前调用处只用了 `label()`（英文界面里
        // 状态栏仍是中文 —— 英文实拍发现，审计附录 DL）。
        let ai_label = crate::i18n::pick(
            crate::i18n::lang(),
            ui.ai_status.label(),
            ui.ai_status.label_en(),
        );
        let ai_w = tr.measure(ai_label, font::CAPTION);
        let ai_x = clock_x as f32 - 20.0 - ai_w;
        let cluster = Rect {
            x: ai_x as i32 - 18,
            y: 2,
            w: ai_w as i32 + 32,
            h: metric::MENUBAR_H - 4,
        };
        if cluster.contains(ui.mouse.0, ui.mouse.1) || self.control_open {
            // 悬停/展开时给一层底，用户才知道这里能点
            rounded_rect(buf, w, h, cluster, radius::SM, color::hairline(), state::hover());
        }
        rounded_rect(buf, w, h, Rect { x: ai_x as i32 - 12, y: 13, w: 6, h: 6 }, 3.0, ui.ai_status.color(), 0.95);
        draw_text(tr, buf, w, h, ai_x, tr.vcenter(0.0, metric::MENUBAR_H as f32, font::CAPTION), ai_label, font::CAPTION, if self.control_open { color::text() } else { color::text_dim() }, 0.9);
        self.status_cluster = cluster;

        // 2026-09-29 P2：**删掉假电量图标**。它是画上去的固定 50% 填充，既没有
        // /sys/class/power_supply 数据源也不代表任何状态 —— 桌面上的假信息比没有更糟。
        // 真有电池节点时再按真实读数画（P3/P4 接 ACPI）。
    }

    /// 控制中心（P4）：顶栏状态簇点开的面板。
    ///
    /// **只放真实能用的东西**（本项目对假控件零容忍，P2 已删掉假电量与假搜索胶囊）：
    /// - AI 三态只读展示（数据来自 `ui.ai_status`）
    /// - 中文输入法开关 → `ime.toggle()`（主循环执行，`ime_on` 只负责显示同步）
    /// - 24 小时制开关 → 写 `settings` 并落盘，与设置中心**同一份数据**
    /// - 快捷跳转：设置 / 终端（复用 `open_app` 那条真实路径）
    ///
    /// **故意不做的**：音量与亮度。系统没有音频栈、也没有背光节点 —— 放两个拖不动的滑块
    /// 就是纯装饰。等真有 `aetherd`/`/sys/class/backlight` 能力再加。
    fn draw_control_center(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        use crate::widgets;
        const PANEL_W: i32 = 264;
        // 高度由行数推导（见 control_layout）：加行自动变高，不存在"忘了同步"
        const PANEL_H: i32 = control_layout::PANEL_H;
        // 右对齐到状态簇（并保证不出屏）
        let x = (self.status_cluster.x + self.status_cluster.w - theme::size::px(PANEL_W)).max(8);
        let panel = Rect {
            x,
            y: metric::MENUBAR_H as i32 + 6,
            w: PANEL_W,
            // 面板高度：跟着缩放，但**夹在屏幕可用高度内**（不让它跑出屏幕）。
            // 150% 下内容仍装不下（需滚动或两栏，见审计附录 CZ）——
            // 这里保证的是「面板本身不出屏」，不是「内容一定放得下」。
            h: theme::size::px(PANEL_H).min(
                (h as i32 - (metric::MENUBAR_H as i32 + theme::size::px(6)) - theme::size::px(8))
                    .max(theme::size::px(120)),
            ),
        };
        shadow(buf, w, h, panel, radius::MD, elevation::elev_2());
        rounded_rect(buf, w, h, panel, radius::MD, color::glass(), color::glass_alpha(true));
        rounded_outline(buf, w, h, panel, radius::MD, color::hairline(), 0.22);
        self.control_hits.clear();
        self.control_panel = panel;
        let Some(tr) = tr else { return };
        // 内容裁剪到面板内：滚起来时上半部分不该画到菜单栏上（审计附录 DD）
        tr.set_clip(Some((panel.x, panel.y, panel.x + panel.w, panel.y + panel.h)));

        // 行矩形。**完全在面板之外的行挪到屏幕外**（y = -1000）：
        // 渲染器会跳过屏外像素，于是文字不画、命中区也失效 ——
        // 看不见的行本来就不该能点。这样滚动时内容不会溢出到面板外面。
        let row = |y: i32| {
            // 行高必须 **≥ 步长**，否则相邻行重叠（实测：写死 34 而动作行步长是 32
            // → 100% 下就重叠 2px、150% 下 3px；几何断言报的 14 处就是它）。
            // 用步长常量本身，则「步长 ≥ 行高」对所有行成立：
            // 动作行 32 = 32（不重叠），开关行 36 > 32（留 4px 缝）。
            let h = theme::size::px(control_layout::ROW_H_ACTION);
            let visible = y + h > panel.y && y < panel.y + panel.h;
            Rect {
                x: panel.x + theme::size::px(4),
                y: if visible { y } else { -1000 },
                w: panel.w - theme::size::px(8),
                h,
            }
        };
        // 与设置中心同一手法：**只登记开关本身**，整行与开关都登记会让一次点击命中两个目标
        let knob = |r: Rect| Rect {
            x: r.x + r.w - theme::size::px(54),
            y: r.y + theme::size::px(7),
            w: theme::size::px(46),
            h: theme::size::px(20),
        };

        // 内容从「面板顶 + 留白 - 滚动量」开始：滚轮滚动的是内容，面板本身不动。
    let content_top = panel.y + theme::size::px(theme::size::TIGHT);
    // 夹取滚动量（内容高度是上一帧量出来的；首帧还不知道就别夹，免得把参数抹成 0）
    if self.control_content_h > 0 {
        let max_scroll = (self.control_content_h - panel.h).max(0);
        if self.control_scroll > max_scroll {
            self.control_scroll = max_scroll;
        }
        if self.control_scroll < 0 {
            self.control_scroll = 0;
        }
    }
    let mut y = content_top - self.control_scroll;
        // ① AI 三态（只读）
        rounded_rect(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::INSET_SM), y: y + theme::size::px(6), w: 8, h: 8 }, 4.0, ui.ai_status.color(), 0.95);
        tr.draw(buf, w, h, (panel.x + theme::size::px(32)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), crate::i18n::tr("AI 中枢", "AI core"), font::CAPTION, color::text(), 0.9);
        tr.draw(buf, w, h, (panel.x + theme::size::px(120)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), crate::i18n::pick(crate::i18n::lang(), ui.ai_status.label(), ui.ai_status.label_en()),
            font::CAPTION, color::text_dim(), 0.9);
        y += theme::size::px(26);
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - theme::size::px(24), h: 1 });
        y += 8;

        // ② 深色模式（默认关 = 白色；用户最常切的一项放最前）
        let r0 = row(y);
        widgets::row(buf, w, h, r0, crate::i18n::tr("深色模式", "Dark mode"), None, widgets::States::at(r0, ui.mouse), Some(tr));
        let s0 = knob(r0);
        widgets::switch(buf, w, h, s0, self.settings.dark_mode, widgets::States::at(s0, ui.mouse));
        self.control_hits.push((s0, ControlHit::ToggleDark));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ③ 中文输入法
        let r1 = row(y);
        widgets::row(buf, w, h, r1, crate::i18n::tr("中文输入法", "Chinese IME"), None, widgets::States::at(r1, ui.mouse), Some(tr));
        let s1 = knob(r1);
        widgets::switch(buf, w, h, s1, self.ime_on, widgets::States::at(s1, ui.mouse));
        self.control_hits.push((s1, ControlHit::ToggleIme));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ③ 24 小时制
        let r2 = row(y);
        widgets::row(buf, w, h, r2, crate::i18n::tr("24 小时制", "24-hour clock"), None, widgets::States::at(r2, ui.mouse), Some(tr));
        let s2 = knob(r2);
        widgets::switch(buf, w, h, s2, self.settings.clock_24h, widgets::States::at(s2, ui.mouse));
        self.control_hits.push((s2, ControlHit::ToggleClock24h));
        y += theme::size::px(control_layout::ROW_H_SWITCH);

        // ④ 快捷跳转
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("快捷跳转", "Quick actions"), Some(tr));
        y += theme::size::px(20);
        // 「通知」带**真实未读数**（不是装饰性的小红点）：没有未读时不显示数字，
        // 因为"0 条未读"和"没有通知中心"在界面上应该长得不一样。
        let unread = self.notifs.unread();
        // 三元组：中文 / English / 命中。**两种语言写在一起**，因此不可能漏翻；
        // 并且有覆盖性测试盯着这张表（`every_control_center_entry_is_bilingual`）。
        for (zh, en, hit) in [
            ("打开设置", "Open Settings", ControlHit::OpenSettings),
            ("打开终端", "Open Terminal", ControlHit::OpenTerminal),
        ] {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }
        // 「通知」单独走一支：它的文案**带未读数**（动态），
        // 而 `i18n::pick` 要求 `&'static str`（那是"两种语言写在调用点"这个设计的代价）。
        // 动态文案用语言分支写，同样两种语言并排 —— 只是换了个形式。
        {
            let label = if unread > 0 {
                if crate::i18n::lang() == crate::i18n::Lang::En {
                    format!("Notifications ({unread})")
                } else {
                    if crate::i18n::is_en() {
                        format!("Notifications ({unread} unread)")
                    } else {
                        format!("通知（{unread} 条未读）")
                    }
                }
            } else {
                crate::i18n::tr("通知", "Notifications").to_string()
            };
            let r = row(y);
            widgets::row(buf, w, h, r, &label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, ControlHit::Notifications));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑤ 音量（批次 K-2）
        //
        // **不可用时不画滑块**（本项目对假控件零容忍）：只显示一行如实的状态文案，
        // 且不登记命中区 —— "画一个拖不动的滑块"比"没有滑块"更糟。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("声音", "Sound"), Some(tr));
        y += theme::size::px(20);
        {
            let r = row(y);
            if ui.audio_enabled {
                widgets::row(buf, w, h, r, crate::i18n::tr("音量", "Volume"), Some(&format!("{}%", ui.audio_volume)),
                             widgets::States::at(r, ui.mouse), Some(tr));
                // 轨道：底槽 + 已填充部分。点击整条轨道即可设定（不需要拖拽，
                // 也就不需要给事件循环加"拖拽状态"——少一处状态就少一类 bug）
                let track = Rect { x: r.x + theme::size::px(theme::size::TIGHT), y: r.y + r.h - 12, w: r.w - 24, h: 6 };
                rounded_rect(buf, w, h, track, 3.0, color::surface_3(), 0.9);
                let fill_w = (track.w as f32 * ui.audio_volume as f32 / 100.0) as i32;
                if fill_w > 0 {
                    rounded_rect(buf, w, h, Rect { x: track.x, y: track.y, w: fill_w, h: track.h },
                                 3.0, color::accent(), 0.95);
                }
                self.control_hits.push((track, ControlHit::VolumeTrack));
            } else {
                widgets::row(buf, w, h, r, crate::i18n::tr("音量", "Volume"), Some(ui.audio_label),
                             widgets::States::at(r, ui.mouse), Some(tr));
                // **不登记命中区**：不可用时点不动，也就不该有可点击区域
            }
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑦ 虚拟桌面（§5.4）：把"切"之外的"增删"也放到手边
        //
        // 为什么放在控制中心而不是再占一个快捷键：`Super+1..9` 已经是"切到第 N 个"，
        // 再给增删各配一个组合键既难记又容易误触；而"给桌面加一个"是低频动作，
        // 收进控制中心正合适。行数由 `theme::size::px(control_layout::SPACE_ROWS)` 决定（面板自动变高）。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(
            buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y,
            &format!(
                "{}（{}）",
                crate::i18n::tr("虚拟桌面", "Virtual desktops"),
                self.space_count
            ),
            Some(tr),
        );
        y += theme::size::px(20);
        for (zh, en, hit) in [
            ("新增桌面", "New desktop", ControlHit::AddSpace),
            ("移除当前桌面", "Remove this desktop", ControlHit::RemoveSpace),
        ] {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }

        // ⑥ 电源（2026-10-02 功能批次 J-2）
        //
        // 放在最下面而不是最上面：关机/重启是**低频且不可逆**的动作，
        // 放在开关旁边会让人手滑点错（Windows/macOS 也把它收在菜单末尾）。
        widgets::divider(buf, w, h, Rect { x: panel.x + theme::size::px(theme::size::TIGHT), y, w: panel.w - 24, h: 1 });
        y += 8;
        widgets::group_header(buf, w, h, panel.x + theme::size::px(theme::size::INSET_SM), y, crate::i18n::tr("电源", "Power"), Some(tr));
        y += theme::size::px(20);
        let power: [(&str, &str, ControlHit); 3] = [
            ("关机", "Power off", ControlHit::PowerOff),
            ("重启", "Restart", ControlHit::Reboot),
            ("注销", "Log out", ControlHit::Logout),
        ];
        for (zh, en, hit) in power {
            let r = row(y);
            let label = crate::i18n::pick(crate::i18n::lang(), zh, en);
            widgets::row(buf, w, h, r, label, None, widgets::States::at(r, ui.mouse), Some(tr));
            self.control_hits.push((r, hit));
            y += theme::size::px(control_layout::ROW_H_ACTION);
        }
        // 量出内容实际高度（供滚动夹取）。页脚**钉在面板底**，不参与滚动。
        self.control_content_h =
            (y - content_top).max(0) + theme::size::px(control_layout::FOOTER);
        tr.draw(
            buf, w, h,
            (panel.x + theme::size::px(theme::size::INSET_SM)) as f32,
            (panel.y + panel.h - theme::size::px(22)) as f32,
            crate::i18n::tr("Esc 或点击别处关闭", "Esc or click elsewhere to close"),
            font::LABEL, color::text_dim(), 0.7,
        );
        tr.set_clip(None);
    }

    /// 向导里被点中的**选项下标**（§5.8）。没有命中给 `None`。
    ///
    /// 与 `palette_hit` 同一做法：命中区由绘制产生，
    /// 所以**只有真的画出来的按钮才能被点到**（画不下就不画、也就不登记）。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn setup_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.setup_hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, idx)| *idx)
    }

    /// 命令面板里被点中的**条目下标**（§5.5）。没有命中给 `None`。
    ///
    /// 用 `palette_hits`（上一帧绘制时登记的）来判断 —— 这是"命中区由绘制产生"的
    /// 一贯做法（桌面图标、设置项、控制中心都这样），好处是**只有真的画出来的东西才能被点到**。
    /// 调用点在 Linux 的 fbdev 循环里；非 Linux 上只有测试用它 —— 按平台 allow。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn palette_hit(&self, x: f32, y: f32) -> Option<usize> {
        self.palette_hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, item)| *item)
    }

    /// 展开中的下拉菜单（登记各项命中区，悬停高亮）。
    fn draw_dropdown(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, tr: Option<&TextRenderer>) {
        let Some(mi) = ui.open_menu else { return };
        let Some(label) = self.menubar_menus.get(mi) else { return };
        let Some(tr) = tr else { return };
        // §3.5：下拉项同样双语；**数量以中文表为准**（两张表形状有测试钉住）
        let lang = crate::i18n::lang();
        let items: Vec<&str> = (0..strings::menu_item_count(mi))
            .map(|j| strings::menu_item(mi, j, lang))
            .collect();
        let panel_w = items
            .iter()
            .map(|s| tr.measure(s, font::BODY))
            .fold(120.0f32, f32::max)
            + 44.0;
        let panel = Rect { x: label.x, y: metric::MENUBAR_H + theme::size::px(4), w: panel_w as i32, h: items.len() as i32 * theme::size::px(30) + theme::size::px(8) };
        shadow(buf, w, h, panel, radius::MD, elevation::elev_2());
        rounded_rect(buf, w, h, panel, radius::MD, color::surface_2(), 0.98);
        rounded_outline(buf, w, h, panel, radius::MD, color::hairline(), 0.12);

        let mut rects = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let ir = Rect { x: panel.x + 4, y: panel.y + theme::size::px(4) + i as i32 * theme::size::px(30), w: panel.w - 8, h: theme::size::px(theme::size::CTRL_H_SM) };
            if ir.contains(ui.mouse.0, ui.mouse.1) {
                rounded_rect(buf, w, h, Rect { x: ir.x + 4, y: ir.y + 3, w: ir.w - 8, h: ir.h - 6 }, radius::SM, color::surface_3(), 0.9);
            }
            draw_text(tr, buf, w, h, (ir.x + theme::size::px(theme::size::INSET_SM)) as f32, tr.vcenter(ir.y as f32, ir.h as f32, font::BODY), item, font::BODY, color::text(), 0.92);
            rects.push(ir);
        }
        self.dropdown = Some((rects, items.to_vec()));
    }
}

// ---------------------------------------------------------------------------
// 窗口：磨砂深灰 + 发丝描边 + 红绿灯（左）+ 居中标题
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_ai_bar(&mut self, buf: &mut [u32], w: usize, h: usize, ui: &UiState, t: f32, tr: Option<&TextRenderer>) {
    let input = ui.ai_input;
    let mouse = ui.mouse;
    let bar_w = 560i32;
    let bar = Rect { x: w as i32 / 2 - bar_w / 2, y: h as i32 - metric::BOTTOM_DOCK - theme::size::px(theme::size::AI_BAR_H) - theme::size::px(theme::size::INSET_SM), w: bar_w, h: theme::size::px(theme::size::AI_BAR_H) };
    let hovered = bar.contains(mouse.0, mouse.1);
    shadow(buf, w, h, bar, radius::LG, elevation::elev_2());
    // 2026-09-29 P2：从"26px 大胶囊"改成**命令栏形态**（radius::LG = 8px，
    // 与 Windows 11 搜索框/输入框同一规范）。胶囊 + 渐变环是最典型的 AI 产品观感。
    rounded_rect(buf, w, h, bar, radius::LG, color::glass(), color::glass_alpha(hovered));
    if ui.ai_focused {
        // 焦点环：**只在思考中脉动**（那是真实状态），空闲时是静态环 —— 此前无条件呼吸，
        // 属于纯装饰动效。
        let ring = if ui.ai_thinking {
            0.45 + 0.25 * (t * 3.0).sin()
        } else {
            0.65
        };
        gradient_outline(buf, w, h, bar, radius::LG, color::accent(), color::accent(), ring);
    } else {
        rounded_outline(buf, w, h, bar, radius::LG, color::hairline(), if hovered { 0.22 } else { 0.14 });
    }

    let Some(tr) = tr else { return };

    // 渐变圆形徽标（AI 元素：极光的容许出口之一）
    let av = Rect { x: bar.x + theme::size::px(theme::size::TIGHT), y: bar.y + 10, w: 32, h: 32 };
    rounded_rect(buf, w, h, av, radius::MD, color::accent(), 0.95);
    let aw = tr.measure_bold("A", font::GLYPH);
    tr.draw_bold(buf, w, h, av.x as f32 + (av.w as f32 - aw) / 2.0, tr.vcenter(av.y as f32, av.h as f32, font::GLYPH), "A", font::GLYPH, color::text(), 0.98);

    let text_x = (bar.x + theme::size::px(58)) as f32;
    let text_y = tr.vcenter(bar.y as f32, bar.h as f32, font::BODY);
    // 可见窗口按**光标**位置算，而不是"保证末尾可见" —— 2.4 支持中间编辑后，
    // 光标可能在任意位置，"末尾可见"对用户没有意义
    let max_w = bar.x as f32 + bar.w as f32 - 96.0 - text_x;
    let (shown, prefix) = visible_window(tr, input, ui.ai_cursor, max_w);
    if input.is_empty() {
        draw_text(
            tr,
            buf,
            w,
            h,
            text_x,
            text_y,
            crate::i18n::pick(crate::i18n::lang(), strings::AI_BAR_HINT, strings::AI_BAR_HINT_EN),
            font::BODY,
            color::text_dim(),
            0.85,
        );
    } else {
        draw_text(tr, buf, w, h, text_x, text_y, shown, font::BODY, color::text(), 0.95);
    }

    // 输入光标：x 由"光标前那一段"的宽度决定
    let caret_x = text_x + tr.measure(prefix, font::BODY) + 4.0;

    // 输入法候选框（2.3）：画在指令条**上方** —— 贴着输入位置，视线不用来回跳
    if ui.ime.show_candidates() {
        draw_ime_candidates(buf, w, h, bar, ui.ime, tr);
    }
    if ui.ai_thinking {
        // 思考中：三点呼吸，明确"在等我"而不是"在等你打字"
        for i in 0..3 {
            let phase = (t * 3.0 - i as f32 * 0.5).sin() * 0.5 + 0.5;
            let a = 0.25 + 0.6 * phase;
            let cx = caret_x as i32 + i * 8;
            rounded_rect(buf, w, h, Rect { x: cx, y: bar.y + theme::size::px(23), w: 5, h: 5 }, 2.5, color::accent(), a);
        }
    // 用**传入的 `t`**，不用 `t_now()`：渲染必须是输入的纯函数，
        // 否则同一个状态会画出不同的画面（走查图与像素回归都做不了）。
        // 与本函数上面算焦点环脉动用的是同一个时钟。
        } else if ((t * 2.0) as i32) % 2 == 0 {
        fill_rect(buf, w, h, Rect { x: caret_x as i32, y: bar.y + 16, w: 2, h: 20 }, color::accent(), 0.9);
    }

    // 右侧快捷键胶囊
    let hint = "Enter";
    let hw = tr.measure(hint, font::LABEL) + 12.0;
    let key = Rect { x: bar.x + bar.w - hw as i32 - 12, y: bar.y + 15, w: hw as i32, h: 22 };
    rounded_rect(buf, w, h, key, 6.0, color::hairline(), 0.10);
    draw_text(tr, buf, w, h, (key.x + 6) as f32, tr.vcenter(key.y as f32, key.h as f32, font::LABEL), hint, font::LABEL, color::text_dim(), 0.8);
    }
}

impl Renderer {
    /// Dock：磨砂托盘 + 中性玻璃图标 + 运行指示点（真实反映已打开窗口）。
    /// Live ISO 会话（ui.show_installer）在末尾附加"安装"图标。
    ///
    /// 图标底座走中性灰阶（§1：极光只留给壁纸、AI 元素、品牌标识），
    /// 区分度由造型与状态（hover 提亮、运行中青色微光）承担，不再用彩色拟物色块。
    /// 桌面图标：默认桌面**不开任何窗口**，图标是打开应用的主要入口（Windows/macOS 的桌面语义）。
    ///
    /// 只放**真实能打开**的三项；图形与 Dock 共用 [`draw_app_icon`]。
    /// 图标贴在左侧竖排（Windows 的桌面图标就是左侧竖列），顶部避开顶栏。
    fn draw_desktop_icons(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        ui: &UiState,
        tr: Option<&TextRenderer>,
    ) {
        const ICON: i32 = 48;
        const CELL_W: i32 = 96;
        const CELL_H: i32 = ICON + 34;
        self.desktop_icons.clear();
        let x = 26;
        let mut y = metric::MENUBAR_H as i32 + 20;
        // 序号与 `open_app` 的映射一致：0 文件 / 1 终端 / 4 设置
        for (idx, name) in [
            (0usize, strings::win_label(strings::WIN_FILES)),
            (1, strings::win_label(strings::WIN_TERM)),
            (4, strings::win_label(strings::WIN_SETTINGS)),
        ] {
            let cell = Rect { x: x - 12, y: y - 6, w: CELL_W, h: CELL_H };
            let tile = Rect { x, y, w: ICON, h: ICON };
            let hovered = cell.contains(ui.mouse.0, ui.mouse.1);
            // 悬停反馈：让"能点"这件事可见（浅底 + accent 描边）
            if hovered {
                rounded_rect(buf, w, h, cell, radius::SM, color::hairline(), 0.12);
                rounded_outline(buf, w, h, cell, radius::SM, color::accent(), 0.35);
            }
            draw_app_icon(buf, w, h, tile, idx, false, false, hovered, false);
            // 标签：深色文字（浅色壁纸上是白底黑字，深色主题下 color::text() 自动转浅）
            if let Some(tr) = tr {
                let tw = tr.measure(name, font::LABEL);
                let tx = tile.x + ((ICON as f32 - tw) / 2.0) as i32;
                tr.draw(buf, w, h, tx as f32, (y + ICON + 6) as f32, name, font::LABEL, color::text(), 0.95);
            }
            self.desktop_icons.push((cell, idx));
            y += CELL_H;
        }
    }

    fn draw_dock(&mut self, buf: &mut [u32], w: usize, h: usize, open_titles: &[&str], ui: &UiState, _tr: Option<&TextRenderer>) {
        let mut apps: Vec<&str> = vec![
            strings::WIN_FILES,
            strings::WIN_TERM,
            strings::WIN_BROWSER,
            strings::WIN_MUSIC,
            strings::WIN_SETTINGS,
        ];
        // 已装应用插在**内建图标与"安装向导"之间** —— 安装向导必须留在最后，
        // 因为 Live ISO 里它是唯一需要被一眼找到的动作。
        // 数据来自 main.rs::load_installed_apps（扫 /var/apps）。
        apps.extend(self.installed_apps.iter().map(|(_, name)| name.as_str()));
        if ui.show_installer {
            apps.push(strings::INSTALLER);
        }
        let n = apps.len() as i32;
        let tray_w = n * metric::DOCK_ICON + (n + 1) * metric::DOCK_PAD;
        let tray_h = metric::DOCK_ICON + 2 * metric::DOCK_PAD;
        let tray = Rect { x: w as i32 / 2 - tray_w / 2, y: h as i32 - metric::BOTTOM_DOCK, w: tray_w, h: tray_h };
        shadow(buf, w, h, tray, radius::MD, elevation::elev_2());
        // 走 glass_alpha()：明亮模式必须够白（0.68 会让粉彩壁纸透上来发脏，
        // 与文档声称的"悬浮层 0.95"也不符），深色模式则是比窗口更暗的底座（P3-27）
        rounded_rect(
            buf,
            w,
            h,
            tray,
            20.0,
            color::glass(),
            color::glass_alpha(tray.contains(ui.mouse.0, ui.mouse.1)),
        );
        rounded_outline(buf, w, h, tray, 20.0, color::hairline(), 0.14);
        // 托盘顶部内高光（玻璃厚度）
        fill_rect(buf, w, h, Rect { x: tray.x + theme::size::px(theme::size::INSET_SM), y: tray.y + 1, w: tray.w - 32, h: 1 }, color::HIGHLIGHT, 0.10);

        self.dock_icons.clear();
        for (i, name) in apps.iter().enumerate() {
            let ix = tray.x + metric::DOCK_PAD + i as i32 * (metric::DOCK_ICON + metric::DOCK_PAD);
            let iy = tray.y + metric::DOCK_PAD;
            let tile = Rect { x: ix, y: iy, w: metric::DOCK_ICON, h: metric::DOCK_ICON };
            self.dock_icons.push(tile);
            let hovered = tile.contains(ui.mouse.0, ui.mouse.1);
            let running = open_titles.contains(name);

            // 底座 + 白色几何符号：与**桌面图标共用**同一函数（两处各画一套必然走形）
            draw_app_icon(buf, w, h, tile, i, i >= DOCK_BUILTINS, *name == strings::INSTALLER, hovered, running);

            // 运行指示点：真实反映打开的窗口（强调色，品牌点缀）
            if running {
                rounded_rect(buf, w, h, Rect { x: ix + metric::DOCK_ICON / 2 - 1, y: iy + metric::DOCK_ICON + 5, w: 3, h: 3 }, 1.5, color::accent(), 0.9);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 软件光标（fbdev 无硬件指针；白箭头 + 黑描边，指向左上）
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_installer(&mut self, buf: &mut [u32], w: usize, h: usize, inst: &InstallerUi, mouse: (f32, f32), mouse_down: bool, tr: Option<&TextRenderer>) {
    // §3.5：安装器界面文案双语
    let lang = crate::i18n::lang();
        let win_w = 640i32;
        let win_h = 400i32;
        let win = Rect {
            x: w as i32 / 2 - win_w / 2,
            y: ((h as i32 - win_h) / 2 - 30).max(56),
            w: win_w,
            h: win_h,
        };
        shadow(buf, w, h, win, radius::LG, elevation::elev_3());
        rounded_rect(buf, w, h, win, radius::LG, color::surface_2(), 0.98);
        rounded_outline(buf, w, h, win, radius::LG, color::hairline(), 0.16);
        let Some(tr) = tr else { return };

        // 标题栏（TITLE 20px 在 36px 栏内垂直居中）
        tr.draw_bold(buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, tr.vcenter(win.y as f32, metric::TITLE_H as f32, font::TITLE), "安装 AetherOS", font::TITLE, color::text(), 0.95);
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + metric::TITLE_H, w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 关闭按钮（右上角，与窗口标题栏三键同一套视觉）。
        // **安装进行中禁用**：写到一半取消是危险操作，不该给这个按钮。
        let cbtn = Rect {
            x: win.x + win.w - metric::CAPTION_BTN_W,
            y: win.y,
            w: metric::CAPTION_BTN_W,
            h: metric::TITLE_H,
        };
        let running = matches!(inst.phase, InstallerPhase::Running);
        let chover = !running && cbtn.contains(mouse.0, mouse.1);
        if chover {
            fill_rect(buf, w, h, cbtn, color::close_hover(), 1.0);
        }
        caption_glyph(
            buf, w, h, cbtn, 2,
            if chover {
                color::HIGHLIGHT
            } else if running {
                color::text_dim()
            } else {
                color::text()
            },
            if running { 0.35 } else { 0.9 },
        );
        self.installer_close = cbtn;

        // 提示行
        draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, (win.y + 52) as f32,
                  crate::i18n::pick(lang, "选择目标磁盘并确认。整盘覆盖，目标盘上的数据将丢失。", "Pick a target disk and confirm. The whole disk is erased; its data will be lost."), font::CAPTION, color::text_dim(), 0.9);

        // 磁盘行
        self.installer_rows.clear();
        let row_y0 = win.y + theme::size::px(78);
        for (i, (dev, mb)) in inst.disks.iter().enumerate() {
            let r = Rect { x: win.x + theme::size::px(theme::size::INSET_SM), y: row_y0 + i as i32 * 50, w: win.w - 32, h: theme::size::px(theme::size::CTRL_H) };
            if inst.selected == i {
                rounded_rect(buf, w, h, r, radius::MD, color::accent(), 0.16);
                rounded_outline(buf, w, h, r, radius::MD, color::accent(), 0.45);
                rounded_rect(buf, w, h, Rect { x: r.x + theme::size::px(theme::size::TIGHT), y: r.y + 16, w: 12, h: 12 }, 6.0, color::accent(), 0.95);
            } else {
                rounded_rect(buf, w, h, r, radius::MD, color::hairline(), 0.05);
                rounded_outline(buf, w, h, r, radius::MD, color::hairline(), 0.10);
                rounded_outline(buf, w, h, Rect { x: r.x + 11, y: r.y + 15, w: 14, h: 14 }, 7.0, color::text_dim(), 0.7);
            }
            tr.draw_bold(buf, w, h, (r.x + theme::size::px(theme::size::INDENT_SUB)) as f32, tr.vcenter(r.y as f32, r.h as f32, font::BODY), dev, font::BODY, color::text(), 0.92);
            let sz = if crate::i18n::is_en() {
                format!("{mb} MB free")
            } else {
                format!("{mb} MB 可用")
            };
            let sw = tr.measure(&sz, font::CAPTION);
            draw_text(tr, buf, w, h, (r.x + r.w - 16) as f32 - sw, tr.vcenter(r.y as f32, r.h as f32, font::CAPTION), &sz, font::CAPTION, color::text_dim(), 0.8);
            self.installer_rows.push((r, dev.clone(), *mb));
        }

        // 阶段信息行
        let msg_y = (win.y + win_h - 104) as f32;
        match (inst.phase, inst.message) {
            (InstallerPhase::Running, _) => {
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, crate::i18n::pick(lang, "正在写入磁盘，请勿关机…", "Writing to disk, do not power off…"), font::BODY, color::accent(), 0.95);
            }
            (InstallerPhase::Done, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, &shown, font::CAPTION, color::success(), 0.95);
            }
            (InstallerPhase::Failed, Some(m)) => {
                let mut shown: String = m.to_string();
                while tr.measure(&shown, font::CAPTION) > (win.w - 36) as f32 && shown.chars().count() > 4 {
                    shown.remove(0);
                }
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, &shown, font::CAPTION, color::danger(), 0.95);
            }
            _ => {
                draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::CONTROL_INSET)) as f32, msg_y, crate::i18n::pick(lang, "就绪。也可以选中磁盘后按 Enter 开始。", "Ready. You can also pick a disk and press Enter to start."), font::CAPTION, color::text_dim(), 0.85);
            }
        }

        // 主按钮（四状态：默认 / 悬停 / 按下 / 禁用；禁用以状态色而非灰表达语义）
        let button = Rect { x: win.x + win.w / 2 - 160, y: win.y + win_h - 62, w: 320, h: theme::size::px(theme::size::CTRL_H) };
        // 四状态按钮：先取 (中文, English)，再按语言挑 —— 比写两遍 match 短，
        // 也不会出现"新增一个阶段却只加了一种语言"
        let label = {
            let (zh, en) = match inst.phase {
                InstallerPhase::Idle => ("开始安装（整盘覆盖）", "Install (erase disk)"),
                InstallerPhase::Running => ("安装中…", "Installing…"),
                InstallerPhase::Done => ("完成 · 重启后从磁盘引导", "Done · boots from disk after reboot"),
                InstallerPhase::Failed => ("重试安装", "Retry install"),
            };
            crate::i18n::pick(lang, zh, en)
        };
        let enabled = matches!(inst.phase, InstallerPhase::Idle | InstallerPhase::Failed);
        let hovered = enabled && button.contains(mouse.0, mouse.1);
        let pressed = hovered && mouse_down;
        if enabled {
            // 与确认弹窗一致：accent() 直接做底 + text() 做字实测只有深色 2.07:1，
            // 读不清"我要开始擦盘了"这个按钮。改用加深的 accent_strong + 纯白字。
            rounded_rect(buf, w, h, button, radius::SM, color::accent_strong(), 0.95);
            if hovered {
                rounded_rect(buf, w, h, button, radius::SM, color::hairline(), state::hover());
            }
            if pressed {
                // 按下是压暗（叠黑），不是降低底色——后者会透出背景显得像变淡
                rounded_rect(buf, w, h, button, radius::SM, [0, 0, 0], state::pressed());
            }
        } else if matches!(inst.phase, InstallerPhase::Done) {
            rounded_rect(buf, w, h, button, radius::SM, color::success(), 0.85);
        } else {
            rounded_rect(buf, w, h, button, radius::SM, color::surface_3(), 0.9);
        }
        let (label_rgb, label_a) = if enabled {
            (color::HIGHLIGHT, 0.98)
        } else if matches!(inst.phase, InstallerPhase::Done) {
            // success 底在明亮模式下是深绿，配白字约 4.3:1，比原来的 text() 好得多
            (color::HIGHLIGHT, 0.95)
        } else {
            (color::text_faint(), state::DISABLED)
        };
        let lw = tr.measure_bold(label, font::BODY);
        let ly = tr.vcenter(button.y as f32, button.h as f32, font::BODY);
        tr.draw_bold(buf, w, h, (button.x + button.w / 2) as f32 - lw / 2.0, ly, label, font::BODY, label_rgb, label_a);
        self.installer_button = button;
    }
}

// ---------------------------------------------------------------------------
// L2+ 权限确认弹窗（模态；安装向导的"开始安装"也走这里）
// ---------------------------------------------------------------------------

impl Renderer {
    fn draw_confirm(
        &mut self,
        buf: &mut [u32],
        w: usize,
        h: usize,
        c: &ConfirmUi,
        mouse: (f32, f32),
        mouse_down: bool,
        t: f32,
        tr: Option<&TextRenderer>,
    ) {
        self.confirm_buttons.clear();
        self.confirm_echo = Rect { x: 0, y: 0, w: 0, h: 0 };

        // 高度按内容自适应：标题区 + 参数行 + 后果说明 + 可选回显区 + 按钮区
        let args: Vec<(String, String)> = c
            .arguments
            .iter()
            .map(|(k, v)| {
                // 头尾都留：只留头部时，"良性前缀 + 第 25 字符起的恶意值"会完整
                // 躲过用户的视线。截断处用省略号明确标出"这里被砍过"（P2-18）。
                let shown = if v.chars().count() > 34 {
                    let head: String = v.chars().take(20).collect();
                    let tail: String = {
                        let rev: Vec<char> = v.chars().rev().take(10).collect();
                        rev.into_iter().rev().collect()
                    };
                    format!("{head}…{tail}")
                } else {
                    v.clone()
                };
                (k.clone(), shown)
            })
            .collect();
        let echo_block = if c.echo_required.is_some() { 96 } else { 0 };
        let win_w = 560i32;
        // 高度由「数量 × 每行高度 + 固定块」算出：**每一项都要跟着缩放**，
    // 否则 150% 下参数行长大、窗口不长 → 内容被切（附录 DA）。
    let win_h = theme::size::px(104)
        + args.len() as i32 * theme::size::px(22)
        + theme::size::px(54)
        + echo_block
        + theme::size::px(68);
        let win = Rect {
            x: w as i32 / 2 - win_w / 2,
            y: ((h as i32 - win_h) / 2 - 10).max(48),
            w: win_w,
            h: win_h,
        };

        // 模态遮罩：压暗背景，明确"必须先回答这个"
        fill_rect(buf, w, h, Rect { x: 0, y: 0, w: w as i32, h: h as i32 }, color::scrim(), color::scrim_alpha());
        shadow(buf, w, h, win, radius::LG, elevation::elev_3());
        rounded_rect(buf, w, h, win, radius::LG, color::surface_2(), 0.99);
        rounded_outline(buf, w, h, win, radius::LG, color::hairline(), 0.18);
        let Some(tr) = tr else { return };

        // 标题行：等级徽章（L2 黄 / L3 红）+ 操作名
        let badge_rgb = c.badge_color();
        let badge_text_rgb = c.badge_text_color();
        let badge_text = if crate::i18n::is_en() {
            format!(
                "L{} {}",
                c.level,
                if c.level >= 3 { "danger" } else { "sensitive write" }
            )
        } else {
            format!("L{} {}", c.level, if c.level >= 3 { "危险" } else { "敏感写" })
        };
        let bw = tr.measure_bold(&badge_text, font::LABEL) + 18.0;
        let badge = Rect { x: win.x + theme::size::px(theme::size::PAD_LG), y: win.y + 20, w: bw as i32, h: 22 };
        // 底色压到 0.12：再浓就会把文字对比度拖到 AA 以下（P2-20 同类）
        rounded_rect(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.12);
        rounded_outline(buf, w, h, badge, radius::SM - 2.0, badge_rgb, 0.7);
        draw_text(tr, buf, w, h, (badge.x + 9) as f32, tr.vcenter(badge.y as f32, badge.h as f32, font::LABEL), &badge_text, font::LABEL, badge_text_rgb, 1.0);
        tr.draw_bold(
            buf, w, h,
            (badge.x + badge.w + 12) as f32,
            tr.vcenter(win.y as f32 + theme::size::px(18) as f32, theme::size::px(26) as f32, 17.0),
            tool_label(c.tool, crate::i18n::is_en()), 17.0, color::text(), 0.96,
        );
        fill_rect(buf, w, h, Rect { x: win.x + 1, y: win.y + theme::size::px(58), w: win.w - 2, h: 1 }, color::hairline(), 0.08);

        // 参数明文（逐项原样展示：确认的前提是看清对象）
        let mut y = win.y + theme::size::px(74);
        draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION),
            crate::i18n::pick(crate::i18n::lang(), "工具", "Tool"), font::CAPTION, color::text_faint(), 0.9);
        draw_text(tr, buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::BODY), c.tool, font::BODY, color::text_dim(), 0.95);
        y += theme::size::px(22);
        for (k, v) in &args {
            draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), k, font::CAPTION, color::text_faint(), 0.9);
            tr.draw_bold(buf, w, h, (win.x + 76) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::BODY), v, font::BODY, color::text(), 0.96);
            y += theme::size::px(22);
        }

        // 后果说明（风险色）+ 警示三角（几何绘制，不依赖字体字形）
        y += 10;
        let cx = win.x + theme::size::px(27);
        let cy = y + 10;
        for row in 0..9i32 {
            let half = row / 2;
            fill_rect(buf, w, h, Rect { x: cx - half, y: cy - 8 + row, w: half * 2 + 1, h: 1 }, badge_rgb, 0.95);
        }
        // 保留开头 + 省略号：后果说明的关键信息在句首（"整盘覆盖写入…不可恢复"），
        // 从尾部静默截断会把最要紧的半句吃掉（P3-23）。
        // 文字用主文字色：彩色文字在淡底上达不到 AA；警示语义由左侧三角与徽章承担。
        let shown = ellipsize(tr, c.consequence, font::CAPTION, (win.w - 64) as f32);
        draw_text(tr, buf, w, h, (win.x + 42) as f32, tr.vcenter(y as f32, theme::size::px(20) as f32, font::CAPTION), &shown, font::CAPTION, color::text(), 0.95);
        y += theme::size::px(34);

        // L3 回显确认：必须原样输入目标，防"手滑点确认"
        if let Some(target) = c.echo_required {
            let tip = if crate::i18n::is_en() {
            format!("L3 dangerous action: type {target} exactly to confirm")
        } else {
            format!("L3 危险操作：请原样输入 {target} 以确认")
        };
            draw_text(tr, buf, w, h, (win.x + theme::size::px(theme::size::PAD_LG)) as f32, y as f32, &tip, font::CAPTION, color::text_dim(), 0.9);
            y += theme::size::px(22);
            let field = Rect { x: win.x + theme::size::px(theme::size::PAD_LG), y, w: win.w - 40, h: 38 };
            let ok = c.echo_ok();
            rounded_rect(buf, w, h, field, radius::SM, color::inset(), 0.9);
            rounded_outline(buf, w, h, field, radius::SM, if ok { color::success() } else { color::hairline() }, if ok { 0.75 } else { 0.18 });
            let fty = tr.vcenter(field.y as f32, field.h as f32, font::BODY);
            if c.echo_input.is_empty() {
                let hint = format!("输入 {target}");
                draw_text(tr, buf, w, h, (field.x + theme::size::px(theme::size::TIGHT)) as f32, fty, &hint, font::BODY, color::text_faint(), 0.8);
            } else {
                draw_text(tr, buf, w, h, (field.x + theme::size::px(theme::size::TIGHT)) as f32, fty, c.echo_input, font::BODY, color::text(), 0.96);
            }
            let tw = tr.measure(c.echo_input, font::BODY);
            if ((t * 2.0) as i32) % 2 == 0 {
                fill_rect(buf, w, h, Rect { x: (field.x + theme::size::px(theme::size::TIGHT)) as i32 + tw as i32, y: field.y + 10, w: 2, h: 18 }, color::accent(), 0.9);
            }
            self.confirm_echo = field;
            y += theme::size::px(50);
        }

        // 按钮：拒绝（danger 描边）/ 允许一次（accent 实心，回显未匹配时禁用）。
        // 不做"永久允许"：安全决策，计划明确不引入。
        let bw2 = 150i32;
        let bh = 42i32;
        let by = y.max(win.y + win_h - 62);
        let deny = Rect { x: win.x + win.w - 20 - bw2 * 2 - 12, y: by, w: bw2, h: bh };
        let allow = Rect { x: win.x + win.w - 20 - bw2, y: by, w: bw2, h: bh };

        // 拒绝按钮：透明底 + 高对比红字。悬停只加轻填充（0.14 的填充会把
        // 文字对比度压到 4.1:1 以下），按下才给更明显的反馈。
        let deny_hover = deny.contains(mouse.0, mouse.1);
        let deny_fill = if deny_hover && mouse_down { 0.14 } else { 0.0 };
        if deny_fill > 0.0 {
            rounded_rect(buf, w, h, deny, radius::SM, color::danger(), deny_fill);
        }
        rounded_outline(buf, w, h, deny, radius::SM, color::danger(), if deny_hover { 0.85 } else { 0.6 });
        let dw = tr.measure_bold("拒绝", font::BODY);
        tr.draw_bold(buf, w, h, (deny.x + deny.w / 2) as f32 - dw / 2.0, tr.vcenter(deny.y as f32, deny.h as f32, font::BODY), "拒绝", font::BODY, color::danger_text(), 0.98);

        let ready = c.echo_ok();
        let allow_hover = ready && allow.contains(mouse.0, mouse.1);
        if ready {
            // 底色用 accent_strong：直接拿 accent 做底、text 做字实测只有
            // 深色 2.07:1 / 明亮 4.11:1，用户在最需要看清的"放行"按钮上读不清字
            rounded_rect(buf, w, h, allow, radius::SM, color::accent_strong(), 0.95);
            if allow_hover {
                rounded_rect(buf, w, h, allow, radius::SM, color::hairline(), state::hover());
                if mouse_down {
                    rounded_rect(buf, w, h, allow, radius::SM, [0, 0, 0], state::pressed());
                }
            }
        } else {
            rounded_rect(buf, w, h, allow, radius::SM, color::surface_3(), 0.9);
        }
        // 实心底上用纯白：这是"放行"按钮，必须一眼读清
        let (lrgb, la) = if ready { (color::HIGHLIGHT, 0.98) } else { (color::text_faint(), state::DISABLED) };
        let lw = tr.measure_bold("允许一次", font::BODY);
        tr.draw_bold(buf, w, h, (allow.x + allow.w / 2) as f32 - lw / 2.0, tr.vcenter(allow.y as f32, allow.h as f32, font::BODY), "允许一次", font::BODY, lrgb, la);

        self.confirm_buttons.push((allow, ConfirmButton::Allow));
        self.confirm_buttons.push((deny, ConfirmButton::Deny));
    }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

#[cfg(test)]
mod span_tests {
    use super::*;

    fn r() -> Rect {
        Rect { x: 10, y: 20, w: 100, h: theme::size::px(theme::size::ROW_H) }
    }

    /// 形状之外的行必须返回空区间 —— 否则调用方会把整行当成"直填带"刷上颜色。
    /// 这是实际踩过的坑：AI 指令条（胶囊形）底边多出一整行 512 像素。
    #[test]
    fn row_span_is_empty_outside_shape() {
        for y in [19.5f32, 19.0, 60.5, 61.0] {
            let (a, b) = row_inner_span(r(), 12.0, y);
            assert!(a >= b, "y={y} 在形状外，应返回空区间，实得 ({a},{b})");
        }
    }

    #[test]
    fn row_span_covers_full_width_in_middle() {
        // 垂直居中处远离圆角，应覆盖整个宽度
        assert_eq!(row_inner_span(r(), 12.0, 40.0), (10, 110));
    }

    #[test]
    fn row_span_shrinks_near_rounded_corner() {
        let (a0, b0) = row_inner_span(r(), 12.0, 20.5); // 顶端行
        let (a1, b1) = row_inner_span(r(), 12.0, 40.0); // 中间行
        assert!(a0 > a1 && b0 < b1, "顶端行的内部区间必须比中间行窄");
    }

    #[test]
    fn row_span_handles_radius_larger_than_half_height() {
        // 胶囊形（radius == h/2）：中间行仍应覆盖整个宽度
        let rr = Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) };
        assert_eq!(row_inner_span(rr, 26.0, 26.0), (0, 200));
        // 顶端行必须内缩
        let (a, b) = row_inner_span(rr, 26.0, 0.5);
        assert!(a > 0 && b < 200, "顶端行应内缩，实得 ({a},{b})");
    }

    /// 关键正确性约束：直填带内的每个像素，逐像素 SDF 算出的覆盖率必须真的是 1。
    ///
    /// 直填带用 `blend_pixel(alpha)` 而不再乘覆盖率 —— 一旦 `i` 取得不够保守，
    /// 边缘像素会被当成"满覆盖"，表现就是圆角边缘变实、和逐像素版本对不上。
    #[test]
    fn inner_span_only_covers_fully_opaque_pixels() {
        let shapes = [
            (Rect { x: 10, y: 20, w: 100, h: theme::size::px(theme::size::ROW_H) }, 12.0f32),
            (Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) }, 26.0),
            (Rect { x: 5, y: 5, w: 46, h: 46 }, 12.0),
            (Rect { x: 3, y: 7, w: 150, h: 42 }, 8.0),
        ];
        for (r, radius) in shapes {
            for yi in 0..r.h {
                let ay = (r.y + yi) as f32 + 0.5;
                let (a, b) = row_inner_span(r, radius, ay);
                for x in a..b {
                    let d = sdf_round_rect(r, radius, x as f32 + 0.5, ay);
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    assert!(
                        cov >= 0.999,
                        "矩形 {r:?} r={radius} 行 {yi} 像素 {x} 落在直填带内，但覆盖率只有 {cov}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod render_equiv_tests {
    use super::*;

    /// 逐像素参考实现：优化前的朴素写法，用于等价性比对。
    fn ref_rounded_rect(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
        let x0 = (r.x as f32 - 1.0).max(0.0) as usize;
        let y0 = (r.y as f32 - 1.0).max(0.0) as usize;
        let x1 = ((r.x + r.w) as f32 + 1.0).min(w as f32) as usize;
        let y1 = ((r.y + r.h) as f32 + 1.0).min(h as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    /// 逐像素参考实现（描边）。
    fn ref_rounded_outline(buf: &mut [u32], w: usize, h: usize, r: Rect, radius: f32, rgb: [u8; 3], alpha: f32) {
        let x0 = (r.x - 2).max(0) as usize;
        let y0 = (r.y - 2).max(0) as usize;
        let x1 = ((r.x + r.w) as usize + 2).min(w);
        let y1 = ((r.y + r.h) as usize + 2).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
                if d < 0.5 && d > -1.5 {
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    fn shapes() -> Vec<(Rect, f32)> {
        vec![
            (Rect { x: 20, y: 15, w: 100, h: 60 }, 12.0),
            (Rect { x: 0, y: 0, w: 200, h: theme::size::px(theme::size::AI_BAR_H) }, 26.0), // 胶囊
            (Rect { x: 5, y: 5, w: 46, h: 46 }, 12.0),  // Dock 图标
            (Rect { x: 3, y: 7, w: 150, h: 42 }, 8.0),  // 按钮
            (Rect { x: 40, y: 30, w: 20, h: 20 }, 3.0), // 小圆点
        ]
    }

    #[test]
    fn rounded_rect_fast_path_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            rounded_rect(&mut fast, w, h, r, radius, [255, 255, 255], 0.7);
            ref_rounded_rect(&mut reference, w, h, r, radius, [255, 255, 255], 0.7);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "rounded_rect {r:?} r={radius}：{diff} 个像素与逐像素参考不一致");
        }
    }

    #[test]
    fn rounded_outline_band_scan_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            rounded_outline(&mut fast, w, h, r, radius, [0, 0, 0], 0.6);
            ref_rounded_outline(&mut reference, w, h, r, radius, [0, 0, 0], 0.6);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(diff, 0, "rounded_outline {r:?} r={radius}：{diff} 个像素与逐像素参考不一致");
        }
    }

    /// 逐像素参考实现（裁切填充）。
    fn ref_fill_clipped(
        buf: &mut [u32],
        w: usize,
        h: usize,
        area: Rect,
        shape: Rect,
        shape_radius: f32,
        rgb: [u8; 3],
        alpha: f32,
    ) {
        let x0 = area.x.max(0) as usize;
        let y0 = area.y.max(0) as usize;
        let x1 = ((area.x + area.w) as usize).min(w);
        let y1 = ((area.y + area.h) as usize).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(shape, shape_radius, x as f32 + 0.5, y as f32 + 0.5);
                let cov = (0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    #[test]
    fn fill_clipped_fast_path_matches_reference() {
        let (w, h) = (240usize, 140);
        let cases = [
            // (area, shape, radius)：内容区贴合窗口圆角
            (Rect { x: 0, y: 0, w: 60, h: 60 }, Rect { x: 0, y: 0, w: 200, h: 120 }, 16.0),
            (Rect { x: 10, y: 20, w: 180, h: 90 }, Rect { x: 10, y: 20, w: 180, h: 90 }, 16.0),
            (Rect { x: 5, y: 5, w: 50, h: 50 }, Rect { x: 5, y: 5, w: 100, h: 100 }, 30.0),
            (Rect { x: 30, y: 10, w: 100, h: theme::size::px(theme::size::ROW_H) }, Rect { x: 0, y: 0, w: 240, h: 140 }, 26.0),
        ];
        for (area, shape, radius) in cases {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            fill_clipped(&mut fast, w, h, area, shape, radius, [200, 100, 50], 0.8);
            ref_fill_clipped(&mut reference, w, h, area, shape, radius, [200, 100, 50], 0.8);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            assert_eq!(
                diff, 0,
                "fill_clipped area={area:?} shape={shape:?} r={radius}：{diff} 像素与参考不一致"
            );
        }
    }

    /// 逐像素参考实现（渐变描边）。
    fn ref_gradient_outline(
        buf: &mut [u32],
        w: usize,
        h: usize,
        r: Rect,
        radius: f32,
        left: [u8; 3],
        right: [u8; 3],
        alpha: f32,
    ) {
        let x0 = (r.x - 2).max(0) as usize;
        let y0 = (r.y - 2).max(0) as usize;
        let x1 = ((r.x + r.w) as usize + 2).min(w);
        let y1 = ((r.y + r.h) as usize + 2).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = sdf_round_rect(r, radius, x as f32 + 0.5, y as f32 + 0.5);
                if d < 0.5 && d > -1.5 {
                    let cov = (0.5 - d).clamp(0.0, 1.0);
                    let t = ((x as f32 - r.x as f32) / r.w.max(1) as f32).clamp(0.0, 1.0);
                    let rgb = [
                        lerp(left[0] as f32, right[0] as f32, t) as u8,
                        lerp(left[1] as f32, right[1] as f32, t) as u8,
                        lerp(left[2] as f32, right[2] as f32, t) as u8,
                    ];
                    blend_pixel(buf, y * w + x, rgb, alpha * cov);
                }
            }
        }
    }

    #[test]
    fn gradient_outline_band_scan_matches_reference() {
        let (w, h) = (240usize, 140);
        for (r, radius) in shapes() {
            let mut fast = vec![0u32; w * h];
            let mut reference = vec![0u32; w * h];
            gradient_outline(&mut fast, w, h, r, radius, [10, 200, 210], [140, 120, 230], 0.9);
            ref_gradient_outline(&mut reference, w, h, r, radius, [10, 200, 210], [140, 120, 230], 0.9);
            let diff = fast.iter().zip(&reference).filter(|(a, b)| a != b).count();
            if diff > 0 {
                let mut shown = 0;
                for (i, (a, b)) in fast.iter().zip(&reference).enumerate() {
                    if a != b && shown < 8 {
                        eprintln!("    差异 ({},{}) fast={:08x} ref={:08x}", i % w, i / w, a, b);
                        shown += 1;
                    }
                }
            }
            assert_eq!(diff, 0, "gradient_outline {r:?} r={radius}：{diff} 像素与参考不一致");
        }
    }
}

#[cfg(test)]
mod blit_tests {
    use super::{dirty_rows, draw_background, draw_background_rows};
    use crate::draw::theme::color;

    #[test]
    fn dirty_rows_detects_single_changed_row() {
        let (w, fw, fh) = (4usize, 4usize, 3usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[w..2 * w].fill(7); // 只有第 1 行变化
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((1, 2)));
    }

    #[test]
    fn dirty_rows_returns_none_when_identical() {
        let (w, fw, fh) = (4usize, 4usize, 3usize);
        let prev = vec![1u32; w * fh];
        assert_eq!(dirty_rows(&prev, &prev, w, fw, fh), None);
    }

    #[test]
    fn dirty_rows_spans_first_to_last_change() {
        let (w, fw, fh) = (4usize, 4usize, 4usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[0] = 9; // 第 0 行
        cur[3 * w + 2] = 9; // 第 3 行
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((0, 4)));
    }

    #[test]
    fn dirty_rows_ignores_padding_beyond_valid_width() {
        // fw < w（行距大于行宽）：padding 区不参与比较
        let (w, fw, fh) = (6usize, 4usize, 2usize);
        let prev = vec![0u32; w * fh];
        let mut cur = prev.clone();
        cur[4] = 5; // 第 0 行有效宽度之外
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), None);
        cur[1] = 5; // 有效宽度之内
        assert_eq!(dirty_rows(&prev, &cur, w, fw, fh), Some((0, 1)));
    }

    /// 分帧生成（行带）必须与整屏一次生成**逐像素一致**。
    ///
    /// 这条不是凑数：壁纸改成分帧生成后，交互路径与 `--shot` 截图走了不同代码路径
    /// （前者分帧、后者 [`Renderer::prepare_background`] 整屏）。两者只要有差异，
    /// 就会出现"截图和真机不一样"这种最难查的 bug。
    /// 设置中心：**渲染一次并直接断言**（走查图的 diff 只能证明"场景变了"，
    /// 证明不了窗口内部结构画对了 —— 两栏布局、控件命中区、不可用页）。
    #[test]
    fn settings_content_renders_two_columns_and_registers_controls() {
        // 本测试所在的模块只导入了几何相关的名字，这里显式引入所需的类型
        use crate::draw::{draw_settings_content, Rect};
        use crate::text::TextRenderer;
        let Some(tr) = TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        // 铺白底：这样才能用"谁更暗"判断左栏（inset 底）与右侧页面
        for p in buf.iter_mut() {
            *p = 0x00FF_FFFF;
        }
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        // 按标题定位「时钟与时区」——**不要用 `first_enabled_page()`**：外观页启用后它已是第一页，
        // 而本测试要验的是时钟页那几个控件。
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "时钟与时区")
            .expect("时钟与时区页必须存在");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
        );

        // 1) 命中区要齐：页面行 + 本页的真控件
        use crate::settings::SettingsHit as H;
        assert!(
            hits.iter().any(|(_, x)| matches!(x, H::Page(_))),
            "左栏必须登记可切换的页面"
        );
        assert!(hits.iter().any(|(_, x)| matches!(x, H::Clock24h)), "24 小时制开关缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::ClockSeconds)), "显示秒开关缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::TzShift(30))), "时区 +1 缺失");
        assert!(hits.iter().any(|(_, x)| matches!(x, H::TzShift(-30))), "时区 -1 缺失");

        // 2) 命中区必须都在窗口内：越界会让点击落到别的控件上
        for (rr, id) in &hits {
            assert!(
                rr.x >= r.x && rr.y >= r.y && rr.x + rr.w <= r.x + r.w && rr.y + rr.h <= r.y + r.h,
                "命中区越界：{id:?} {rr:?}"
            );
            assert!(rr.w > 0 && rr.h > 0, "命中区不能为空：{id:?}");
        }

        // 3) 不可用的页不能出现在命中区里（点了不该有反应）。
        //    2026-10-02：三页全部接入后已无不可用页 —— 有才断言，没有就跳过。
        if let Some(disabled) = crate::settings::PAGES.iter().position(|p| !p.enabled) {
            assert!(
                !hits.iter().any(|(_, x)| matches!(x, H::Page(i) if *i == disabled)),
                "不可用页不该登记命中区"
            );
        }

        // 4) 两栏结构：左栏底（inset 0.55）应比右侧页面（保持底色）暗
        let lum = |p: u32| ((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF);
        let y = 590; // 左栏页行在 ~390 之前结束，取靠下的空白处更稳
        let left = lum(buf[y * w + 80]);
        let right = lum(buf[y * w + 700]);
        assert!(
            left < right,
            "左栏应比右侧页面暗（两栏结构）：left={left} right={right}"
        );

        // 「外观」页必须登记深色模式开关 —— 这是"默认白、用户可切换"的界面入口
        let appear = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("外观页必须存在");
        let mut hits2 = Vec::new();
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), appear, &mut hits2,
        );
        assert!(
            hits2.iter().any(|(_, x)| matches!(x, H::ToggleDark)),
            "外观页缺少深色模式开关"
        );
        assert!(
            hits2.iter().all(|(rr, _)| rr.w > 0 && rr.h > 0),
            "外观页的命中区不能为空"
        );
    }

    /// 缩放光标：两种朝向都要画出像素，且在屏幕边缘（含 0,0 与右下角）不越界、不 panic。
    #[test]
    fn resize_cursor_draws_and_clamps_at_edges() {
        use crate::draw::{draw_cursor, draw_resize_cursor};
        let (w, h) = (64usize, 48usize);
        for (mx, my) in [(32.0, 24.0), (0.0, 0.0), (63.0, 47.0)] {
            for horizontal in [true, false] {
                let mut buf = vec![0u32; w * h];
                draw_resize_cursor(&mut buf, w, h, mx, my, horizontal);
                assert!(
                    buf.iter().any(|p| *p != 0),
                    "缩放光标应画出像素（{mx},{my} horizontal={horizontal}）"
                );
            }
            let mut b2 = vec![0u32; w * h];
            draw_cursor(&mut b2, w, h, mx, my);
            assert!(b2.iter().any(|p| *p != 0), "普通箭头应画出像素（{mx},{my}）");
        }
    }

    #[test]
    fn background_rows_match_full_generation() {
        let (w, h) = (37usize, 53usize); // 非 64 整数倍，逼出末段不足一次行数的边界
        for &mode in &[color::Mode::Light, color::Mode::Dark] {
            color::set_mode(mode);
            let (mut whole, mut banded) = (vec![0u32; w * h], vec![0u32; w * h]);
            draw_background(&mut whole, w, h, 1.2, 0, false);
            let mut y = 0;
            while y < h {
                let y1 = (y + 8).min(h);
                draw_background_rows(&mut banded, w, h, 1.2, y, y1, 0, false);
                y = y1;
            }
            assert_eq!(whole, banded, "mode={mode:?}: 分帧生成与整屏生成不一致");
        }
        color::set_mode(color::Mode::Light); // 恢复默认，避免影响后续测试
    }

    /// 越界的行带必须是无操作，不能 panic、不能写出缓冲区。
    #[test]
    fn background_rows_clamps_out_of_range() {
        let (w, h) = (8usize, 8usize);
        let mut buf = vec![0u32; w * h];
        draw_background_rows(&mut buf, w, h, 1.0, h, h + 5, 0, false); // y0 == h
        draw_background_rows(&mut buf, w, h, 1.0, 100, 200, 0, false); // 完全越界
        draw_background_rows(&mut buf, w, h, 1.0, 5, 2, 0, false); // 反向区间
        assert!(buf.iter().all(|&p| p == 0), "越界行带不应写入任何像素");
    }
    /// §2.4：网络页必须有一个**可点**的 DNS 编辑入口。
    ///
    /// 这条测试盯的是"控件真的登记了命中区"——只画出来但没登记命中，
    /// 用户点下去毫无反应，而这正是本项目最不能接受的那种"假控件"。
    #[test]
    fn network_page_offers_a_clickable_dns_field() {
        // 本模块没有 import 这两个名字，用函数内 `use` 最不容易互相干扰
        use crate::draw::{draw_settings_content, Rect};
        // 与同文件其它绘制测试一致：**没有字体就跳过**。
        // 注意必须传 `Some(&tr)` —— `tr = None` 时绘制会提前返回，
        // 一个命中区都不会登记（第一次写这条测试时就是栽在这里：
        // 测试失败，但原因是"没画"，不是"控件没登记"）。
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let w = 420;
        let h = 320;
        let mut buf = vec![0u32; w * h];
        let mut hits = Vec::new();
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "网络")
            .expect("应当存在「网络」设置页");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page,
            &mut hits,
        );
        assert!(
            hits.iter().any(|(_, x)| matches!(x, crate::settings::SettingsHit::EditDns)),
            "网络页必须登记 DNS 编辑入口的命中区，否则就是「点了没用」的假控件"
        );
        // 每个命中区都要有面积（零面积区域点不到）
        for (rect, _) in &hits {
            assert!(rect.w > 0 && rect.h > 0, "命中区不能是零面积：{rect:?}");
        }
    }

    /// §3.4 无障碍：**用 WCAG 对比度客观验证**，而不是靠肉眼说「更清楚了」。
    ///
    /// 用纯函数（收 `light` 参数）而不是全局状态：测试并行执行，
    /// 全局原子量会被其它用例改掉，断言就成了掷骰子。
    #[test]
    fn high_contrast_raises_wcag_ratios_in_both_themes() {
        use crate::draw::theme::color::{contrast_ratio, push_bg, push_fg};
        // 基色取自色板（与 color:: 里的字面量一致）
        let cases: Vec<(&str, bool, [u8; 3], [u8; 3])> = vec![
            ("浅色/主文字", true, [26, 28, 34], [255, 255, 255]),
            ("浅色/次要文字", true, [94, 98, 108], [255, 255, 255]),
            ("浅色/占位文字", true, [146, 150, 160], [255, 255, 255]),
            ("深色/主文字", false, [240, 241, 246], [46, 49, 60]),
            ("深色/次要文字", false, [180, 182, 192], [46, 49, 60]),
            ("深色/占位文字", false, [136, 139, 150], [46, 49, 60]),
        ];
        for (name, light, fg, bgground) in cases {
            let before = contrast_ratio(fg, bgground);
            let after = contrast_ratio(push_fg(fg, light), push_bg(bgground, light));
            assert!(
                after > before,
                "{name}：高对比度**没有**提升对比度（{before:.2} → {after:.2}）"
            );
            assert!(
                after >= 4.5,
                "{name}：高对比度下仍低于 WCAG AA 正文 4.5（实得 {after:.2}）"
            );
        }
    }

    /// 最弱的一档（占位文字）在正常模式下**本来就不达标** —— 这条记录现状，
    /// 也说明高对比度不是"锦上添花"而是必需：它把这档从 ~3 提到 ≥4.5。
    #[test]
    fn faint_text_is_the_one_that_actually_needs_high_contrast() {
        use crate::draw::theme::color::{contrast_ratio, push_fg};
        let normal = contrast_ratio([146, 150, 160], [255, 255, 255]);
        assert!(normal < 4.5, "占位文字在正常模式下就不该达标（实得 {normal:.2}）");
        let hc = contrast_ratio(push_fg([146, 150, 160], true), [255, 255, 255]);
        assert!(hc >= 4.5, "高对比度必须把它拉过 AA（实得 {hc:.2}）");
    }

    /// 变换的方向性：**浅色主题往前景更暗、深色主题更亮**，绝不能反。
    ///
    /// 这条是防"帮倒忙"的：如果只写"推向极端"，浅色主题下的灰字会被推**亮**，
    /// 与白底对比更差 —— 那是把无障碍做成了反效果。
    #[test]
    fn push_direction_depends_on_theme() {
        use crate::draw::theme::color::push_fg;
        let grey = [146u8, 150, 160];
        let light_hc = push_fg(grey, true);
        let dark_hc = push_fg(grey, false);
        // 用 u32 求和：u8 相加会溢出（146+150+160 = 456 > 255）
        let sum = |c: [u8; 3]| c.iter().map(|v| *v as u32).sum::<u32>();
        assert!(
            sum(light_hc) < sum(grey),
            "浅色主题应把前景推得更暗：{grey:?} → {light_hc:?}"
        );
        assert!(
            sum(dark_hc) > sum(grey),
            "深色主题应把前景推得更亮：{grey:?} → {dark_hc:?}"
        );
    }

    /// §3.4：「外观」页必须登记高对比度开关 —— 无障碍功能**只有真的能点到**才算做了。
    #[test]
    fn appearance_page_registers_the_high_contrast_switch() {
        // 本模块没有 import 这两个名字，用函数内 use 最不容易互相干扰
        use crate::draw::{draw_settings_content, Rect};
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("应存在「外观」页");
        draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        assert!(
            hits.iter().any(|(_, x)| matches!(x, H::ToggleContrast)),
            "外观页缺少高对比度开关"
        );
        // 命中区不能是零面积（零面积 = 点不到）
        for (rect, x) in &hits {
            if matches!(x, H::ToggleContrast) {
                assert!(rect.w > 0 && rect.h > 0, "高对比度开关命中区是零面积：{rect:?}");
            }
        }
    }

    /// §3.5：「外观」页必须登记**语言开关**。
    ///
    /// 此前 `locale` 字段存在、能落盘，却**既改不了也不生效** ——
    /// 这条测试盯的就是"存了不用"这种半拉子状态不许再出现。
    #[test]
    fn appearance_page_registers_the_language_switch() {
        // 本模块没有 import Rect，用函数内 use
        use crate::draw::Rect;
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "外观")
            .expect("应存在「外观」页");
        crate::draw::draw_settings_content(
            &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        let locale_hits: Vec<_> = hits.iter().filter(|(_, x)| matches!(x, H::Locale(_))).collect();
        assert!(
            !locale_hits.is_empty(),
            "外观页缺少语言开关（`locale` 又会变成「存了不用」）"
        );
        // 默认中文下，点它应当切到 English（下标 1）
        assert!(
            locale_hits.iter().any(|(_, x)| matches!(x, H::Locale(1))),
            "默认中文时，语言开关应指向 English"
        );
        for (rect, _) in locale_hits {
            assert!(rect.w > 0 && rect.h > 0, "语言开关命中区是零面积：{rect:?}");
        }
    }

    /// 冒烟测试：**每个可用的设置页都要能画出来而不 panic**。
    ///
    /// 新加一页（如 §5.9 的快捷键页）时最容易忘的就是"能画"——
    /// 而这类问题只在真机打开那一页时才暴露。这条测试一次覆盖所有页。
    #[test]
    fn every_enabled_settings_page_draws_without_panicking() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (820usize, 620usize);
        let r = crate::draw::Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        for (i, p) in crate::settings::PAGES.iter().enumerate() {
            if !p.enabled {
                continue;
            }
            let mut buf = vec![0u32; w * h];
            let mut hits = Vec::new();
            crate::draw::draw_settings_content(
                &mut buf, w, h, r, crate::draw::test_ctx(Some(&tr)), i, &mut hits,
            );
            assert!(
                hits.iter().any(|(_, x)| matches!(x, crate::settings::SettingsHit::Page(_))),
                "页「{}」没有登记左栏导航命中区",
                p.title
            );
        }
    }

    /// §5.11：应用商店页要**分别登记**「可安装」与「已安装」两类命中。
    ///
    /// 这条测试盯的是"点了没反应/点错了"这类问题：可安装的那行必须是 `InstallApp`、
    /// 已安装的那行必须是 `RemoveApp`，而**已安装**与**未安装**的判断依据是商店
    /// 自己读到的版本（P-1 曾因为把它当成"名字"而全部判成「重新安装」）。
    #[test]
    fn app_store_page_registers_install_and_remove_hits() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        // 造两个临时目录：可安装目录里有两个包，已装目录里只有一个（版本一致）
        let root = std::env::temp_dir().join(format!(
            "aether-storeui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let (cat, inst) = (root.join("cat"), root.join("inst"));
        for (base, id, name, ver) in [
            (&cat, "alpha", "甲", "1.0"),
            (&cat, "beta", "乙", "2.0"),
            (&inst, "alpha", "甲", "1.0"),
        ] {
            std::fs::create_dir_all(base.join(id)).unwrap();
            std::fs::write(
                base.join(id).join("app.json"),
                format!(r#"{{"id":"{id}","name":"{name}","version":"{ver}"}}"#),
            )
            .unwrap();
        }
        let mut view = crate::store::StoreView::default();
        view.refresh(&cat, &inst);
        assert_eq!(view.installed().len(), 1, "应读到 1 个已装应用");
        assert_eq!(view.action_of(0), Some(crate::store::StoreAction::UpToDate));
        assert_eq!(view.action_of(1), Some(crate::store::StoreAction::Install));

        let (w, h) = (820usize, 620usize);
        let mut buf = vec![0u32; w * h];
        let r = crate::draw::Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
        let mut hits = Vec::new();
        let page = crate::settings::PAGES
            .iter()
            .position(|p| p.title == "应用商店")
            .expect("应用商店页必须存在");
        crate::draw::draw_settings_content(
            &mut buf, w, h, r,
            crate::draw::test_ctx_with_store(Some(&tr), &view),
            page, &mut hits,
        );
        use crate::settings::SettingsHit as H;
        let installs: Vec<usize> = hits
            .iter()
            .filter_map(|(_, x)| match x {
                H::InstallApp(i) => Some(*i),
                _ => None,
            })
            .collect();
        let removes: Vec<usize> = hits
            .iter()
            .filter_map(|(_, x)| match x {
                H::RemoveApp(i) => Some(*i),
                _ => None,
            })
            .collect();
        // 已安装的 alpha（下标 0）不该像按钮一样能点 → 只有 beta 是可点的安装项
        assert_eq!(installs, vec![1], "只有未安装的 beta 该登记安装命中：{installs:?}");
        assert_eq!(removes, vec![0], "已安装的 alpha 该登记卸载命中：{removes:?}");
        // 命中区不能在窗口外（否则点不到）
        for (rr, id) in &hits {
            assert!(
                rr.x >= r.x && rr.y >= r.y && rr.x + rr.w <= r.x + r.w && rr.y + rr.h <= r.y + r.h,
                "命中区越界：{id:?} {rr:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// §3.4：字号缩放必须是**纯函数**可测的（不碰全局状态 → 并行测试不会互相干扰）。
    #[test]
    fn font_scaling_is_pure_and_snaps_to_steps() {
        use crate::draw::theme::ui::font_at;
        assert_eq!(font_at(100, 14.0), 14.0);
        assert_eq!(font_at(125, 14.0), 17.5);
        assert_eq!(font_at(150, 14.0), 21.0);
        // 越界/奇怪的值要吸附到最近档位（设置文件可以被手改）
        assert_eq!(font_at(0, 20.0), 20.0, "0 应当吸附到 100%");
        // 注意：**112 才是更接近 100 的那个**（113 更接近 125）——
        // 这条语义在 `settings.rs` 的 `snap_scale` 测试里已经写死了，这里与它保持一致。
        assert_eq!(font_at(112, 20.0), 20.0, "112 更接近 100");
        assert_eq!(font_at(113, 20.0), 25.0, "113 更接近 125");
        assert_eq!(font_at(120, 20.0), 25.0, "120 更接近 125");
        assert_eq!(font_at(999, 10.0), 15.0, "999 吸附到最大档 150%");
        // 永远为正（0 字号会让渲染器除零/画不出东西）
        assert!(font_at(100, 0.0) >= 1.0);
    }

    /// **护栏**：最大档位下，关键字号必须仍然**小于它所在的固定行高** ——
    /// 不缩布局就放大字号，最容易出的问题就是文字互相压住。
    ///
    /// 这条测试的价值在于：以后有人调大 `font::TITLE`/`BODY` 或调小 `metric::` 高度，
    /// 它会立刻红 —— 而不是等到用户在 150% 下看到错位的界面。
    #[test]
    fn the_largest_step_still_fits_the_fixed_row_heights() {
        use crate::draw::theme::ui::font_at;
        use crate::draw::theme::{font, metric};
        let max = crate::settings::UI_SCALE_STEPS.iter().copied().max().unwrap_or(150);
        // 标题栏要装得下 TITLE
        assert!(
            font_at(max, font::TITLE) <= metric::TITLE_H as f32,
            "最大档 {max}% 下标题字号 {} 超过了标题栏高 {}",
            font_at(max, font::TITLE),
            metric::TITLE_H
        );
        // 顶栏要装得下 BODY
        assert!(
            font_at(max, font::BODY) <= metric::MENUBAR_H as f32,
            "最大档 {max}% 下正文字号 {} 超过了顶栏高 {}",
            font_at(max, font::BODY),
            metric::MENUBAR_H
        );
        // 设置页的行高（40）要装得下正文 + 说明两行里的较大者
        assert!(
            font_at(max, font::BODY) <= 40.0,
            "最大档 {max}% 下正文超过了设置行高 40"
        );
    }

    /// §3.5：AI 三态的英文标签必须齐全，且与中文不同。
    #[test]
    fn ai_status_has_english_labels_for_every_state() {
        for st in [
            crate::draw::AiStatus::Local,
            crate::draw::AiStatus::Cloud,
            crate::draw::AiStatus::Offline,
        ] {
            assert!(!st.label().is_empty(), "中文标签不能为空");
            assert!(!st.label_en().is_empty(), "英文标签不能为空");
            assert_ne!(st.label(), st.label_en(), "两种语言不该一模一样");
        }
    }

    /// §3.5：**控制中心的双语必须真的写在源码里**（探针式核对）。
    ///
    /// 为什么用源码扫描而不是画一遍看：绘制结果没法断言"这行字是英文"，
    /// 而这里要守的性质很具体 —— 每一处文案都**同时**写了中文和英文。
    /// 有人把某处改回硬编码中文时，这条测试立刻红
    /// （与快捷键总览那套"探针"同一思路）。
    #[test]
    fn control_center_strings_are_bilingual_in_the_source() {
        let src = include_str!("draw.rs");
        let pairs = [
            ("AI 中枢", "AI core"),
            ("深色模式", "Dark mode"),
            ("中文输入法", "Chinese IME"),
            ("24 小时制", "24-hour clock"),
            ("快捷跳转", "Quick actions"),
            ("打开设置", "Open Settings"),
            ("打开终端", "Open Terminal"),
            ("通知", "Notifications"),
            ("声音", "Sound"),
            ("音量", "Volume"),
            ("电源", "Power"),
            ("关机", "Power off"),
            ("重启", "Restart"),
            ("注销", "Log out"),
            ("Esc 或点击别处关闭", "Esc or click elsewhere to close"),
        ];
        for (zh, en) in pairs {
            let probe = format!("(\"{zh}\", \"{en}\"");
            assert!(
                src.contains(&probe),
                "控制中心的「{zh}」没有配对英文「{en}」——它被改回硬编码中文了？"
            );
        }
        // 带未读数的动态文案走语言分支，单独核对两条分支都在
        assert!(src.contains("Notifications ({unread})"), "动态文案缺英文分支");
        assert!(src.contains("通知（{unread} 条未读）"), "动态文案缺中文分支");
    }

    /// §5.5：命令面板的**可见行都要能点**，且登记的是**条目下标**（不是行号）。
    ///
    /// 这条盯的是"点了没反应 / 点错了"：如果没有登记命中区，`palette_hit` 永远是 `None`；
    /// 如果登记成了行号，点了第一条会执行到别的条目（行是过滤排序后的，两者不同）。
    #[test]
    fn palette_registers_a_clickable_row_for_every_visible_result() {
        let Some(tr) = crate::text::TextRenderer::load() else {
            eprintln!("无可用字体，跳过");
            return;
        };
        let (w, h) = (800usize, 600usize);
        let mut p = crate::palette::Palette::default();
        p.open_with(crate::palette::build_items(
            &["a", "b"],
            &[("设置：外观", "Settings: Appearance"), ("设置：网络", "Settings: Network")],
            &[],
        ));
        let mut buf = vec![0u32; w * h];
        let mut hits = Vec::new();
        crate::draw::draw_palette(&mut buf, w, h, &p, Some(&tr), &mut hits);
        assert_eq!(
            hits.len(),
            p.visible().len(),
            "每一条可见结果都该登记命中区"
        );
        assert!(!hits.is_empty(), "至少要有一条可见结果，否则这条测试没有意义");
        // 命中区要有真实大小，且登记的必须是**条目下标**
        for (i, (r, item)) in hits.iter().enumerate() {
            assert!(r.w > 0 && r.h > 0, "命中区不能是空的：{r:?}");
            assert_eq!(
                *item, p.visible()[i].item,
                "第 {i} 行登记的必须是它对应的条目下标"
            );
        }
    }

    /// §3.5：预览失败原因的**两套文案**，四种原因全覆盖。
    ///
    /// 这条测试能存在，正因为文案被抽成了纯函数（`preview_error_text`）——
    /// 之前文案住在 `read_preview` 里、又只有中文，既不可测也没法翻译。
    #[test]
    fn preview_error_text_covers_all_reasons_in_both_languages() {
        use crate::i18n::Lang;
        use crate::draw::{preview_error_text, PreviewError};
        let cases: Vec<PreviewError> = vec![
            PreviewError::IsDir,
            PreviewError::TooLarge("12.3 MB".into()),
            PreviewError::Binary,
        ];
        for e in &cases {
            let zh = preview_error_text(Lang::Zh, e);
            let en = preview_error_text(Lang::En, e);
            assert!(!zh.is_empty() && !en.is_empty(), "{e:?} 两种语言都要有文案");
            assert!(
                !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "英文文案里有汉字：{en}"
            );
        }
        // 带参数的那种：大小文字必须出现在两种文案里（否则用户不知道到底多大）
        let big = PreviewError::TooLarge("12.3 MB".into());
        assert!(preview_error_text(Lang::Zh, &big).contains("12.3 MB"));
        assert!(preview_error_text(Lang::En, &big).contains("12.3 MB"));
        // Io / Message 原样透传：它们不是本模块的话，不该被改写
        let io = PreviewError::Io("permission denied".into());
        assert_eq!(preview_error_text(Lang::Zh, &io), "permission denied");
        assert_eq!(preview_error_text(Lang::En, &io), "permission denied");
        let m = PreviewError::Message("界面自己取好的话".into());
        assert_eq!(preview_error_text(Lang::En, &m), "界面自己取好的话");
    }

}
