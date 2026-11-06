<#
.SYNOPSIS
  构建 rime-sync Windows 便携版 zip（解压即用，不写注册表）。

.DESCRIPTION
  从 Tauri 的 release 产物目录收集 exe 与 resources，附一份使用说明后压成 zip。
  不负责编译；请先执行 pnpm tauri build --bundles nsis（或 --no-bundle 只出 exe）。

  0.6.22：便携包必须带 resources/prebuilt（否则在「部署不产出 build」的机器上
  输入法打不出字），并尽量带 resources/weasel/weasel-*.exe（fix-ime 装回输入法
  的载荷）。缺 prebuilt 直接失败；缺 weasel 安装器只告警，并在 README 里提示
  手动安装小狼毫——fix-ime 检测到便携版也会给出同样的提示。

.EXAMPLE
  pwsh -File packaging/portable-zip.ps1
  pwsh -File packaging/portable-zip.ps1 -TargetDir src-tauri/target/release -OutDir packaging/out
#>
[CmdletBinding()]
param(
  [string]$TargetDir = "src-tauri/target/release",
  [string]$OutDir = "packaging/out",
  [string]$Version = ""
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not [System.IO.Path]::IsPathRooted($TargetDir)) {
  $TargetDir = Join-Path $repoRoot $TargetDir
}
if (-not [System.IO.Path]::IsPathRooted($OutDir)) {
  $OutDir = Join-Path $repoRoot $OutDir
}

if ([string]::IsNullOrWhiteSpace($Version)) {
  $confPath = Join-Path $repoRoot "src-tauri/tauri.conf.json"
  if (-not (Test-Path $confPath)) { throw "找不到 $confPath，无法确定版本号" }
  $Version = (Get-Content -Raw -Encoding UTF8 $confPath | ConvertFrom-Json).version
}
if ([string]::IsNullOrWhiteSpace($Version)) { throw "版本号为空" }

$exe = Join-Path $TargetDir "rime-sync.exe"
if (-not (Test-Path $exe)) {
  # Cargo 包名是 rime-sync-app，产物文件名随包名走；两种名字都接受。
  $exe = Join-Path $TargetDir "rime-sync-app.exe"
}
if (-not (Test-Path $exe)) {
  $candidate = Get-ChildItem -Path $TargetDir -Filter *.exe -File |
    Where-Object { $_.Name -notmatch 'setup|uninstall' } |
    Select-Object -First 1
  if ($candidate) { $exe = $candidate.FullName }
}
if (-not (Test-Path $exe)) {
  throw "找不到可执行文件（$TargetDir 下无 rime-sync.exe / rime-sync-app.exe）；请先运行 pnpm tauri build --bundles nsis 或 cargo build --release"
}

$stage = Join-Path $OutDir ("rime-sync_" + $Version + "_x64-portable")
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item $exe (Join-Path $stage "rime-sync.exe") -Force

# 0.6.17：CLI 由 bundle.externalBin 打进安装包，tauri-build 会同时把它复制到
# <TargetDir>\rime-sync-cli.exe。便携版保持同一布局：解压后可直接跑
# .\rime-sync-cli.exe fix-ime 排查输入法产物。缺了就说明 sidecar 构建没做，
# 便携包会与安装包不一致 —— 直接失败，不要静默发一个残缺包。
$cli = Join-Path $TargetDir "rime-sync-cli.exe"
if (-not (Test-Path -LiteralPath $cli)) {
  throw "缺少 $cli：先 cargo build -p rime-sync-cli --release 并按 target triple 命名（见 packaging/windows/README.md）"
}
Copy-Item -LiteralPath $cli -Destination (Join-Path $stage "rime-sync-cli.exe") -Force

# 0.6.22：resources 必须随包。安装版由 NSIS 铺 $INSTDIR\resources；便携版没有
# 安装器，只能自己带，否则 prebuilt 兜底与 fix-ime 装回输入法全部落空。
$resources = Join-Path $TargetDir "resources"
if (-not (Test-Path -LiteralPath $resources)) {
  throw "缺少 $resources：先跑 pnpm tauri build（tauri.conf.json 的 bundle.resources 会复制到这里）"
}
$stagedResources = Join-Path $stage "resources"
Copy-Item -LiteralPath $resources -Destination $stagedResources -Recurse -Force

# prebuilt 是「部署不产出 build」机器的唯一兜底（0.6.16），便携版不许缺。
$prebuilt = Join-Path $stagedResources "prebuilt"
if (-not (Test-Path -LiteralPath $prebuilt)) {
  throw "便携包必须含 resources/prebuilt（生成：scripts/ime/gen-prebuilt-build.py），否则等于没有输入法产物"
}
$prebuiltFiles = @(Get-ChildItem -Path $prebuilt -Recurse -File)
if ($prebuiltFiles.Count -eq 0) {
  throw "resources/prebuilt 是空目录：先跑 scripts/ime/gen-prebuilt-build.py"
}

# fix-ime 用 resources/weasel/weasel-*.exe 装回小狼毫；构建时没 fetch-weasel
# 就没有这个文件——不阻塞打包，但要让用户看得见（README + fix-ime 提示）。
$weaselInstallers = @()
$weaselDir = Join-Path $stagedResources "weasel"
if (Test-Path -LiteralPath $weaselDir) {
  $weaselInstallers = @(Get-ChildItem -Path $weaselDir -Filter "weasel-*.exe" -File)
}
if ($weaselInstallers.Count -eq 0) {
  Write-Warning "便携包内没有 resources/weasel/weasel-*.exe：fix-ime 无法自动装回输入法，用户需手动安装小狼毫（README 已提示）"
}

# WebView2 运行时是系统组件：便携包不内嵌，缺失时提示用户安装。
$readme = @"
rime-sync $Version · Windows 便携版
=====================================

1. 把整个目录解压到任意位置（例如 D:\Tools\rime-sync）。
2. 双击 rime-sync.exe 运行；不需要管理员权限，不写注册表。
3. 若提示缺少 WebView2 Runtime：安装微软的 "Evergreen Bootstrapper"
   （https://developer.microsoft.com/microsoft-edge/webview2/），Win11 一般已自带。
4. 卸载 = 删除本目录；配置与备份在 %APPDATA%\rime-sync，需要彻底清理时一并删除。
5. 输入法产物异常时，在本目录运行：rime-sync-cli.exe fix-ime
   （排查日志：%APPDATA%\com.rimesync.app\bootstrap.log）。
6. 便携包已带 resources/prebuilt（部署不产出 build 的机器拿它兜底）。
   如果本目录没有 resources/weasel/weasel-*.exe（构建时未打包小狼毫安装器），
   fix-ime 无法自动装回输入法，请手动安装小狼毫（Weasel）后再重试，
   或改用 NSIS 安装版。

注意：便携版与 NSIS 安装版共用同一份配置目录，请不要同时运行两个实例。
"@
Set-Content -Path (Join-Path $stage "README-便携版.txt") -Value $readme -Encoding UTF8

$zip = Join-Path $OutDir ("rime-sync_" + $Version + "_x64-portable.zip")
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -CompressionLevel Optimal

Write-Host "portable zip 已生成：$zip"
Write-Host ("resources: prebuilt={0} files, weasel-installer={1} 个" -f $prebuiltFiles.Count, $weaselInstallers.Count)
Get-ChildItem $zip | Format-Table Name, Length
