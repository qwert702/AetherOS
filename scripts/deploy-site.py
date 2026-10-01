#!/usr/bin/env python3
"""把 AetherOS 官网部署到 VPS（自动适配 Debian / RHEL 系布局）。

## 一条命令

    python scripts/deploy-site.py                 # 构建 + 上传 + 加 vhost + 回归验证
    python scripts/deploy-site.py --tls           # 额外用 certbot 签发 HTTPS
    python scripts/deploy-site.py --check-only    # 只做线上校验，不改服务器

## 凭据（只从环境变量读，绝不写进文件）

    AETHER_VPS_HOST / AETHER_VPS_USER / AETHER_VPS_PASSWORD（或 AETHER_VPS_KEY）

## 这台生产服务器的真实情况（2026-10-01 侦察）

    Alibaba Cloud Linux 3（RHEL 系，用 dnf/yum，**没有 apt**）
    nginx 已装且运行中，占用 80/443
    配置在 /etc/nginx/conf.d/*.conf（**不是** sites-available/sites-enabled）
    站点根约定 /var/www/<域名>
    上面跑着 cbnac.com / school / admin.school / api / blog / dili / dsh.* / teach / storage 等多个业务

所以本脚本的纪律是：
  1. **不装 nginx、不重启服务**，只新增一个 `conf.d/aether.cbnac.com.conf`
  2. 只碰 `/var/www/aether.cbnac.com` 与那一个新配置文件；已有文件先备份
  3. `nginx -t` 通过才 reload
  4. reload 后**回归验证**：既有站点（cbnac.com 等）必须仍然返回原来的状态码 ——
     在生产机器上加站点，最怕的是把别人的业务搞挂

## 为什么不把 ISO 放这台机器

38.5 MB × 每次下载都是实打实的出网流量（阿里云按流量计费）。默认不传 ISO；
要传就加 `--with-iso`（中国用户下载会明显更快，但会产生流量费用 —— 你自己权衡）。
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
DOMAIN = "aether.cbnac.com"
PY = sys.executable

#: 回归验证：这些既有站点在 reload 前后必须返回同样的状态码
REGRESSION = ["https://cbnac.com/", "https://school.cbnac.com/", "https://api.cbnac.com/"]


def env(name: str, default: str = "") -> str:
    v = os.environ.get(name, "").strip()
    if not v and not default:
        sys.exit(f"缺环境变量 {name}（不要写进文件）：setx {name} \"...\" 后重开终端")
    return v or default


def run(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, encoding="utf-8",
                          errors="replace", **kw)


def build() -> None:
    print("== 1) 本地构建站点 ==")
    r = run([PY, str(ROOT / "scripts" / "gen-site.py"), "--refresh"])
    print(r.stdout.strip() or r.stderr.strip()[-800:])
    if r.returncode != 0:
        sys.exit("构建失败")
    r = run([PY, str(ROOT / "scripts" / "gen-site.py"), "--check"])
    print(r.stdout.strip() or r.stderr.strip()[-400:])
    if r.returncode != 0:
        sys.exit("站点门禁未过")


def connect(host: str, user: str):
    try:
        import paramiko
    except ImportError:
        sys.exit("需要 paramiko（用项目 venv 的 python 运行本脚本）")
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    key = os.environ.get("AETHER_VPS_KEY", "").strip()
    print(f"== 2) 连接 {user}@{host} ==")
    if key:
        c.connect(host, username=user, key_filename=key, timeout=25)
    else:
        c.connect(host, username=user, password=env("AETHER_VPS_PASSWORD"), timeout=25)
    return c


def sh(c, cmd: str, timeout: int = 600, check: bool = True) -> str:
    _i, o, e = c.exec_command(cmd, timeout=timeout)
    out = o.read().decode("utf-8", "replace").strip()
    err = e.read().decode("utf-8", "replace").strip()
    code = o.channel.recv_exit_status()
    if check and code != 0:
        sys.exit(f"远端命令失败（{code}）：{cmd}\n{(err or out)[-800:]}")
    return out or err


def probe_layout(c) -> dict:
    """识别包管理器与 nginx 配置布局 —— 决定后面所有路径。"""
    layout = {}
    layout["pkg"] = "dnf" if sh(c, "command -v dnf >/dev/null && echo dnf || echo apt") == "dnf" else "apt"
    layout["conf_dir"] = "/etc/nginx/conf.d" if sh(
        c, "[ -d /etc/nginx/conf.d ] && echo yes || echo no") == "yes" else "/etc/nginx/conf.d"
    layout["nginx"] = sh(c, "command -v nginx || echo none")
    layout["certbot"] = sh(c, "command -v certbot || echo none")
    layout["root"] = f"/var/www/{DOMAIN}"
    print(f"  包管理器={layout['pkg']}  配置目录={layout['conf_dir']}  "
          f"nginx={'有' if layout['nginx'] != 'none' else '无'}  "
          f"certbot={'有' if layout['certbot'] != 'none' else '无'}")
    return layout


def baseline(c) -> dict:
    """reload 前的基线：既有站点状态码（用于回归对比）。"""
    print("== 3) 记录既有站点基线 ==")
    base = {}
    for url in REGRESSION:
        code = sh(c, f"curl -s -o /dev/null -w '%{{http_code}}' -m 12 {url} || echo ERR", check=False)
        base[url] = code.strip()
        print(f"  {url} → {base[url]}")
    return base


def upload(c, layout: dict, with_iso: bool) -> None:
    print("== 4) 上传站点 ==")
    sftp = c.open_sftp()

    def ensure(path: str) -> None:
        cur = ""
        for part in path.strip("/").split("/"):
            cur += "/" + part
            try:
                sftp.stat(cur)
            except OSError:
                sftp.mkdir(cur)

    ensure(layout["root"])
    n = 0
    for p in sorted(SITE.rglob("*")):
        rel = p.relative_to(SITE).as_posix()
        if p.is_dir():
            ensure(f"{layout['root']}/{rel}")
            continue
        sftp.put(str(p), f"{layout['root']}/{rel}")
        n += 1
    print(f"  上传 {n} 个文件 → {layout['root']}")
    if with_iso:
        iso = ROOT / "aetheros-0.1-amd64.iso"
        if iso.exists():
            ensure(f"{layout['root']}/dl")
            print(f"  上传 ISO（{iso.stat().st_size / 1048576:.1f} MB）…")
            sftp.put(str(iso), f"{layout['root']}/dl/aetheros-0.1-amd64.iso")
        else:
            print("  警告：本地没有 ISO，跳过")
    sftp.close()
    sh(c, f"chown -R nginx:nginx {layout['root']} 2>/dev/null || chown -R www-data:www-data {layout['root']}; "
          f"find {layout['root']} -type d -exec chmod 755 {{}} +; "
          f"find {layout['root']} -type f -exec chmod 644 {{}} +")


def write_vhost(c, layout: dict, with_iso: bool) -> None:
    """新增一个 vhost —— 只写 HTTP 段，TLS 交给 certbot（它自己会插入 443 段）。"""
    print("== 5) 新增 vhost（只加这一个文件，不动其它配置）==")
    conf = f"{layout['conf_dir']}/{DOMAIN}.conf"
    dl = ""
    if with_iso:
        dl = f"""
    # 国内下载：ISO 放在本机（会产出流量费用，--with-iso 才开）
    location /dl/ {{
        alias {layout['root']}/dl/;
        add_header Cache-Control "public, max-age=86400";
    }}
"""
    body = f"""# {DOMAIN} —— AetherOS 官网
# 由 scripts/deploy-site.py 生成（2026-10-01）。与其它站点完全独立：
# 只用一个 server_name 精确匹配，不设 default_server，不影响任何既有业务。
server {{
    listen 80;
    listen [::]:80;
    server_name {DOMAIN};

    root {layout['root']};
    index index.html;
    charset utf-8;

    # certbot 的 ACME 校验（既有配置里已有 /var/www/certbot）
    location /.well-known/acme-challenge/ {{ root /var/www/certbot; }}
{dl}
    # 安全头
    add_header X-Content-Type-Options "nosniff" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;
    add_header Content-Security-Policy "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; base-uri 'self'; frame-ancestors 'none'" always;

    gzip on;
    gzip_vary on;
    gzip_min_length 512;
    gzip_types text/plain text/css application/javascript application/json image/svg+xml application/xml;

    location /assets/img/ {{ expires 30d; add_header Cache-Control "public, max-age=2592000"; access_log off; }}
    location ~* \\.(css|svg|woff2?)$ {{ expires 7d; add_header Cache-Control "public, max-age=604800"; }}
    location ~* \\.html$ {{ expires 10m; add_header Cache-Control "public, max-age=600, must-revalidate"; }}

    location = /sitemap.xml {{ access_log off; }}
    location = /robots.txt  {{ access_log off; }}

    error_page 404 /404.html;

    # 仓库文件与隐藏文件不该被访问
    location ~ /\\.(?!well-known) {{ deny all; }}
    location ~* \\.(md|rs|toml|py|sh|json)$ {{ deny all; }}
}}
"""
    sftp = c.open_sftp()
    if sh(c, f"[ -f {conf} ] && echo yes || echo no", check=False) == "yes":
        sh(c, f"cp -a {conf} {conf}.bak-$(date +%Y%m%d-%H%M%S)")
        print("  已备份既有同名配置")
    with sftp.file(conf, "w") as f:
        f.write(body)
    sftp.close()
    print(f"  写入 {conf}")
    print("  nginx -t:", sh(c, "nginx -t 2>&1"))


def reload_and_regress(c, base: dict) -> None:
    print("== 6) reload + 生产回归验证 ==")
    sh(c, "systemctl reload nginx || nginx -s reload")
    print("  nginx 已 reload")
    bad = []
    for url, code in base.items():
        now = sh(c, f"curl -s -o /dev/null -w '%{{http_code}}' -m 12 {url} || echo ERR", check=False).strip()
        ok = now == code or (code == "ERR" and now != "ERR")
        print(f"  {url} → {code} → {now}  {'✓' if ok else '✗ 变了！'}")
        if not ok:
            bad.append(url)
    if bad:
        sys.exit(f"⚠️ 既有站点状态码发生变化（{', '.join(bad)}）—— 请立刻检查 nginx 配置！")
    print("  既有站点全部未受影响 ✓")


def tls(c, layout: dict) -> None:
    print("== 7) HTTPS 证书 ==")
    if layout["certbot"] == "none":
        print("  服务器上没有 certbot，跳过（可手工签发后再补 443 段）")
        return
    if "nginx" not in sh(c, "certbot plugins 2>/dev/null | grep -i nginx || echo none", check=False):
        print("  装 certbot 的 nginx 插件…")
        sh(c, f"{layout['pkg']} install -y python3-certbot-nginx || "
              f"{layout['pkg']} install -y certbot-nginx || echo '插件安装失败，稍后手工处理'", check=False)
    out = sh(c, f"certbot --nginx -d {DOMAIN} --non-interactive --agree-tos "
                f"--register-unsafely-without-email --redirect", timeout=600, check=False)
    print(" ", out[-700:])


def verify(c) -> None:
    print("== 8) 线上校验 ==")
    for url in (f"http://{DOMAIN}/", f"https://{DOMAIN}/", f"https://{DOMAIN}/download.html",
                f"https://{DOMAIN}/en/"):
        code = sh(c, f"curl -s -o /dev/null -w '%{{http_code}}' -m 15 {url} || echo ERR", check=False)
        print(f"  {url} → {code.strip()}")
    head = sh(c, f"curl -sI -m 15 https://{DOMAIN}/ | head -12", check=False)
    print("  响应头：\n" + "\n".join("    " + l for l in head.splitlines()[:10]))


def main() -> int:
    ap = argparse.ArgumentParser(description="部署 AetherOS 官网到 VPS")
    ap.add_argument("--tls", action="store_true", help="用 certbot 签发 HTTPS 证书")
    ap.add_argument("--check-only", action="store_true", help="只做线上校验")
    ap.add_argument("--with-iso", action="store_true", help="把 ISO 也传到本机 /dl/（产生流量费用）")
    ap.add_argument("--skip-build", action="store_true", help="跳过本地构建（用已有 site/）")
    args = ap.parse_args()

    host = env("AETHER_VPS_HOST")
    user = env("AETHER_VPS_USER", "root")
    if not args.skip_build:
        build()
    c = connect(host, user)
    try:
        layout = probe_layout(c)
        if args.check_only:
            verify(c)
            return 0
        base = baseline(c)
        upload(c, layout, args.with_iso)
        write_vhost(c, layout, args.with_iso)
        reload_and_regress(c, base)
        if args.tls:
            tls(c, layout)
        verify(c)
    finally:
        c.close()
    print("\n完成。")
    print(f"  · 站点：https://{DOMAIN}/（TLS 若未签，先 --tls）")
    print("  · 建议：部署后轮换 VPS 密码（它曾在聊天记录里出现过）")
    print("  · 注意：aether.cbnac.com 是泛解析，镜像子域也会落到这台机器 —— 需要时再加 vhost")
    return 0


if __name__ == "__main__":
    sys.exit(main())
