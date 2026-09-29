# scripts/setup-vm.ps1 — AetherOS 开发虚拟机自动化（Windows 宿主）
#
# ⚠️ 本文件必须以「UTF-8 with BOM」保存（仓库里唯一的 .ps1）：
#    Windows PowerShell 5.1 对**无 BOM** 的 .ps1 按 ANSI 解码，中文字符串会被拆坏，
#    报出"缺少右 }"之类的假语法错误（2026-09-29 实测：无 BOM 时 1 处、加 BOM 后 0 处）。
#    改动本文件后请确认 BOM 还在：首三字节应为 EF BB BF。
#
# 用法（管理员 PowerShell）:
#   powershell -ExecutionPolicy Bypass -File scripts/setup-vm.ps1
#
# 凭据（脚本内不硬编码密码）:
#   $env:AETHER_VM_PASSWORD = "<构建机登录密码>"   # 可选；不设则在运行时安全提示输入
#   $env:AETHER_VM_USER     = "aether"            # 可选，默认 aether
#
# 步骤:
#   1. winget 静默安装 VirtualBox（已装则跳过）
#   2. 下载 Ubuntu 24.04 Server ISO（已存在则跳过）
#   3. VBoxManage 创建 VM（4GB 内存 / 2 CPU / 40GB 磁盘）
#   4. VBoxManage unattended install 无人值守装系统（无需人工点安装器）
#   5. 启动 VM；随后在 VM 内运行 platform/build-iso.sh
#
# 说明：这是**本机一次性测试 VM**，密码只用于无人值守安装与后续 SSH
#       （SSH 侧读 scripts/vm.py 的 AETHER_VM_PASSWORD，与这里同一个值）。

param(
    [string]$VmUser = $(if ($env:AETHER_VM_USER) { $env:AETHER_VM_USER } else { "aether" }),
    [string]$VmPassword = $env:AETHER_VM_PASSWORD
)

$ErrorActionPreference = "Stop"
$VMName = "AetherOS-Build"
$WorkDir = "$env:USERPROFILE\aether-vm"
$IsoPath = "$WorkDir\ubuntu-24.04-live-server-amd64.iso"

function Info($m) { Write-Host "==> $m" }

function Get-VmPassword([string]$Provided) {
    # 优先环境变量/参数；交互式终端下退化为安全提示输入（不回显、不进历史）
    if ($Provided) { return $Provided }
    if ([Console]::IsInputRedirected) {
        throw "缺少密码：请先设置 `$env:AETHER_VM_PASSWORD 再重跑（脚本不再硬编码密码）。"
    }
    $sec = Read-Host "构建机登录密码（仅本机测试 VM，输入不回显）" -AsSecureString
    $plain = (New-Object System.Management.Automation.PSCredential("vm", $sec)).GetNetworkCredential().Password
    if (-not $plain) { throw "密码为空，已中止。" }
    return $plain
}

if (-not $VmUser) { throw "构建机用户名不能为空（AETHER_VM_USER）" }
$VMPassword = Get-VmPassword $VmPassword

# 1. VirtualBox
Info "检查 VirtualBox"
$VBox = "$env:ProgramFiles\Oracle\VirtualBox\VBoxManage.exe"
if (-not (Test-Path $VBox)) {
    Info "winget 安装 VirtualBox（静默）"
    winget install --id Oracle.VirtualBox -e --accept-source-agreements --accept-package-agreements
} else { Info "已安装" }

# 2. Ubuntu ISO（清华 TUNA 镜像，国内高速）
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
if (-not (Test-Path $IsoPath)) {
    Info "从 TUNA 镜像下载 Ubuntu 24.04.3 Server ISO（约 2.6GB）"
    Invoke-WebRequest -Uri "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04.3/ubuntu-24.04.3-live-server-amd64.iso" -OutFile $IsoPath
} else { Info "ISO 已存在" }

# 3. 创建 VM
if (-not (& $VBox list vms | Select-String $VMName)) {
    Info "创建 VM: $VMName"
    & $VBox createvm --name $VMName --ostype Ubuntu_64 --register
    & $VBox modifyvm $VMName --memory 4096 --cpus 2 --nat-localhostonly1 on
    & $VBox createmedium disk --name "$WorkDir\$VMName-disk" --size 40960 --format VDI
    & $VBox storageattach $VMName --storagectl "IDE" --port 0 --device 0 --type hdd --medium "$WorkDir\$VMName-disk.vdi"
    & $VBox storagectl $VMName --name "SATA" --add sata
    & $VBox storageattach $VMName --storagectl "SATA" --port 1 --device 0 --type dvddrive --medium $IsoPath
}

# 4. 无人值守安装 + 5. 启动
Info "启动无人值守安装（装完自动重启进系统）"
& $VBox unattended install $VMName --iso=$IsoPath --user=$VmUser --password=$VMPassword `
    --full-user-name="Aether Builder" --hostname=aether-build `
    --install-additions --time-zone=Asia/Shanghai --unattended=install
& $VBox startvm $VMName --type headless
Info "完成。VM 安装完毕后: 共享项目文件夹 → VM 内运行 platform/build-iso.sh"
