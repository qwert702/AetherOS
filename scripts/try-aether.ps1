# 一条命令把 AetherOS 跑起来（Windows）：没有 ISO 就下载 Release 里的，然后用 QEMU 引导。
#
#   pwsh -File scripts\try-aether.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\try-aether.ps1
#
# 想调内存/版本：pwsh -File scripts\try-aether.ps1 -Memory 2048 -Version v0.1.0
param(
    [int]$Memory = 1024,
    [string]$Version = "v0.1.0",
    [string]$IsoName = "aetheros-0.1-amd64.iso"
)

$ErrorActionPreference = "Stop"
$url = "https://github.com/qwert702/AetherOS/releases/download/$Version/$IsoName"

$qemuCmd = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
if (-not $qemuCmd) {
    Write-Host @"
[try-aether] 没找到 qemu-system-x86_64。装一个：

  winget install SoftwareFreedomConservancy.QEMU
  （或从 https://www.qemu.org/download/#windows 下载安装包）

不想装 QEMU 也可以：直接下载 ISO 用 VirtualBox / VMware 挂载开机。
ISO 直链：https://github.com/qwert702/AetherOS/releases/latest
"@ -ForegroundColor Yellow
    exit 1
}

if (-not (Test-Path $IsoName)) {
    Write-Host "[try-aether] 下载 ISO（约 33 MB）→ $IsoName"
    $old = $ProgressPreference
    $ProgressPreference = "Continue"   # 33 MB 不给进度条会像卡住
    try {
        Invoke-WebRequest -Uri $url -OutFile $IsoName
    } finally {
        $ProgressPreference = $old
    }
}

Write-Host "[try-aether] 引导 $IsoName（内存 $Memory MB）。在窗口里操作，关掉窗口就退出。"
Write-Host "[try-aether] 桌面提示：Alt+1–4 切换布局，终端里可以直接敲命令。"
& $qemuCmd.Source -cdrom $IsoName -m $Memory -boot d -vga std
