<#
.SYNOPSIS
  下载 Microsoft Edge WebView2 Evergreen **Standalone** 安装器（x64）到
  packaging/windows/vendor/，供离线安装包内嵌。

.DESCRIPTION
  只给离线包（packaging/nsis-hooks-offline.nsh → RIMESYNC_EMBED_WEBVIEW）用。
  在线包（nsis-hooks.nsh）不会用到本文件，也不会在安装时联网下载任何东西。

  与 fetch-weasel.ps1 的差别：WebView2 Standalone 是"永远最新"的滚动链接
  （linkid=2124701），上游不提供固定版本，所以这里**不校验 SHA-256**，
  改为校验：可执行文件头（MZ）+ 体积下限（默认 100 MiB，防抓回 HTML 错误页/
  只拿到 1.5 MB 的在线引导器）。

.PARAMETER Force
  即使本地文件已存在且通过校验，也重新下载。

.PARAMETER MinBytes
  体积下限，默认 100 MiB。

.PARAMETER OutputDir
  输出目录，默认 <脚本目录>/vendor 。

.PARAMETER ExpectedSha256
  可选：显式锁定某个构建的 SHA-256（内网留档复现时用）。

.EXAMPLE
  pwsh -File packaging/windows/fetch-webview2.ps1

.EXAMPLE
  pwsh -File packaging/windows/fetch-webview2.ps1 -ExpectedSha256 <hash>
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [long]$MinBytes = 104857600,
    [string]$OutputDir = '',
    [string]$ExpectedSha256 = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 滚动链接：x64 Standalone（约 130 MB）。不要换成 linkid=2124703（那是在线引导器 ~1.5MB）。
$WebView2Url  = 'https://go.microsoft.com/fwlink/?linkid=2124701'
$WebView2Name = 'MicrosoftEdgeWebView2RuntimeInstallerX64.exe'

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

function Test-WebView2Payload {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $false }
    $size = (Get-Item -LiteralPath $Path).Length
    if ($size -lt $MinBytes) { return $false }
    if (-not (Test-PortableExecutable -Path $Path)) { return $false }
    if (-not [string]::IsNullOrEmpty($ExpectedSha256)) {
        if ((Get-FileSha256 -Path $Path) -ne $ExpectedSha256.ToLowerInvariant()) { return $false }
    }
    return $true
}

if ([string]::IsNullOrEmpty($OutputDir)) {
    $scriptDir = $PSScriptRoot
    if ([string]::IsNullOrEmpty($scriptDir)) {
        $scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
    }
    $OutputDir = Join-Path $scriptDir 'vendor'
}

$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$target = Join-Path $OutputDir $WebView2Name

if ((Test-Path -LiteralPath $target) -and -not $Force) {
    if (Test-WebView2Payload -Path $target) {
        Write-Host "[webview2] 已存在且校验通过，跳过下载：$target"
        Write-Host "[webview2] 大小 $((Get-Item -LiteralPath $target).Length) 字节，SHA-256 $(Get-FileSha256 -Path $target)"
        exit 0
    }
    Write-Host "[webview2] 现有文件不足 $([math]::Round($MinBytes / 1MB)) MiB 或不是可执行文件，重新下载"
}

$temp = "$target.download"
$attempts = 3
for ($i = 1; $i -le $attempts; $i++) {
    try {
        Write-Host "[webview2] 下载 ($i/$attempts)：$WebView2Url"
        if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force }
        # 滚动链接会 302 到 bbbwexchange/msedge 的 CDN：-MaximumRedirection 交给默认 5 次
        Invoke-WebRequest -Uri $WebView2Url -OutFile $temp -UseBasicParsing -TimeoutSec 1800
        break
    } catch {
        if ($i -eq $attempts) {
            if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue }
            throw "[webview2] 下载失败：$($_.Exception.Message)"
        }
        Write-Warning "[webview2] 第 $i 次下载失败，稍后重试：$($_.Exception.Message)"
        Start-Sleep -Seconds (10 * $i)
    }
}

if (-not (Test-Path -LiteralPath $temp)) {
    throw "[webview2] 下载未产生文件：$temp"
}

$size = (Get-Item -LiteralPath $temp).Length
if ($size -lt $MinBytes) {
    $mib = [math]::Round($size / 1MB, 1)
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[webview2] 体积只有 $mib MiB（< $([math]::Round($MinBytes / 1MB)) MiB）：很可能拿到的是在线引导器或错误页，不是 Standalone 安装器"
}

if (-not (Test-PortableExecutable -Path $temp)) {
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[webview2] 文件头不是 MZ（不是 Windows 可执行文件）"
}

$hash = Get-FileSha256 -Path $temp
if (-not [string]::IsNullOrEmpty($ExpectedSha256) -and $hash -ne $ExpectedSha256.ToLowerInvariant()) {
    Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
    throw "[webview2] SHA-256 不匹配：期望 $ExpectedSha256，实际 $hash"
}

Move-Item -LiteralPath $temp -Destination $target -Force
Write-Host "[webview2] OK：$target"
Write-Host "[webview2]     大小 $size 字节（$([math]::Round($size / 1MB, 1)) MiB），SHA-256 $hash"
