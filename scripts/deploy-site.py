#!/usr/bin/env python3
"""把 AetherOS 官网部署到 VPS（自动适配 Debian / RHEL 系布局）。

## 一条命令

    python scripts/deploy-site.py                 # 构建 + 上传 + 加 vhost + 回归验证
    python scripts/deploy-site.py --tls           # 额外用 certbot 签发 HTTPS
    python scripts/deploy-site.py --check-only    # 只做线上校验，不改服务器

## 凭据（只从环境变量读，绝不写进文件）

    AETHER_VPS_HOST / AETHER_VPS_USER / AETHER_VPS_KEY（私钥路径，推荐）
    AETHER_VPS_PASSWORD（口令；需显式 AETHER_VPS_ALLOW_PASSWORD=1 才允许）

纪律（2026-10-02 安全审计 H-5 之后）：
  * 主机密钥**首次记录、之后严格校验**（TOFU，同 `scripts/vm.py`）——
    此前用的 `AutoAddPolicy` 会静默接受任何主机密钥，等于对中间人开门
  * 默认**拒绝以 root 直连**（需显式 AETHER_VPS_ALLOW_ROOT=1）
  * 默认**要求密钥认证**（口令需显式 AETHER_VPS_ALLOW_PASSWORD=1）
  * 本脚本**只做代码级校验**：不做任何真实部署动作，除非你显式运行它并给出凭据

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
import base64
import hashlib
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
DOMAIN = "aether.cbnac.com"
PY = sys.executable

# paramiko 是可选依赖：模块级导入一次，供各辅助函数共用类型（缺了在 connect 里统一报错）
try:
    import paramiko
except ImportError:  # pragma: no cover - 只在没装 paramiko 时走到
    paramiko = None  # type: ignore[assignment]

#: 主机密钥记录（TOFU；已 gitignore）。首次连接写入，之后密钥变了就拒连。
_DEFAULT_KNOWN_HOSTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), ".vps_known_hosts")

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


def _known_hosts_path() -> str:
    return os.environ.get("AETHER_VPS_KNOWN_HOSTS") or _DEFAULT_KNOWN_HOSTS


def _load_known_hosts(client) -> bool:
    """加载主机密钥记录。返回是否**已有**记录（决定严格校验还是首次信任）。"""
    path = _known_hosts_path()
    if not os.path.isfile(path) or os.path.getsize(path) == 0:
        return False
    try:
        client.load_host_keys(path)
    except (OSError, paramiko.SSHException) as exc:
        sys.exit(f"主机密钥记录读取失败（{path}）：{exc}")
    return True


def _remember_host_key(client) -> None:
    """首次连接：打印指纹并落盘，之后一律严格校验。"""
    transport = client.get_transport()
    if transport is None:
        sys.exit("连接已断开，无法记录主机密钥")
    key = transport.get_remote_server_key()
    fingerprint = base64.b64encode(hashlib.sha256(key.asbytes()).digest()).decode().rstrip("=")
    path = _known_hosts_path()
    try:
        client.save_host_keys(path)
    except OSError as exc:
        sys.exit(f"写入主机密钥记录失败（{path}）：{exc}")
    print(f"  首次连接：已记录主机密钥 {key.get_name()} SHA256:{fingerprint} → {path}")
    print("        可用 `ssh-keyscan <host> | ssh-keygen -lf -` 交叉核对后再继续")


def connect(host: str, user: str):
    """连接生产机。**默认严格校验主机密钥、默认非 root、默认用密钥认证。**

    为什么这么改（2026-10-02 安全审计 H-5）：此前是
    `AutoAddPolicy()` + 默认 `root` + 没密钥就走口令 ——
    中间人只要能把流量引到自己那里就能拿到 root 口令与整套站点内容；
    而目标机器上跑着**多个真实业务**（cbnac.com 等），代价不只是本站。
    """
    try:
        import paramiko as _p  # noqa: F401  （已在模块级导入；这里给出可执行的缺失提示）
    except ImportError:
        sys.exit("需要 paramiko（用项目 venv 的 python 运行本脚本）")

    c = paramiko.SSHClient()
    has_record = _load_known_hosts(c)
    # 首次连接：记录指纹（TOFU）；之后密钥变了**直接拒连**，不静默接受新密钥
    c.set_missing_host_key_policy(paramiko.RejectPolicy() if has_record else paramiko.AutoAddPolicy())

    key = os.environ.get("AETHER_VPS_KEY", "").strip()
    allow_password = os.environ.get("AETHER_VPS_ALLOW_PASSWORD", "") == "1"
    if not key and not allow_password:
        sys.exit(
            "缺少凭据或凭据方式不安全：请设 AETHER_VPS_KEY=<私钥路径>（推荐）。\n"
            "        坚持用口令认证需显式设 AETHER_VPS_ALLOW_PASSWORD=1"
            "（口令经 SSH 加密传输，但它可被复用、且会留在 shell 历史/进程环境里）"
        )
    if user == "root":
        if os.environ.get("AETHER_VPS_ALLOW_ROOT", "") != "1":
            sys.exit(
                f"拒绝以 root 直连 {host}：请用有 sudo 的普通账号（AETHER_VPS_USER），\n"
                "        或显式设 AETHER_VPS_ALLOW_ROOT=1 表示你确实要这么做。\n"
                "        这台机器上跑着多个业务，root 会话的一次误操作代价过大。"
            )
        print("  ⚠️ 已允许 root 直连（AETHER_VPS_ALLOW_ROOT=1）")

    print(f"== 2) 连接 {user}@{host}（主机密钥{'严格校验' if has_record else '首次记录'}）==")
    if key:
        c.connect(host, username=user, key_filename=os.path.expanduser(key), timeout=25,
                  banner_timeout=30, auth_timeout=30)
    else:
        c.connect(host, username=user, password=env("AETHER_VPS_PASSWORD"),
                  timeout=25, banner_timeout=30, auth_timeout=30)
    if not has_record:
        _remember_host_key(c)
    return c


def sudo(c, cmd: str, timeout: int = 600, check: bool = True) -> str:
    """以 root 执行一条命令：普通账号 + `sudo -n`。

    审计 H-5 之后，脚本默认不再用 root 直连（见 `connect()`），需要 root 的步骤
    都走这里。`-n` 是**非交互**：没配免密 sudo 就立刻失败并给出提示，
    而不是把密码提示卡在远端会话里。
    """
    quoted = cmd.replace("'", "'\\''")
    return sh(c, f"sudo -n bash -c '{quoted}'", timeout=timeout, check=check)


def sh(c, cmd: str, timeout: int = 600, check: bool = True) -> str:
    _i, o, e = c.exec_command(cmd, timeout=timeout)
    out = o.read().decode("utf-8", "replace").strip()
    err = e.read().decode("utf-8", "replace").strip()
    code = o.channel.recv_exit_status()
    if check and code != 0:
        sys.exit(f"远端命令失败（{code}）：{cmd}\n{(err or out)[-800:]}")
    return out or err


#: `nginx -T` 里每个文件来源的注释头：`# configuration file /etc/nginx/nginx.conf:`
_NGINX_T_FILE_RE = re.compile(r"^#\s*configuration file\s+(\S+):\s*$")

#: master 进程命令行：`nginx: master process /path/nginx -c /path/nginx.conf`
_NGINX_MASTER_RE = re.compile(r"nginx: master process\s+")


def running_nginx(ps_line: str) -> tuple[str | None, str | None]:
    """从 master 进程命令行里取出 **(二进制路径, 配置文件路径)**。

    ## 为什么两样都要问它（2026-10-15 生产事故的根因）

    这台生产机上有**两套 nginx**，而且 `nginx -t`/`nginx -T` 默认用的那个**不是正在服务的**：

    * **跑着的**：`/www/server/nginx/sbin/nginx -c /www/server/nginx/conf/nginx.conf`
      （宝塔面板的构建，**编了 `stream` 模块**），只 `include
      /www/server/panel/vhost/nginx/*.conf`；
    * **PATH 里的 `nginx`**：nginx/1.24.0，**没编 `stream`** ✗，默认配置是
      `/etc/nginx/nginx.conf`（`include /etc/nginx/conf.d/*.conf`）。

    于是老逻辑"写 `/etc/nginx/conf.d/` + 用 `nginx -t` 校验"在本机是**两层假绿**：
    `nginx -t` 说 OK（它测的是另一个文件）、站点也正常（真正生效的是宝塔那份 vhost）、
    脚本还打印"部署成功" —— 而写进去的配置**永远不会被加载**。
    更糟的是：拿 PATH 里那个二进制去测宝塔配置，会报 `unknown directive "stream"` ——
    **一个根本不存在的故障**。

    结论：**校验与 dump 都必须用同一个二进制 + 同一份配置**，否则测的不是正在服务的东西。
    返回 `(None, None)` 表示命令行认不出来（调用方回退到 PATH 里的 `nginx` + 默认配置）。
    """
    m = _NGINX_MASTER_RE.search(ps_line or "")
    if not m:
        return (None, None)
    rest = ps_line[m.end():].strip()
    if not rest:
        return (None, None)
    binary = rest.split()[0]
    mc = re.search(r"(?:^|\s)-c\s+(\S+)", rest)
    return (binary, mc.group(1) if mc else None)



def parse_vhost_file(nginx_t: str, domain: str) -> str | None:
    """从 `nginx -T` 的输出里找出**真正生效的**那个 vhost 文件。

    `nginx -T` 会把每个文件的来源打成 `# configuration file <路径>:` 注释头，
    所以"哪个注释头之后出现了 `server_name <域名>;`"就是生效文件。

    **注意**：只有配上 `-c <运行中的配置>`（见 `running_nginx_conf`）才代表真实生效的那份；
    不加 `-c` 时它测的是 nginx 的编译期默认配置，可能与正在服务的进程不是同一个文件。
    """
    current: str | None = None
    for raw in nginx_t.splitlines():
        line = raw.strip()
        m = _NGINX_T_FILE_RE.match(line)
        if m:
            current = m.group(1)
            continue
        if current and re.match(rf"^server_name\s+[^;]*\b{re.escape(domain)}\b", line):
            return current
    return None


def selftest() -> int:
    """自检：**不需要服务器、不需要凭据** —— 只验纯解析逻辑。

    为什么要有它：`parse_vhost_file` 的正确性只能靠真实服务器验证，而生产机不会
    随时可动（也不该为了测试去动）。把"从 `nginx -T` 里认出生效文件"这条判据钉成
    自检，本机就能跑 —— 这正是当初那个"写进死文件还报成功"的坑的守卫。
    """
    failed = 0

    def check(name: str, ok: bool, detail: str = "") -> None:
        nonlocal failed
        print(f"  [{'OK  ' if ok else 'FAIL'}] {name}{('：' + detail) if detail else ''}")
        if not ok:
            failed += 1

    # ① 常规布局：conf.d 里的文件定义了本站
    common = (
        "# configuration file /etc/nginx/nginx.conf:\n"
        "user nginx;\n"
        "# configuration file /etc/nginx/conf.d/site.conf:\n"
        "server {\n    listen 80;\n    server_name aether.cbnac.com;\n}\n"
    )
    check("常规布局能找出生效文件",
          parse_vhost_file(common, "aether.cbnac.com") == "/etc/nginx/conf.d/site.conf")

    # ② 宝塔布局：只有 panel/vhost 被 include，conf.d 里那份是死文件
    bt = (
        "# configuration file /www/server/nginx/conf/nginx.conf:\n"
        "include /www/server/panel/vhost/nginx/*.conf;\n"
        "# configuration file /www/server/panel/vhost/nginx/aether.cbnac.com.conf:\n"
        "server {\n    server_name aether.cbnac.com;\n    listen 443 ssl;\n}\n"
        "# configuration file /etc/nginx/conf.d/aether.cbnac.com.conf:\n"
        "server {\n    server_name aether.cbnac.com;\n}\n"
    )
    check("宝塔布局返回**第一个**（真正被 include 的）文件",
          parse_vhost_file(bt, "aether.cbnac.com") == "/www/server/panel/vhost/nginx/aether.cbnac.com.conf")

    # ③ 多域名共存时不能认错
    multi = (
        "# configuration file /etc/nginx/conf.d/other.conf:\n"
        "server {\n    server_name school.cbnac.com;\n}\n"
        "# configuration file /etc/nginx/conf.d/aether.conf:\n"
        "server {\n    server_name aether.cbnac.com www.aether.cbnac.com;\n}\n"
    )
    check("多域名场景不会认到别的站点",
          parse_vhost_file(multi, "aether.cbnac.com") == "/etc/nginx/conf.d/aether.conf")

    # ④ 子串不能误判（school.cbnac.com 里含 cbnac.com）
    check("域名按整词匹配（不会把 school.cbnac.com 当成 aether.cbnac.com）",
          parse_vhost_file(multi, "aether.cbnac.com") != "/etc/nginx/conf.d/other.conf")

    # ⑤ 找不到就返回 None（调用方据此回退并告警，而不是瞎猜）
    check("找不到时返回 None", parse_vhost_file(common, "nope.example.com") is None)

    # ⑥ 没有 `-T` 注释头（老版本 nginx）也要安全返回 None
    check("没有来源注释时返回 None",
          parse_vhost_file("server { server_name aether.cbnac.com; }\n", "aether.cbnac.com") is None)

    # ⑦ 从 master 命令行取"正在用的配置文件"——这是上面整套判断的前提
    check("能取出运行中的二进制与配置",
          running_nginx("nginx: master process /www/server/nginx/sbin/nginx "
                        "-c /www/server/nginx/conf/nginx.conf")
          == ("/www/server/nginx/sbin/nginx", "/www/server/nginx/conf/nginx.conf"))
    check("没有 -c 时配置为 None（回退默认），二进制仍取得到",
          running_nginx("nginx: master process /usr/sbin/nginx") == ("/usr/sbin/nginx", None))
    check("无关键词（不是 nginx 行）时返回 (None, None)",
          running_nginx("sshd: root@pts/0") == (None, None))

    if failed:
        print(f"[FAIL] 自检不通过：{failed} 项")
        return 1
    print("[ OK ] 自检通过（9 项：生效 vhost 识别 + 运行中进程识别）")
    return 0


def probe_layout(c) -> dict:
    """识别包管理器与 nginx 配置布局 —— 决定后面所有路径。"""
    layout = {}
    layout["pkg"] = "dnf" if sh(c, "command -v dnf >/dev/null && echo dnf || echo apt") == "dnf" else "apt"
    # **先问运行中的 nginx 自己**：它读的是哪个配置文件（master 命令行里的 `-c`），
    # 以及那个配置里哪个文件定义了本站的 server 块。
    # 不这么做的话，`nginx -T`/`nginx -t` 测的是编译期默认配置 —— 本机上那**不是**
    # 正在服务的那个文件，于是"校验通过 + 写入不生效"会同时发生（见 running_nginx_conf）。
    ps = sh(c, "ps -eo pid,cmd | grep 'nginx: master process' | grep -v grep | head -1", check=False)
    layout["nginx_bin"], layout["nginx_conf"] = running_nginx(ps)
    _nx = layout["nginx_bin"] or "nginx"
    _tcmd = (f"{_nx} -T" + (f" -c {layout['nginx_conf']}" if layout["nginx_conf"] else "")
             + " 2>/dev/null || nginx -T 2>/dev/null || true")
    t = sh(c, _tcmd, check=False)
    found = parse_vhost_file(t, DOMAIN)
    layout["conf_dir"] = os.path.dirname(found) if found else "/etc/nginx/conf.d"
    layout["vhost_file"] = found or f"{layout['conf_dir']}/{DOMAIN}.conf"
    layout["vhost_effective"] = bool(found)
    layout["nginx"] = sh(c, "command -v nginx || echo none")
    layout["certbot"] = sh(c, "command -v certbot || echo none")
    layout["root"] = f"/var/www/{DOMAIN}"
    print(f"  包管理器={layout['pkg']}  配置目录={layout['conf_dir']}  "
          f"nginx={'有' if layout['nginx'] != 'none' else '无'}  "
          f"certbot={'有' if layout['certbot'] != 'none' else '无'}")
    print(f"  运行中的 nginx 配置={layout['nginx_conf'] or '（未带 -c，用编译期默认）'}")
    if found:
        print(f"  生效的 vhost 文件（由 nginx -T 认定）={found}")
        if found != f"{layout['conf_dir']}/{DOMAIN}.conf":
            print("  提示：生效文件不在惯例目录下 —— 后面只改这个文件，不改惯例路径")
    else:
        print(f"  ⚠️ 没能在 `nginx -T` 里找到 {DOMAIN} 的 server 块 → 回退到 "
              f"{layout['vhost_file']}；**如果这台 nginx 不 include 该目录，配置不会生效**")
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
    sudo(c, f"chown -R nginx:nginx {layout['root']} 2>/dev/null || chown -R www-data:www-data {layout['root']}; "
             f"find {layout['root']} -type d -exec chmod 755 {{}} +; "
             f"find {layout['root']} -type f -exec chmod 644 {{}} +")


def nginx_test(c, layout: dict) -> str:
    """语法检查 —— **必须测运行中的那份配置**（`-c`），否则可能测了个寂寞。

    本机实测：`nginx -t` 默认测 `/etc/nginx/nginx.conf`，而正在服务的进程用的是
    `/www/server/nginx/conf/nginx.conf`。不传 `-c` 时"语法 OK"不能证明改动是安全的。
    """
    conf = layout.get("nginx_conf")
    nx = layout.get("nginx_bin") or "nginx"
    cmd = (f"{nx} -t" + (f" -c {conf}" if conf else "")) + " 2>&1"
    return sudo(c, cmd)


def write_vhost(c, layout: dict, with_iso: bool) -> None:
    """新增一个 vhost —— 只写 HTTP 段，TLS 交给 certbot（它自己会插入 443 段）。

    安全头走**片段 include**，理由见 `deploy/security-headers.conf`：
    nginx 的 `add_header` 不跨层级继承，某个 location 里写了 Cache-Control 就会
    把 server 级的安全头全部丢掉（含所有 HTML 页面）—— 配置看起来已加固、
    线上却没有（2026-10-02 审计 M-10）。
    """
    print("== 5) 新增 vhost（只加这一个文件，不动其它配置）==")

    # 安全头片段是 vhost 的依赖：必须**先**落盘，否则 include 指向不存在的文件，
    # nginx -t 会直接失败（宁可在这里失败，也不要留一份坏配置在服务器上）。
    snippet_dir = "/etc/nginx/snippets"
    snippet = f"{snippet_dir}/aether-security-headers.conf"
    local_snippet = ROOT / "deploy" / "security-headers.conf"
    if not local_snippet.is_file():
        sys.exit(f"缺少 {local_snippet} —— 安全头片段是 vhost 的依赖，不能跳过")
    sudo(c, f"mkdir -p {snippet_dir}")
    sftp = c.open_sftp()
    sftp.put(str(local_snippet), snippet)
    sftp.close()
    sudo(c, f"chmod 644 {snippet}")
    print(f"  写入 {snippet}")

    conf = layout.get("vhost_file") or f"{layout['conf_dir']}/{DOMAIN}.conf"
    inc = f"include {snippet};"
    dl = ""
    if with_iso:
        dl = f"""
    # 国内下载：ISO 放在本机（会产出流量费用，--with-iso 才开）
    location /dl/ {{
        alias {layout['root']}/dl/;
        add_header Cache-Control "public, max-age=86400";
        {inc}
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
    # 安全头：片段必须在**每个带 add_header 的层级**各 include 一次（nginx 继承规则）
    {inc}

    gzip on;
    gzip_vary on;
    gzip_min_length 512;
    gzip_types text/plain text/css application/javascript application/json image/svg+xml application/xml;

    location /assets/img/ {{ expires 30d; add_header Cache-Control "public, max-age=2592000"; {inc} access_log off; }}
    location ~* \\.(css|svg|woff2?)$ {{ expires 7d; add_header Cache-Control "public, max-age=604800"; {inc} }}
    location ~* \\.html$ {{ expires 10m; add_header Cache-Control "public, max-age=600, must-revalidate"; {inc} }}

    location = /sitemap.xml {{ access_log off; }}
    location = /robots.txt  {{ access_log off; }}

    error_page 404 /404.html;

    # 仓库文件与隐藏文件不该被访问
    location ~ /\\.(?!well-known) {{ deny all; }}
    location ~* \\.(md|rs|toml|py|sh|json)$ {{ deny all; }}
}}
"""
    sftp = c.open_sftp()
    # **绝不覆盖 certbot 管理的既有 vhost**（2026-10-15 真实事故）。那种文件里已经有
    # certbot 插进去的 `listen 443 ssl` 段（连同证书路径），照老逻辑整个覆盖 = 把 HTTPS 弄没。
    # 这种机器上"内容更新"其实只靠第 4 步上传文件 —— vhost 早就配好了，只需**校验**它是对的。
    existing = sh(c, f"cat {conf} 2>/dev/null || echo __NONE__", check=False)
    if "listen 443" in existing:
        print(f"  {conf} 已存在且带 TLS 段（certbot 管理）→ **不覆盖**，只校验与本站一致")
        problems = []
        if f"root {layout['root']};" not in existing:
            problems.append(f"root 不是 {layout['root']}")
        if f"{inc}" not in existing:
            problems.append(f"没有 {inc}")
        if problems:
            sys.exit("  既有 vhost 与本站不一致：" + "；".join(problems) +
                     "\n  → 脚本不猜：请人工核对后修改该文件，或先移除它再重跑")
        print("  root 与安全头片段都对得上 ✓（页面内容已由第 4 步上传）")
        print("  nginx -t:", nginx_test(c, layout))
        return
    if existing != "__NONE__":
        sudo(c, f"cp -a {conf} {conf}.bak-$(date +%Y%m%d-%H%M%S)")
        print("  已备份既有同名配置")
    with sftp.file(conf, "w") as f:
        f.write(body)
    sftp.close()
    print(f"  写入 {conf}")
    print("  nginx -t:", nginx_test(c, layout))


def reload_and_regress(c, base: dict) -> None:
    print("== 6) reload + 生产回归验证 ==")
    # **三种重载方式依次判断，不能只试前两种**（2026-10-15 真实事故）：
    # 这台生产机的 nginx 是"宝塔"式 —— 由 /www/server/nginx/sbin/nginx **手工启动**
    # （不在 systemd 下，systemd 说 "not active"），而 `/run/nginx.pid` 是 **0 字节**
    # （2026-10-03 留下的空文件），于是 `nginx -s reload` 报
    # `invalid PID number "" in "/run/nginx.pid"` —— 两种老办法同时失效。
    # 唯一可靠的第三条路：直接给 **master 进程**发 HUP（nginx 官方的优雅重载信号，零停机）。
    active = sh(c, "systemctl is-active nginx 2>/dev/null || true", check=False).strip()
    pid_ok = sh(c, "test -s /run/nginx.pid && echo ok || echo no", check=False).strip()
    if active == "active":
        sudo(c, "systemctl reload nginx")
        print("  systemd 重载完成")
    elif pid_ok == "ok":
        sudo(c, "nginx -s reload")
        print("  pid 文件重载完成")
    else:
        master = sh(c, "ps -eo pid,cmd | awk '/nginx: master process/ && !/awk/ {print $1; exit}'",
                    check=False).strip()
        if not master.isdigit():
            sys.exit("  systemd 未托管、pid 文件不可用、又找不到 nginx master 进程 —— "
                     "拒绝猜测（生产机上盲重启会与既有实例抢端口），请人工确认后重跑")
        before = sh(c, "ps -eo pid,cmd | grep -E 'nginx: worker' | grep -v grep", check=False)
        sudo(c, f"kill -HUP {master}")
        print(f"  已给 master {master} 发 HUP（systemd 未托管、pid 文件为空的机器走这条）")
        after = sh(c, "ps -eo pid,cmd | grep -E 'nginx: worker' | grep -v grep", check=False)
        if after == before:
            sys.exit("  发了 HUP 但 worker 进程没变 —— 重载可能没生效，请立刻人工检查 "
                     "(nginx -t / error.log)")
        print("  worker 进程已换代 ✓（重载真的生效了，不是「看起来生效」）")
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
        sudo(c, f"{layout['pkg']} install -y python3-certbot-nginx || "
              f"{layout['pkg']} install -y certbot-nginx || echo '插件安装失败，稍后手工处理'", check=False)
    out = sudo(c, f"certbot --nginx -d {DOMAIN} --non-interactive --agree-tos "
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


def verify_security_headers(c) -> None:
    """验证安全头**真的下发**了 —— 而不是只写在配置里。

    2026-10-02 审计 M-10 的教训：`add_header` 的继承规则让"配置里写了"与
    "响应里有"变成两件事，而这个差异在配置文件里完全看不出来。所以部署脚本
    自己必须回头看一眼真实响应。

    **不达标必须让部署失败**（代码审查发现）：此前这里只 `print("⚠️ …")`，
    `main()` 照常返回 0 —— 等于把"自检"降级成"提示"，线上真的丢了头也照样报
    "部署成功"。这与本项目最忌讳的"看起来做了 ≠ 真的做了"是同一个毛病，
    也与同文件 `reload_and_regress()` 不达标即 `sys.exit` 的做法不一致。
    """
    print("== 8a) 安全头实际下发校验 ==")
    # HTML 是最关键的一类（CSP 的主要保护对象），也正是此前被丢掉的层级
    probe = sh(
        c,
        f"curl -sI -m 15 https://{DOMAIN}/download.html | tr -d '\\r' | grep -ci "
        f"'\\(content-security-policy\\|x-content-type-options\\|strict-transport-security\\)' "
        f"|| echo 0",
        check=False,
    ).strip()
    try:
        found = int(probe)
    except ValueError:
        found = 0
    if found >= 3:
        print(f"  HTML 响应含 {found}/3 项关键安全头 ✓")
        return
    print(f"  ✗ HTML 响应只含 {found}/3 项关键安全头 —— 部署判定失败")
    print("     修复要点：安全头片段必须在带 add_header 的每个层级各 include 一次（M-10）")
    print("     （尤其检查 certbot 生成的 443 段是否也被补上了 include）")
    sys.exit(1)


def main() -> int:
    ap = argparse.ArgumentParser(description="部署 AetherOS 官网到 VPS")
    ap.add_argument("--tls", action="store_true", help="用 certbot 签发 HTTPS 证书")
    ap.add_argument("--check-only", action="store_true", help="只做线上校验")
    ap.add_argument("--with-iso", action="store_true", help="把 ISO 也传到本机 /dl/（产生流量费用）")
    ap.add_argument("--skip-build", action="store_true", help="跳过本地构建（用已有 site/）")
    ap.add_argument("--selftest", action="store_true",
                    help="只验纯解析逻辑（生效 vhost 识别），不需要服务器与凭据")
    args = ap.parse_args()

    if args.selftest:
        return selftest()

    host = env("AETHER_VPS_HOST")
    # 默认**不再假设 root**（审计 H-5）：请给一个有 sudo 的普通账号，脚本里用 sudo 提权
    user = env("AETHER_VPS_USER", "aether-deploy")
    if not args.skip_build:
        build()
    c = connect(host, user)
    try:
        layout = probe_layout(c)
        if args.check_only:
            verify(c)
            verify_security_headers(c)
            return 0
        base = baseline(c)
        upload(c, layout, args.with_iso)
        write_vhost(c, layout, args.with_iso)
        reload_and_regress(c, base)
        if args.tls:
            tls(c, layout)
        verify(c)
        verify_security_headers(c)
    finally:
        c.close()
    print("\n完成。")
    print(f"  · 站点：https://{DOMAIN}/（TLS 若未签，先 --tls）")
    print("  · 建议：部署后轮换 VPS 密码（它曾在聊天记录里出现过）")
    print("  · 注意：aether.cbnac.com 是泛解析，镜像子域也会落到这台机器 —— 需要时再加 vhost")
    return 0


if __name__ == "__main__":
    sys.exit(main())
