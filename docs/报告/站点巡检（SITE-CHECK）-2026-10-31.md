# 站点巡检（SITE-CHECK）— 2026-10-31

> 范围：仓库内 `site/` 生成结果 + 线上 `https://aether.cbnac.com/`。
> 本报告记录部署前的漂移、2026-10-31 的实际部署，以及部署后的验证结果。

## 一、结论

| 对象 | 结论 |
|---|---|
| 本地生成站点 | **通过**：9 个页面与实测数据一致，下载脚本哈希与 `site/release.json` 一致 |
| 线上站点可用性 | **通过**：抽查的 8 个 URL 全部 HTTP 200，TLS 正常，安全响应头存在 |
| 线上站点内容 | **已同步**：52,544 行 / 77 文件 / 793 测试；`site-health-check` **39/39 通过** |
| 线上部署 | **已完成**：2026-10-31 22:44（Asia/Shanghai）上传 24 个文件并热重载 nginx |
| 本地文档链接 | **通过**：154 条本地链接，断链 0 条 |

线上站点曾落后于仓库；现在已同步。下载页对 v0.1.1 ISO 装机问题的警告仍然保留，因为那是**已发布产物**的真实状态，需重建 ISO 后才能解除。

## 二、本地站点检查

命令与结果：

```text
python scripts/gen-site.py --check
[ OK ] site: 9 个页面与实测数据一致 · 下载脚本哈希与发布一致
       （v0.1.1 · ISO 41.0 MB · 52,544 行 · 793 项测试）

python scripts/check-links.py
检查 103 条本地链接，断链 0 条
```

本轮发现并修掉的站点硬编码错误：

| 页面 | 旧值 | 新值 | 原因 |
|---|---|---|---|
| `site/en/index.html` | `43 files` | `77 files` | 英文首页文件数长期手写，中文页已是 77 |
| `site/index.html`、`site/en/index.html` | `og:image:height=793` | `630` | 分享图实际尺寸 1200×630；793 是测试数被误注入 |
| `site/faq.html`、`site/en/faq.html` | `24,388 行` / `24,388 lines` | `52,544` | FAQ 的手写总行数停留在旧版本 |
| `site/download.html`、`site/en/download.html`、`site/faq.html` | `1024×793` | `1024×768` | 最小分辨率笔误（793 同样是测试数串入） |
| `site/index.html`、`site/en/index.html`、`site/download.html`、`site/en/download.html` | ISO 构建日期 `2026-09-29` / `2026-10-01` | `2026-10-15` | 与 `site/release.json` 的 `iso_date` 对齐 |

同时给 `scripts/gen-site.py --check` 补了两类回归判据：

- 首页必须出现当前的 `rust_files`（不再只查行数/测试数）；
- `og:image:height` 必须是 `630`，防止测试数再次被注入分享图元数据。

## 三、线上站点检查

### 3.1 可用性

以下地址均返回 `200`，内容类型正确：

- `https://aether.cbnac.com/`
- `https://aether.cbnac.com/download.html`
- `https://aether.cbnac.com/changelog.html`
- `https://aether.cbnac.com/faq.html`
- `https://aether.cbnac.com/en/`
- `https://aether.cbnac.com/en/download.html`
- `https://aether.cbnac.com/robots.txt`
- `https://aether.cbnac.com/sitemap.xml`

### 3.2 安全响应头

`curl -sSI https://aether.cbnac.com/` 实测存在：

```text
Content-Security-Policy: default-src 'self'; ... frame-ancestors 'none'
X-Content-Type-Options: nosniff
Referrer-Policy: strict-origin-when-cross-origin
Strict-Transport-Security: max-age=31536000; includeSubDomains
Permissions-Policy: geolocation=(), microphone=(), camera=()
```

### 3.3 部署前漂移（历史）

| 页面 | 部署前线上值 | 仓库当前值 |
|---|---|---|
| 首页（中文） | 51,138 行 / 71 个源文件 / 768 项测试 | 52,544 / 77 / 793 |
| 首页（英文） | 51,138 lines / 71 files / 768 tests | 52,544 / 77 / 793 |
| FAQ（中文） | 24,388 行 / 71 个源文件 | 52,544 / 77 |
| FAQ（英文） | 24,388 lines / 71 source files | 52,544 / 77 |

下载页的 `v0.1.1` ISO 事实（41.0 MB、SHA256 `eea68da9…`）与 `site/release.json` 一致；页面明确提示
“v0.1.1 ISO 装机不可用（未经 isohybrid）”，这是对**已发布产物**的准确描述，需重建 ISO 后才能解除。

### 3.4 部署后验证

2026-10-31 22:44（Asia/Shanghai）执行 `deploy-site.py --skip-build`，结果：

- 上传 **24 个文件** → `/var/www/aether.cbnac.com`；
- 现有 vhost `/www/server/panel/vhost/nginx/aether.cbnac.com.conf` 带 TLS 段，脚本**未覆盖**，只校验 root 与安全头片段；
- `nginx -t` 使用运行中的二进制/配置，语法通过；
- 给 master `3980202` 发 HUP，worker 进程换代，确认重载真实生效；
- 既有业务 `cbnac.com` / `school.cbnac.com` / `api.cbnac.com` 均保持 200；
- 安全响应头实测 **3/3 关键项**通过。

部署后 `python scripts/site-health-check.py`：

```text
39/39 项通过；失败 0 项
```

首页确认出现 **52,544 行 / 77 个源文件 / 793 项测试**，ISO 日期为 **2026-10-15**；下载页哈希仍为
`eea68da9…`，与 `site/release.json` 一致。

## 四、部署访问状态

本次部署使用用户提供的 root 私钥（明确选择 `AETHER_VPS_ALLOW_ROOT=1`），属于一次性例外。
部署期间已创建非 root 账号：

```text
aether-deploy:x:1001:1001::/home/aether-deploy:/bin/bash
groups: aether-deploy, wheel
sudo -n true → SUDO_OK
```

root 私钥已从本机删除。后续应优先使用 `aether-deploy`，不要继续使用 root 直连。

**已知限制**：非 root 的 `deploy-site.py --check-only` 在 `probe_layout` 阶段没有识别到
`/www/server/panel/vhost/nginx`，回退到 `/etc/nginx/conf.d`。因此本次实际部署仍由 root 完成；
如果要长期用 `aether-deploy` 部署，需要先修 `probe_layout` 的 `nginx -T` 权限处理，并确认
`/var/www/aether.cbnac.com` 的写权限。这是一个后续安全改进项，不应假装已经解决。

## 五、结论边界

- 线上站点已完成一次真实部署，公开数字已同步；这不是本地模拟结果。
- 本次使用了 root 一次性例外；root 私钥已删除，但**这把 key 曾在聊天记录中出现，仍建议轮换**。
- 下载页的 v0.1.1 ISO 装机警告仍然有效；只有重建并发布新 ISO 后才能解除。
- 非 root 部署链路尚未完全验证，已记录为后续改进项。
