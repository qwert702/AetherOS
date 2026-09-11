# scripts/setup-vm.ps1 — AetherOS 开发虚拟机自动化（Windows 宿主）
#
# 用法（管理员 PowerShell）:
#   powershell -ExecutionPolicy Bypass -File scripts/setup-vm.ps1
#
# 步骤:
#   1. winget 静默安装 VirtualBox（已装则跳过）
#   2. 下载 Ubuntu 24.04 Server ISO（已存在则跳过）
#   3. VBoxManage 创建 VM（4GB 内存 / 2 CPU / 40GB 磁盘）
#   4. VBoxManage unattended install 无人值守装系统（无需人工点安装器）
#   5. 启动 VM；随后在 VM 内运行 platform/build-iso.sh
#
# 无人值守登录账号: aether / aetheros

$ErrorActionPreference = "Stop"
$VMName = "AetherOS-Build"
$User = "aether"; $Password = "aetheros"
$WorkDir = "$env:USERPROFILE\aether-vm"
$IsoPath = "$WorkDir\ubuntu-24.04-live-server-amd64.iso"

function Info($m) { Write-Host "==> $m" }

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
& $VBox unattended install $VMName --iso=$IsoPath --user=$User --password=$Password `
    --full-user-name="Aether Builder" --hostname=aether-build `
    --install-additions --time-zone=Asia/Shanghai --unattended=install
& $VBox startvm $VMName --type headless
Info "完成。VM 安装完毕后: 共享项目文件夹 → VM 内运行 platform/build-iso.sh"
