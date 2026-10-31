# 站点巡检（SITE-CHECK）— 2026-10-31

> 范围：仓库内 `site/` 生成结果 + 线上 `https://aether.cbnac.com/`。
> 本报告只记录实际请求/命令的结果；没有部署生产站点。

## 一、结论

| 对象 | 结论 |
|---|---|
| 本地生成站点 | **通过**：9 个页面与实测数据一致，下载脚本哈希与 `site/release.json` 一致 |
| 线上站点可用性 | **通过**：抽查的 8 个 URL 全部 HTTP 200，TLS 正常，安全响应头存在 |
| 线上站点内容 | **不一致**：仍是旧版数字（51,138 行 / 71 文件 / 768 测试），仓库已是 52,544 / 77 / 793 |
| 线上部署 | **未执行**：当前只有 root 账号，`deploy-site.py` 按安全策略拒绝 root 直连；没有非 root 部署凭据 |
| 本地文档链接 | **通过**：103 条本地链接，断链 0 条 |

线上站点不是“坏了”，而是**没有跟上当前仓库**。这是公开信息准确性问题，不影响已有下载文件的哈希。

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

### 3.3 内容漂移

| 页面 | 线上值 | 仓库当前值 |
|---|---|---|
| 首页（中文） | 51,138 行 / 71 个源文件 / 768 项测试 | 52,544 / 77 / 793 |
| 首页（英文） | 51,138 lines / 71 files / 768 tests | 52,544 / 77 / 793 |
| FAQ（中文） | 24,388 行 / 71 个源文件 | 52,544 / 77 |
| FAQ（英文） | 24,388 lines / 71 source files | 52,544 / 77 |

下载页的 `v0.1.1` ISO 事实（41.0 MB、SHA256 `eea68da9…`）与 `site/release.json` 一致；页面明确提示
“v0.1.1 ISO 装机不可用（未经 isohybrid）”，这是对**已发布产物**的准确描述，需重建 ISO 后才能解除。

## 四、部署阻塞与建议

`deploy-site.py --check-only` 被安全策略拒绝：

```text
拒绝以 root 直连 47.98.207.149：请用有 sudo 的普通账号（AETHER_VPS_USER）
```

当前环境事实：

- `AETHER_VPS_HOST` 已设置；
- `AETHER_VPS_USER=root`；
- 本机有 SSH 私钥，但没有配置非 root 部署账号。

建议按以下顺序处理（**需要服务器管理权限**）：

1. 在 VPS 上创建有 `sudo` 的普通部署账号（或使用已有的非 root 账号），配置公钥登录；
2. 设置 `AETHER_VPS_USER=<非 root 账号>`、`AETHER_VPS_KEY=<私钥路径>`；
3. 先跑只读校验：
   ```bash
   python scripts/deploy-site.py --check-only --skip-build
   ```
4. 确认后再部署：
   ```bash
   python scripts/deploy-site.py
   ```
5. 部署后重新抓取首页/FAQ，确认 52,544 / 77 / 793 已上线，并复查安全响应头；
6. 按安全审计建议轮换曾在聊天记录中出现过的 VPS 口令。

## 五、结论边界

- 本报告**没有**修改生产服务器，也没有声称线上已经更新。
- 线上站点仍可用、TLS 与安全头正常；问题是公开的规模/测试数字落后。
- 本地站点已修正并通过门禁；等非 root 部署凭据就绪后，按第四节执行即可同步线上。
