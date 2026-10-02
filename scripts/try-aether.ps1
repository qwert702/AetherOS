# 一条命令把 AetherOS 跑起来（Windows）：没有 ISO 就下载 Release 里的，然后用 QEMU 引导。
#
#   pwsh -File scripts\try-aether.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\try-aether.ps1
#
# 想调内存/版本：pwsh -File scripts\try-aether.ps1 -Memory 2048 -Version v0.1.0
param(
    [int]$Memory = 1024,
    [string]$Version = "v0.1.0",
    [string]$IsoName = "aetheros-0.1-amd64.iso",
    # 已发布 ISO 的 SHA256（与 site/release.json 同源）。
    # ⚠️ 这是 **Release 资产**（40,327,168 字节）的哈希，与仓库根目录那份本地构建
    # 产物不同；2026-10-02 实际下载复核过，与 GitHub API 的 digest 一致。
    # 传空字符串（-Sha256 ""）表示知情地跳过校验；自建镜像填自己的哈希。
    [string]$Sha256 = "7c50f4814785d83b3defd0e2e7a35524f182c775e0be5a9fa2de893bd7fecdcb"
)

$ErrorActionPreference = "Stop"
$url = "https://github.com/qwert702/AetherOS/releases/download/$Version/$IsoName"

# 完整性校验：这个脚本会把下载到的东西**当作完整操作系统在 QEMU 里引导**，
# 而哈希本来就公开在官网下载页上（2026-10-02 审计 M-11）。
function Assert-IsoHash {
    if ([string]::IsNullOrWhiteSpace($Sha256)) {
        Write-Host "[try-aether] 注意：-Sha256 为空 —— 跳过完整性校验（知情选择）"
        return
    }
    $actual = (Get-FileHash -Path $IsoName -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $Sha256.ToLower()) {
        Write-Host "[try-aether] ✗ ISO 完整性校验失败 —— 已拒绝引导它" -ForegroundColor Red
        Write-Host "   文件    : $IsoName"
        Write-Host "   期望    : $($Sha256.ToLower())"
        Write-Host "   实得    : $actual"
        Write-Host "   自建镜像: -Sha256 <你的哈希>"
        Write-Host "   确认无误要跳过: -Sha256 `"`""
        exit 1
    }
    Write-Host "[try-aether] ✓ ISO SHA256 校验通过（$actual）"
}

$qemuCmd = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
if (-not $qemuCmd) {
    Write-Host @"
[try-aether] 没找到 qemu-system-x86_64。装一个：

  winget install SoftwareFreedomConservancy.QEMU
  （或从 https://www.qemu.org/download/#windows 下载安装包）

不想装 QEMU 也可以：直接下载 ISO 用 VirtualBox / VMware 挂载开机。
ISO 直链：https://github.com/qwert702/AetherOS/releases/download/v0.1.0/aetheros-0.1-amd64.iso
"@ -ForegroundColor Yellow
    exit 1
}

if (-not (Test-Path $IsoName)) {
    Write-Host "[try-aether] 下载 ISO（约 38.5 MB）→ $IsoName"
    $old = $ProgressPreference
    $ProgressPreference = "Continue"   # 38 MB 不给进度条会像卡住
    try {
        Invoke-WebRequest -Uri $url -OutFile $IsoName
    } finally {
        $ProgressPreference = $old
    }
    # 刚下载的校验不过 → 删掉，否则下次会拿这份坏文件去引导
    try {
        Assert-IsoHash
    } catch {
        Remove-Item -Force $IsoName -ErrorAction SilentlyContinue
        Write-Host "[try-aether] 已删除校验失败的下载文件" -ForegroundColor Red
        throw
    }
} else {
    # 已存在的文件同样要校验：可能是上次被截断的下载，也可能被人替换过
    Assert-IsoHash
}

Write-Host "[try-aether] 引导 $IsoName（内存 $Memory MB）。在窗口里操作，关掉窗口就退出。"
Write-Host "[try-aether] 桌面提示：Alt+1–4 切换布局，终端里可以直接敲命令。"
& $qemuCmd.Source -cdrom $IsoName -m $Memory -boot d -vga std
