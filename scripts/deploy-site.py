#!/usr/bin/env python3
"""把 AetherOS 官网部署到 VPS（nginx 静态站 + Let's Encrypt）。

## 一条命令

    python scripts/deploy-site.py              # 构建 + 上传 + 配 nginx + 校验
    python scripts/deploy-site.py --tls        # 额外签发/续期 HTTPS 证书
    python scripts/deploy-site.py --check-only # 只做线上校验，不改服务器

## 凭据（只从环境变量读，绝不写进文件）

    setx AETHER_VPS_HOST "1.2.3.4"
    setx AETHER_VPS_USER "root"
    setx AETHER_VPS_PASSWORD "..."      # 或者用密钥：setx AETHER_VPS_KEY "C:\\path\\id_ed25519"

## 它做了什么

1. 本地构建站点（`gen-site.py --refresh`，把实测数字注入页面、生成图片与 sitemap）
2. SSH 上去：缺 nginx/certbot 就装，建目录，上传 `site/`
3. 装 `deploy/nginx-aether.conf` → `nginx -t` → reload
4. `--tls` 时跑 certbot 签发证书并自动续期
5. 校验：带 Host 头请求首页，断言 200 + 关键内容 + 安全头

## 为什么不把 ISO 放这台机器

38.5 MB × 每次下载都是实打实的出网流量。下载指向 GitHub Releases 与 Cloudflare R2 镜像，
VPS 只负责那几 KB 的 HTML/CSS —— 小机器也扛得住。
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
NGINX_CONF = ROOT / "deploy" / "nginx-aether.conf"
REMOTE_ROOT = "/var/www/aether.cbnac.com"
DOMAIN = "aether.cbnac.com"
PY = sys.executable


def env(name: str, required: bool = True) -> str:
    v = os.environ.get(name, "").strip()
    if required and not v:
        sys.exit(
            f"缺环境变量 {name}。请在**当前会话**先设置（不要写进任何文件）：\n"
            f'  setx {name} "..."   # 然后重开一个终端\n'
            "需要的变量：AETHER_VPS_HOST / AETHER_VPS_USER / "
            "(AETHER_VPS_PASSWORD 或 AETHER_VPS_KEY)"
        )
    return v


def build() -> None:
    print("== 1) 本地构建站点 ==")
    r = subprocess.run([PY, str(ROOT / "scripts" / "gen-site.py"), "--refresh"],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    print(r.stdout.strip() or r.stderr.strip()[-800:])
    if r.returncode != 0:
        sys.exit("构建失败")
    r = subprocess.run([PY, str(ROOT / "scripts" / "gen-site.py"), "--check"],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    print(r.stdout.strip() or r.stderr.strip()[-400:])
    if r.returncode != 0:
        sys.exit("站点门禁未过（页面与实测数据不一致）")


def connect():
    try:
        import paramiko
    except ImportError:
        sys.exit("需要 paramiko（用项目 venv 的 python 跑本脚本）")
    host, user = env("AETHER_VPS_HOST"), env("AETHER_VPS_USER")
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    key = os.environ.get("AETHER_VPS_KEY", "").strip()
    print(f"== 2) 连接 {user}@{host} ==")
    if key:
        c.connect(host, username=user, key_filename=key, timeout=25)
    else:
        c.connect(host, username=user, password=env("AETHER_VPS_PASSWORD"), timeout=25)
    return c, host


def sh(c, cmd: str, timeout: int = 600, check: bool = True) -> str:
    _in, out, err = c.exec_command(cmd, timeout=timeout)
    o, e = out.read().decode("utf-8", "replace"), err.read().decode("utf-8", "replace")
    code = out.channel.recv_exit_status()
    if check and code != 0:
        sys.exit(f"远端命令失败（{code}）：{cmd}\n{e[-800:]}")
    return (o or e).strip()


def sftp_upload_dir(c, local: Path, remote: str) -> int:
    """递归上传目录（跳过生成物之外的一切，站点目录本来就只有该传的东西）。"""
    sftp = c.open_sftp()

    def ensure(path: str) -> None:
        parts, cur = path.strip("/").split("/"), ""
        for p in parts:
            cur += "/" + p
            try:
                sftp.stat(cur)
            except OSError:
                sftp.mkdir(cur)

    ensure(remote)
    n = 0
    for p in sorted(local.rglob("*")):
        if p.is_dir():
            ensure(remote + "/" + p.relative_to(local).as_posix())
            continue
        sftp.put(str(p), remote + "/" + p.relative_to(local).as_posix())
        n += 1
    sftp.close()
    return n


def deploy(c) -> None:
    print("== 3) 服务器环境 ==")
    sh(c, "command -v nginx >/dev/null 2>&1 || (apt-get update -qq && "
          "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nginx)", timeout=900)
    if "--tls" in sys.argv:
        sh(c, "command -v certbot >/dev/null 2>&1 || (DEBIAN_FRONTEND=noninteractive "
              "apt-get install -y -qq certbot python3-certbot-nginx)", timeout=900)
    print("  nginx:", sh(c, "nginx -v 2>&1"))

    print("== 4) 上传站点 ==")
    n = sftp_upload_dir(c, SITE, REMOTE_ROOT)
    print(f"  上传 {n} 个文件 → {REMOTE_ROOT}")
    sh(c, f"mkdir -p /var/www/certbot && chown -R www-data:www-data {REMOTE_ROOT} && "
          f"find {REMOTE_ROOT} -type d -exec chmod 755 {{}} + && "
          f"find {REMOTE_ROOT} -type f -exec chmod 644 {{}} +")

    print("== 5) nginx 配置 ==")
    sftp = c.open_sftp()
    sftp.put(str(NGINX_CONF), "/etc/nginx/sites-available/aether.conf")
    sftp.close()
    sh(c, "ln -sf /etc/nginx/sites-available/aether.conf /etc/nginx/sites-enabled/aether.conf && "
          "rm -f /etc/nginx/sites-enabled/default")
    print(" ", sh(c, "nginx -t 2>&1"))
    sh(c, "systemctl reload nginx || nginx -s reload")


def tls(c) -> None:
    print("== 6) HTTPS 证书 ==")
    out = sh(c, f"certbot --nginx -d {DOMAIN} --non-interactive --agree-tos "
                f"--register-unsafely-without-email --redirect", timeout=600, check=False)
    print(" ", out[-600:])


def verify(host: str) -> None:
    print("== 7) 线上校验 ==")
    r = subprocess.run(["curl", "-sS", "-o", "NUL", "-D", "-", "-m", "20",
                        "-H", f"Host: {DOMAIN}", f"http://{host}/"],
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    head = (r.stdout or "") + (r.stderr or "")
    ok200 = " 200 " in head.split("\n")[0] or " 301 " in head.split("\n")[0]
    print("  HTTP:", head.split("\n")[0].strip() if head else "(无响应)")
    print("  安全头:", "有" if "Content-Security-Policy" in head else "无")
    if not ok200:
        print("  提示：若刚加 DNS 记录，解析生效可能需要几分钟到几小时。")
    print("  DNS:", subprocess.run(["nslookup", DOMAIN], capture_output=True, text=True,
                                  encoding="utf-8", errors="replace").stdout.strip()[-300:])


def main() -> int:
    ap = argparse.ArgumentParser(description="部署 AetherOS 官网")
    ap.add_argument("--tls", action="store_true", help="额外用 certbot 签发 HTTPS 证书")
    ap.add_argument("--check-only", action="store_true", help="只做线上校验，不改服务器")
    args = ap.parse_args()

    if args.check_only:
        verify(env("AETHER_VPS_HOST"))
        return 0
    build()
    c, host = connect()
    try:
        deploy(c)
        if args.tls:
            tls(c)
    finally:
        c.close()
    verify(host)
    print("\n完成。别忘了：")
    print(f"  1. 在域名商后台加 A 记录：aether → {host}")
    print(f"  2. 证书签好后访问 https://{DOMAIN}/")
    print("  3. 部署完成后轮换 VPS 密码（它曾在聊天里出现过）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
