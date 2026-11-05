# 账号与博客改造报告

> 日期：2026-11-01
> 范围：GitHub 账号门面、个人博客 `blog.cbnac.com`、AetherOS 作者入口
> 线上博客：<https://blog.cbnac.com/blog/>
> 作者 Profile：<https://github.com/qwert702/qwert702>
> 博客源码：<https://github.com/qwert702/blog>

## 一、结论

本轮完成了三件事：

1. GitHub 账号从“只有 AetherOS 一个项目页”整理成有作者 Profile、博客入口、统一描述和 topics 的账号；
2. 个人博客从 Vite SPA 重建为可静态部署的博客，并在全站加入 GitHub 账号入口；
3. 重复的 `awesome-*` 仓库与空仓库完成归档，不再干扰账号首页。

## 二、GitHub 账号

### 2.1 Profile

- 新建并写入 `qwert702/qwert702` Profile README：
  - AetherOS 作者/维护者身份
  - 项目定位与技术关注点
  - 官网、博客、源码、文档入口
  - GitHub Issues / PR 联系方式
- Profile bio 已设置为 AetherOS 作者与维护者。
- Profile 网站字段：`https://blog.cbnac.com/blog/`
- Profile README 已加入博客入口：`https://blog.cbnac.com/blog/`
- AetherOS 仓库主页保留：`https://aether.cbnac.com/`

### 2.2 仓库门面

已更新：

- `AetherOS`：README 中英文增加博客入口；保留既有 CI/许可证/Release/官网徽章、作者与维护区、社区文件。
- `blog`：新建仓库，描述、homepage、topics 已设置；源码与线上页面同步。
- `dsh-context-compressor`、`kore-agent`、`english-learning-demo`、`OpenChatCut`、`deepseek-harness`：补齐仓库描述。
- `dsh-*`、`server-gateway-firewall`、`CutForge`、`OpenChatCut`、`skill-engineering-audit`、`dsh-community`、`dsh-link-plugin`：补齐 topics。
- 新增 `SECURITY.md`、`CONTRIBUTING.md`、`CODE_OF_CONDUCT.md`、`AUTHORS.md`、`CITATION.cff`、Issue/PR 模板、CODEOWNERS、Dependabot。

### 2.3 归档仓库

以下仓库已归档（保留历史，不删除）：

- `awesome-deepseek-harness-1`
- `awesome-deepseek-harness-2`
- `awesome-deepseek-harness-3`
- `awesome-dsh-plugin`
- `awesome-dsh-plugin-1`
- `Awesome-DeepSeek-Harness-Plugins`
- `Blue-Whale-Harness`
- `-`

canonical 列表保留为 `awesome-deepseek-harness`。归档仓库描述已标注 superseded，避免访客误入。

## 三、个人博客

### 3.1 现状与改造

- 旧站：`https://blog.cbnac.com/blog/`，Vite + React SPA；首页依赖远程 CloudFront 视频；正文依赖客户端渲染。
- 服务器上只有构建产物，旧前端源码不在本地 AetherOS 仓库；已完整备份旧静态目录与根入口。
- 新站：无运行时框架的静态 HTML/CSS/JS；保留 `/blog/` 路径，便于回滚和迁移。
- 新站页面：
  - `/blog/` 首页
  - `/blog/about.html` 关于
  - `/blog/archive.html` 归档
  - `/blog/projects.html` 项目
  - `/blog/posts/aetheros.html`
  - `/blog/posts/static-blog.html`
  - `/blog/posts/notes.html`
  - `/blog/404.html`、`rss.xml`、`sitemap.xml`、`robots.txt`

### 3.2 GitHub 入口

以下位置都可到达 `https://github.com/qwert702`：

- 全站 header 的 GitHub 按钮
- 首页 hero 的 GitHub 按钮
- 首页 GitHub 项目卡与 6 个项目卡
- `projects.html` 的 12 个项目卡与 GitHub 全部仓库入口
- About 页的 GitHub 区块
- 文章页 header/footer
- 根入口 `https://blog.cbnac.com/` 的 GitHub 按钮

### 3.3 设计

- 主题：地质 / 时间，沿用「时光镌刻于层岩之中」
- 暖白岩层、墨色正文、矿物青强调色；衬线标题 + 高可读正文
- 明暗双主题、移动端双行导航、`prefers-reduced-motion`
- 恢复旧版 CloudFront 视频 hero（静音、循环、`preload=metadata`），配静态海报与暗色渐变降级；移动端降低视频透明度
- 首页新增「项目」区，`projects.html` 展示 AetherOS、DSH 插件、CutForge、OpenChatCut、服务器防火墙等 18 个项目，不再只出现 AetherOS

### 3.4 旧资料与 URL 恢复

- 新增 `/blog/resources.html`，恢复旧站资源页内容：
  - Claude Code 安装教程、安装命令与三张旧截图
  - Claude Code 基础指令速查
  - 数学课件与 Python 教程入口
- 旧 URL 已做跳转：
  - `/blog/resource` → `/blog/resources.html`
  - `/blog/resource/claude-code` → `/blog/resources.html#claude-code`
  - `/blog/resource/claude-code-basics` → `/blog/resources.html#basics`
  - `/blog/about` → `/blog/about.html`
  - `/blog/admin-panel` → `/blog/`（新静态站没有后台面板）
- 旧站 `/math/` 与 `/tutorials/python/` 目录一直保留在原位，仍返回 200。
- 旧站三张资源截图已复制到 `assets/resources/`，不再依赖已移动的旧 SPA 目录。
- 旧站 CC-Switch 安装包文件在服务器上未找到；资源页保留官方入口，不伪造下载链接。

### 3.5 部署与安全

- 部署目录：`/var/www/cbnac.cn/blog`
- 旧目录已移动为带时间戳的 `blog.old-*`，并在服务器 `/root/blog-backup-*.tar.gz` 保留压缩备份
- `https://cbnac.com/blog/` 已 301 到 `https://blog.cbnac.com/blog/`
- nginx 已加：
  - `Strict-Transport-Security`
  - `Content-Security-Policy`
  - `Permissions-Policy`
  - `X-Frame-Options`
  - `X-Content-Type-Options`
  - `Referrer-Policy`
  - `charset utf-8`
- `nginx -t` 通过后 reload

## 四、验证

| 项目 | 结果 |
|---|---|
| 博客首页 | HTTP 200 |
| 关于 / 归档 / 资料 / 三篇文章 | HTTP 200 |
| 首页 GitHub 链接 | 12 处 |
| 项目页 GitHub 链接 | 18 个项目、全部指向 GitHub |
| 首页视频 hero | 旧 CloudFront 视频已恢复，CSP 增加 `media-src` |
| 旧 `/blog/resource`、`/blog/about` | 301 到新资料页 / 关于页 |
| 旧 `/math/`、`/tutorials/python/` | HTTP 200，仍在原路径 |
| 旧资源截图 | 3 张已复制到 `assets/resources/`，可访问 |
| RSS / sitemap | XML 解析通过 |
| 本地 HTML 内部链接 | 除 `root-index.html` 的服务端绝对路径外，无断链 |
| 安全头 | HSTS / CSP / Permissions-Policy / X-Frame-Options / nosniff / Referrer-Policy 均存在 |
| `cbnac.com/blog/` | 301 → `blog.cbnac.com/blog/` |
| GitHub 仓库归档数 | 8 |
| `blog` 仓库 | 存在，README/源码/描述/homepage 已设置 |

## 五、边界与后续

- 旧博客的独立源码仓库没有在服务器上找到；但旧构建产物已完整备份，旧资源页内容、关于页原文和三张截图已恢复到新站，旧资料 URL 也已做跳转。新站的文章区是本次新写的初始文章；若后续找到旧文章导出，可直接替换 `posts/`。
- GitHub 头像、显示名、所在地、邮箱、社交账号没有编造；只设置了可确认的 `qwert702` 身份与博客链接。
- GitHub Social preview 目前没有公开 REST API，需要在仓库 Settings → Social preview 手动上传；博客社交封面已生成在 `blog/assets/og-cover.png`。
- 博客的登录/管理接口仍是服务器上原有 Next.js `/api/blog`，新静态站未接入后台编辑器；这是下一阶段工作，不影响阅读。
- 未做真实 Lighthouse 跑分；当前页面无框架运行时、资源体积小，性能路径已按静态站收敛。
