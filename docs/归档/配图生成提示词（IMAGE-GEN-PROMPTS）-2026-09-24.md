# AetherOS 图片生成提示词集

用途：喂给图片生成 AI（Midjourney / DALL·E / Stable Diffusion / 即梦等），产出系统视觉资产。
原则：**所有场景共用同一条风格锚点**，保证全套图风格统一。

---

## 0. 风格锚点（Style Anchor）——每个提示词都要带上

### 中文版

> 深空基调的高级操作系统视觉：深邃的近黑蓝灰底色（#10121A 到 #20242F 渐变），克制的极光光带，主色青（#40BECD）与辅色紫（#9682DC）双色柔和交织，苹果级极简美学，大量留白与呼吸感，细腻噪点颗粒，电影级柔和光晕，8K 超精细，专业 UI 设计壁纸，无文字、无 logo、无界面元素

### 英文版（多数模型对英文响应更好，推荐主用）

> Premium operating system wallpaper, deep space aesthetic: near-black blue-gray background gradient (#10121A to #20242F), restrained aurora light bands in cyan (#40BECD) and violet (#9682DC) softly interweaving, Apple-grade minimalist elegance, generous negative space, subtle film grain, cinematic soft glow, ultra-detailed 8K, professional OS design wallpaper, no text, no logo, no UI elements

### 通用负面提示词（Negative Prompt，SD 类模型用）

> text, watermark, logo, letters, UI elements, buttons, windows, people, faces, clutter, oversaturated, neon cyberpunk, harsh lines, lens flare overload, cartoon, anime, low quality

### 关键纪律（生成翻车最多就翻在这几条）

1. **不要赛博朋克霓虹**——"aurora"很容易被模型理解成高饱和霓虹灯，必须靠"restrained / soft / muted"压住
2. **不要出现文字和界面**——模型爱在"OS wallpaper"里塞假窗口假文字，负面词必须带
3. **饱和度宁低勿高**——这套设计的核心是克制，图太艳就废了
4. **生成 1920×1080 或更高，缩到 1280×760 用**——缩图能显著提升细腻度

---

## 场景 1 · 主桌面壁纸（默认）

**中文**
```
深空基调的高级操作系统桌面壁纸：近黑蓝灰底色自上而下渐变（#10121A 到 #20242F），
画面上三分之一处一条宽阔的极光光带缓缓流动，青色（#40BECD）为主、紫色（#9682DC）
为辅，柔和交织如丝绸，光带边缘自然消散进深色背景，下半屏近乎纯深色留白
（为桌面图标和 Dock 留出干净区域），细腻噪点颗粒，电影级柔和光晕，
苹果级极简美学，8K 超精细，无文字、无界面元素
```

**English**
```
Premium OS desktop wallpaper, deep space aesthetic: near-black blue-gray gradient
background (#10121A top to #20242F bottom), a single wide flowing aurora band
drifting across the upper third, cyan (#40BECD) dominant with violet (#9682DC)
accents, silky smooth light ribbons dissolving into darkness, lower half nearly
empty dark space for desktop icons and dock, subtle film grain, cinematic soft
glow, Apple-grade minimalism, ultra-detailed 8K, no text, no UI elements
--ar 16:9
```

---

## 场景 2 · 锁屏 / 登录界面背景

比主壁纸更暗、更静——锁屏上要叠时钟和用户信息，背景必须退后。

**中文**
```
操作系统锁屏背景：极深的近黑蓝灰底色（比桌面壁纸更暗 30%），画面正中央一团
极其柔和的极光光晕，青色与紫色以极低亮度缓慢呼吸般交融，边缘完全融入黑暗，
四周大面积纯暗留白，氛围静谧、克制、高级，细腻噪点，电影级柔光，
无文字、无界面元素，8K
```

**English**
```
OS lock screen background: very deep near-black blue-gray base (30% darker than
a desktop wallpaper), a single soft aurora glow blooming at the exact center,
cyan and violet breathing together at very low luminosity, edges dissolving
completely into darkness, vast empty dark space all around, serene restrained
premium atmosphere, subtle film grain, cinematic soft light, no text, no UI
elements, 8K --ar 16:9
```

---

## 场景 3 · 开机画面（Boot Splash）

中心对称，给 logo 留位——开机画面是品牌时刻，要仪式感。

**中文**
```
操作系统开机画面：纯近黑背景（#0D0F14），画面正中央一圈极细的青紫色极光光环，
如恒星诞生般从黑暗中浮现，环的内侧微微发亮、外侧自然消散，完美的中心对称
构图，环心留纯黑圆形区域（用于放置系统 logo），极简、仪式感、高级感，
细腻噪点颗粒，无文字，8K
```

**English**
```
OS boot splash screen: pure near-black background (#0D0F14), a single thin ring
of cyan-violet aurora light emerging from darkness at the exact center, like a
star being born, inner edge faintly luminous, outer edge dissolving naturally,
perfect radial symmetry, pure black circular area at the core reserved for a
system logo, minimal, ceremonial, premium, subtle film grain, no text,
8K --ar 16:9
```

---

## 场景 4 · 安装器 / 向导背景

比桌面更有"旅程感"——安装是用户与系统的第一次正式见面。

**中文**
```
操作系统安装向导背景：深空蓝灰渐变底色，一道极光光带从画面左下角升起、
向右上方蜿蜒流动，如一条光的河流穿越深空，青色为主紫色点缀，光带走势
引导视线向右上（隐喻前进与安装进程），右侧和上方大面积深色留白
（为向导面板留位），柔和电影光晕，细腻噪点，苹果级极简，无文字无界面，8K
```

**English**
```
OS installer wizard background: deep space blue-gray gradient, an aurora band
rising from the lower-left corner and winding toward the upper-right like a
river of light crossing deep space, cyan dominant with violet accents, the
flow direction guiding the eye upward-right (metaphor for progress), generous
dark negative space on the right and top for the wizard panel, soft cinematic
glow, subtle film grain, Apple-grade minimalism, no text, no UI, 8K --ar 16:9
```

---

## 场景 5 · AI 面板 / AI 相关氛围图

AI 是产品灵魂——这一张可以比别的场景更"活"，紫色调占比提高。

**中文**
```
AI 主题氛围图：深空底色上，青色与紫色两股极光相互缠绕、盘旋上升，
如思想在流动、如神经元在对话，光带中隐约有极细的粒子光点闪烁
（暗示智能与计算），画面中央偏上一处柔和的高亮焦点，整体神秘、灵动、
克制，深紫色调占比高于其他场景，细腻噪点，电影级光效，无文字，8K
```

**English**
```
AI-themed ambient artwork: over a deep space base, two streams of aurora,
one cyan and one violet, spiraling around each other and rising like flowing
thought or conversing neurons, faint tiny particles of light sparkling within
the streams (hinting at intelligence and computation), a soft luminous focal
point slightly above center, mysterious, alive, restrained, higher violet
proportion, subtle film grain, cinematic lighting, no text, 8K --ar 16:9
```

---

## 场景 6 · 备用壁纸组（深 / 浅变体各一）

发布时至少给两张备选，一张更沉、一张更亮。

**6a 暗夜（更深）**
```
English: Ultra-dark minimalist OS wallpaper, almost pure black background
(#0A0C10), a single whisper-thin horizontal aurora line in dim cyan glowing
faintly across the middle third, extreme restraint, vast darkness, subtle
grain, premium, no text --ar 16:9
```

**6b 晨曦（更亮，青色提亮 40%）**
```
English: Premium OS wallpaper, deep blue-gray base (#1A1E2A) brightened with
a soft dawn-like glow, a luminous aurora band in brighter cyan (#5FD6E3)
sweeping gracefully across the upper half, hints of violet at the edges,
feeling of early morning over a dark ocean, subtle grain, cinematic, minimal,
no text --ar 16:9
```

---

## 场景 7 · 发布宣传横幅（README / 官网头图）

唯一允许出现文字的场镜——但文字后期自己加，不让模型生成。

**中文**
```
科技产品发布横幅：深空蓝灰渐变底色，一道壮丽的青紫极光横贯画面中上部，
如面纱般轻盈展开，画面下三分之一为纯净深色平台（用于后期叠加产品名称
与标语文字），大气、史诗感、克制的高级，电影级宽幅构图，细腻噪点，
无文字、无 logo，8K 超宽画幅
```

**English**
```
Tech product launch banner: deep space blue-gray gradient, a magnificent
cyan-violet aurora sweeping across the upper-middle of the frame like a
weightless veil, lower third a clean dark plateau for post-production text
overlay, epic yet restrained, cinematic ultrawide composition, subtle film
grain, no text, no logo, 8K --ar 21:9
```

---

## 使用速查

| 场景 | 画幅 | 生成后用途 |
|---|---|---|
| 1 主壁纸 | 16:9 | rootfs `/usr/share/aether/wallpapers/default.png` |
| 2 锁屏 | 16:9 | M6 锁屏界面 |
| 3 开机画面 | 16:9 | M6 开机动画底帧 |
| 4 安装器 | 16:9 | aether-install 向导背景 |
| 5 AI 氛围 | 16:9 | AI 面板背景 / 关于页 |
| 6 备选×2 | 16:9 | 壁纸选择器 |
| 7 横幅 | 21:9 | README 头图 / M6 发布物料 |

**工作流建议**：英文提示词为主 → 每个场景生成 4 张候选 → 挑 1 张 → 统一过一遍
降饱和 + 压暗（如果需要）→ 缩放到 1920 宽存档、1280×760 入系统。
