<#
.SYNOPSIS
  下载固定版本的 Weasel（小狼毫）官方安装包到 packaging/windows/vendor/。

.DESCRIPTION
  rime-sync 的 Windows NSIS 安装包会把该文件打进 $INSTDIR\resources\，
  安装时以 /S 静默执行。CI 必须在 'pnpm tauri build --bundles nsis' 之前运行本脚本。

  文件不入库（见同目录 .gitignore）。脚本校验大小与 SHA-256，不匹配即失败。

.PARAMETER Force
  即使本地文件已存在且校验通过，也重新下载。

.PARAMETER Mirror
  可选镜像前缀（拼在 GitHub URL 前面），例如 https://gh-proxy.com 。

.PARAMETER OutputDir
  输出目录，默认 <脚本目录>/vendor 。

.EXAMPLE
  pwsh -File packaging/windows/fetch-weasel.ps1

.EXAMPLE
  pwsh -File packaging/windows/fetch-weasel.ps1 -Force -Mirror https://gh-proxy.com
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [string]$Mirror = '',
    [string]$OutputDir = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---- 固定版本与指纹（改版本时必须同步 nsis-hooks.nsh 的 RIMESYNC_WEASEL_*）----
$WeaselVersion   = '0.17.4.0'
$WeaselTag       = '0.17.4'
$WeaselInstaller = "weasel-$WeaselVersion-installer.exe"
$WeaselSize      = 12431118
$WeaselSha256    = 'cf509534a8f5f8af9c98ed7cbb8f135439f145a8cbe7e50ede42bb5b5ab45c29'
$WeaselUrl       = "https://github.com/rime/weasel/releases/download/$WeaselTag/$WeaselInstaller"

# Windows PowerShell 5.1 默认可能仍是 TLS 1.0
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch {
    # 老 .NET 上没有 Tls12 枚举值时忽略
}

function Get-FileSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-PortableExecutable {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        if ($stream.Length -lt 2) { return $false }
        $first = $stream.ReadByte()
        $second = $stream.ReadByte()
        return ($first -eq 0x4D -and $second -eq 0x5A)   # 'MZ'
    } finally {
        $stream.Dispose()
    }
}

if ([string]::IsNullOrEmpty($OutputDir)) {
    $scriptDir = $PSScriptRoot
    if ([string]::IsNullOrEmpty($scriptDir)) {
        $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    }
    $OutputDir = Join-Path $scriptDir 'vendor'
}

# 统一成绝对路径：后面用 .NET FileStream 校验文件头
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$target = Join-Path $OutputDir $WeaselInstaller

if ((Test-Path -LiteralPath $target) -and -not $Force) {
    $existingSize = (Get-Item -LiteralPath $target).Length
    if ($existingSize -eq $WeaselSize -and (Get-FileSha256 -Path $target) -eq $WeaselSha256) {
        Write-Host "[weasel] 已存在且校验通过，跳过下载：$target"
        exit 0
    }
    Write-Host "[weasel] 现有文件大小或 SHA-256 不符，重新下载"
}

if ([string]::IsNullOrEmpty($Mirror)) {
    $url = $WeaselUrl
} else {
    $url = $Mirror.TrimEnd('/') + '/' + $WeaselUrl
}

$temp = "$target.download"
$attempts = 3
for ($i = 1; $i -le $attempts; $i++) {
    try {
        Write-Host "[weasel] 下载 ($i/$attempts)：$url"
        if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force }
        Invoke-WebRequest -Uri $url -OutFile $temp -UseBasicParsing -TimeoutSec 300
        break
    } catch {
        if ($i -eq $attempts) {
            if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue }
            throw "[weasel] 下载失败：$($_.Exception.Message)"
        }
        Write-Warning "[weasel] 第 $i 次下载失败，稍后重试：$($_.Exception.Message)"
        Start-Sleep -Seconds (5 * $i)
    }
}

if (-not (Test-Path -LiteralPath $temp)) {
    throw "[weasel] 下载未产生文件：$temp"
}

$size = (Get-Item -LiteralPath $temp).Length
if ($size -ne $WeaselSize) {
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[weasel] 大小不符：期望 $WeaselSize 字节，实际 $size 字节"
}

if (-not (Test-PortableExecutable -Path $temp)) {
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[weasel] 文件头不是 MZ（不是 Windows 可执行文件）"
}

$hash = Get-FileSha256 -Path $temp
if ($hash -ne $WeaselSha256) {
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[weasel] SHA-256 不匹配：期望 $WeaselSha256，实际 $hash"
}

Move-Item -LiteralPath $temp -Destination $target -Force
Write-Host "[weasel] OK：$target"
Write-Host "[weasel]     大小 $size 字节，SHA-256 $hash"
