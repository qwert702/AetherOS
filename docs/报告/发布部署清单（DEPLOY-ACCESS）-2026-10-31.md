# 发布部署清单（DEPLOY-ACCESS）— 2026-10-31

> 这份文档回答：**要把 AetherOS 官网部署到 `aether.cbnac.com`，需要给执行者什么账号/凭据。**
> 原则：不直连 root、不把私钥内容发到聊天里、不在仓库保存密码。

## 一、需要提供的三样东西

| 项目 | 要求 | 当前状态 |
|---|---|---|
| VPS 地址 | 已有 `AETHER_VPS_HOST=47.98.207.149` | 已设置 |
| 非 root 账号 | 专用账号，例如 `aether-deploy`，有 `sudo -n` 权限 | **未创建**；当前只有 `root` |
| SSH 私钥路径 | 本机私钥文件路径，例如 `C:\Users\cbn\.ssh\id_ed25519` | 本机存在；尚未配置到 VPS 账号 |

**不需要**：

- 不需要 root 密码；
- 不需要把私钥内容贴给我；
- 不需要 GitHub Token（代码推送已经可用）。

只需要在 VPS 上创建账号、安装公钥，然后在本机设置三个环境变量：

```powershell
$env:AETHER_VPS_HOST = "47.98.207.149"
$env:AETHER_VPS_USER = "aether-deploy"
$env:AETHER_VPS_KEY  = "$HOME\.ssh\id_ed25519"
```

## 二、在 VPS 上创建部署账号（只需 root 执行一次）

> 以下命令按 Debian/Ubuntu 写；RHEL/CentOS 把 `sudo` 组换成 `wheel`。
> 请先在本机查看公钥（公钥可以公开，私钥不要发）：
>
> ```powershell
> Get-Content "$HOME\.ssh\id_ed25519.pub"
> ```

在 VPS 上以 root 执行，把最后一行替换成本机公钥：

```bash
id aether-deploy >/dev/null 2>&1 || useradd -m -s /bin/bash aether-deploy
install -d -m 700 -o aether-deploy -g aether-deploy /home/aether-deploy/.ssh
cat > /home/aether-deploy/.ssh/authorized_keys <<'EOF'
ssh-ed25519 AAAA...你的本机公钥... cbn@your-machine
EOF
chown aether-deploy:aether-deploy /home/aether-deploy/.ssh/authorized_keys
chmod 600 /home/aether-deploy/.ssh/authorized_keys
usermod -aG sudo aether-deploy

# deploy-site.py 目前用 `sudo -n bash -c` 执行部署命令，因此需要免密 sudo。
# 这是当前脚本的权限事实；如果要做严格最小权限，需要先改造脚本再另配 sudoers。
echo 'aether-deploy ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/aether-deploy
chmod 440 /etc/sudoers.d/aether-deploy
visudo -cf /etc/sudoers.d/aether-deploy
```

## 三、本机验证

先跑前置检查（不会连接服务器、不会读取私钥内容）：

```powershell
python scripts/deploy-preflight.py
```

预期全部 PASS。当前实际结果是：

```text
[PASS] AETHER_VPS_HOST  (47.98.207.149)
[PASS] AETHER_VPS_USER  (root)
[FAIL] 非 root 账号  (当前是 root；deploy-site.py 默认拒绝 root 直连)
[FAIL] AETHER_VPS_KEY  (未设置；推荐密钥认证，不建议用口令)
```

所以现在还不能部署；这不是代码问题，是访问方式不满足安全策略。

## 四、部署命令

前置检查通过后，先做**只读**校验：

```powershell
python scripts/deploy-site.py --check-only --skip-build
```

确认线上当前状态和路径都正确，再正式部署：

```powershell
python scripts/deploy-site.py
```

部署后必须跑：

```powershell
python scripts/site-health-check.py
```

要求：线上首页/FAQ 出现 **52,544 行 / 77 文件 / 793 测试**，安全响应头仍存在，退出码为 0。

## 五、需要访问的具体位置

`deploy-site.py` 会按实际 nginx 布局识别路径，通常涉及：

| 路径/操作 | 用途 |
|---|---|
| `/var/www/aether.cbnac.com/` | 站点根目录（上传 HTML/assets） |
| `/var/www/aether.cbnac.com/updates/` | 更新包与签名清单 |
| 正在运行的 nginx 的 `conf.d` 或宝塔 vhost 文件 | 写入/校验 `aether.cbnac.com` vhost |
| `nginx -t` / `nginx -T` | 用**运行中的** nginx 二进制和配置校验 |
| reload nginx（`systemctl` / `nginx -s reload` / `kill -HUP`） | 让配置生效；脚本有三种回退 |
| `certbot`（仅 `--tls`） | 证书签发/续期，可选 |

## 六、其他工作需要的凭据

| 工作 | 需要什么 | 不需要什么 |
|---|---|---|
| 重建 ISO | Linux Buildroot 构建机的 SSH 地址/账号/密钥，或本机可用的 Linux 环境 | VPS 凭据 |
| 物理机验收 | 一台 x86_64 物理机 + 可拍照/记录串口的方式 | 账号密码（本机操作即可） |
| 真实 LLM 验证 | 自建网关的 Base URL + 临时 API Key（放环境变量） | 不要把上游 Key 写进系统或仓库 |
| GitHub 发布 | 已有推送权限 | 不需要把 Token 发到聊天 |

## 七、安全边界

- 不允许 `root` 直接 SSH（`deploy-site.py` 会主动拒绝）。
- 不允许把私钥、VPS 密码、API Key 写进仓库或文档。
- 不允许关闭 TLS 校验或使用 `AutoAddPolicy` 式主机密钥绕过；部署脚本使用 known_hosts/TOFU。
- 部署后按安全审计建议轮换曾在聊天记录中出现过的 VPS 口令。

## 八、当前结论

**你只需要给我一个非 root 的 VPS 账号（或创建好该账号并配置本机公钥），不需要给我 root 密码或私钥内容。** 账号就绪后，我会按以下顺序执行：

1. `deploy-preflight.py` 全 PASS；
2. `deploy-site.py --check-only --skip-build`；
3. `deploy-site.py` 正式部署；
4. `site-health-check.py` 验证线上数字与安全头；
5. 把部署结果补进站点巡检报告。
