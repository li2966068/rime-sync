#Requires -Version 7.0
<#
.SYNOPSIS
  Windows 自建输入法构建缓存（rime.dll / WeaselServer.exe / Boost）的写入与命中校验。

.DESCRIPTION
  release.yml 的 windows job 用 actions/cache 缓存**产物目录**（librime 安装前缀、
  Weasel 输出、boost 产物），命中后跳过 40 分钟的自建。缓存可能过期、残缺、或者干脆是
  上一次坏构建留下的，所以命中之后必须先过这个脚本：

    -Mode verify  比对 build/cache-manifest.json 里记录的输入指纹（librime / weasel 源码树
                  （git 树对象 id）/ 插件源码哈希 / 构建脚本哈希 / 版本），再对产物做二进制断言（rime.dll 里必须有
                  rimesync_vnum 与 numpad 明文、WeaselServer.exe / WeaselDeployer.exe 必须
                  在、安装包大小与哈希必须对得上）。
                  通过 -> 打印 cache=hit，写 GITHUB_OUTPUT valid=true。
                  不通过 -> 打印 cache=invalid reason=...，删掉缓存的产物目录（走全量构建），
                  写 valid=false，退出码仍然是 0（job 继续跑全量构建，不红）。

    -Mode write   全量构建成功之后调用：对同一组产物做断言并写出 manifest。

  设计约束：判 invalid 时**只**删产物目录（git clean -fdx 保留下游 vendored 文件），
  绝不改动 third_party/weasel 里入库的脚手架（output\7z.exe、install.nsi 等）。

.PARAMETER Mode
  verify（缺省）校验缓存；write 写清单。

.PARAMETER RepoRoot
  仓库根目录，缺省按脚本位置推（scripts\ime\ -> 仓库根）。

.PARAMETER ManifestPath
  清单路径，缺省 build\cache-manifest.json（该文件本身也在 actions/cache 的 path 里）。

.PARAMETER SkipWin32
  与 build-librime-win.ps1 / build-weasel.ps1 的 -SkipWin32 对齐；CI 不传。

.PARAMETER KeepInvalid
  本地排查用：判定 invalid 时不删目录。

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/verify-win-build-cache.ps1 -Mode verify

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/verify-win-build-cache.ps1 -Mode write
#>
[CmdletBinding()]
param(
    [ValidateSet('verify', 'write')]
    [string]$Mode = 'verify',

    [string]$RepoRoot,

    [string]$ManifestPath,

    [switch]$SkipWin32,

    [switch]$KeepInvalid,

    [string]$GitHubOutput = $env:GITHUB_OUTPUT,

    [string]$GitHubSummary = $env:GITHUB_STEP_SUMMARY
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# 缓存里 rime.dll 必须带的明文常量（rimesync-extra 注册表用的字符串）。
# build-librime-win.ps1 断言的是全量（+ rimesync_punct / rimesync_vselect）；
# 这里按 CI 验收口径只要求 vnum 与 numpad，punct/vselect 由 release.yml 的安装包断言兜。
$ExpectedComponents = @('rimesync_vnum', 'numpad')
$ManifestSchema = 1
$BoostVersion = '1.84.0'
$PlatformToolset = 'v143'
$MinInstallerBytes = 8MB

# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------
function Get-JsonValue {
    param($Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    $prop = $Object.PSObject.Properties[$Name]
    if ($null -eq $prop) { return $null }
    return $prop.Value
}

function Set-ActionOutput {
    param([string]$Name, [string]$Value)
    if ([string]::IsNullOrWhiteSpace($GitHubOutput)) { return }
    Add-Content -LiteralPath $GitHubOutput -Value ('{0}={1}' -f $Name, $Value) -Encoding utf8
}

function Add-Summary {
    param([string[]]$Lines)
    if ([string]::IsNullOrWhiteSpace($GitHubSummary)) { return }
    Add-Content -LiteralPath $GitHubSummary -Value ($Lines -join [Environment]::NewLine) -Encoding utf8
}

function Get-FileSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# 目录的内容指纹：相对路径 + 每个文件的 SHA-256，排序后再取一次 SHA-256。
# 排除 deps/build/dist/.git（它们是构建产物或别处克隆来的，任何一次构建都会变）。
function Get-TreeFingerprint {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string[]]$ExcludeNames = @('deps', 'build', 'dist', '.git')
    )
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $root = (Resolve-Path -LiteralPath $Path).Path
    while ($root.EndsWith('\') -or $root.EndsWith('/')) { $root = $root.Substring(0, $root.Length - 1) }
    $entries = New-Object System.Collections.Generic.List[string]
    foreach ($file in (Get-ChildItem -LiteralPath $root -Recurse -File -Force)) {
        $rel = $file.FullName.Substring($root.Length)
        if ($rel.StartsWith('\') -or $rel.StartsWith('/')) { $rel = $rel.Substring(1) }
        $segs = @($rel -split '[\\/]')
        $excluded = $false
        if ($segs.Length -gt 1) {
            foreach ($seg in $segs[0..($segs.Length - 2)]) {
                if ($ExcludeNames -contains $seg) { $excluded = $true; break }
            }
        }
        if ($excluded) { continue }
        $normalized = $rel -replace '\\', '/'
        $entries.Add($normalized + ' ' + (Get-FileSha256 -Path $file.FullName))
    }
    if ($entries.Count -eq 0) { return '' }
    $text = (($entries | Sort-Object) -join "`n") + "`n"
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
        return (($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $sha.Dispose()
    }
}

# 二进制里搜 ASCII 明文：ISO-8859-1 是字节到字符 1:1，和 build-librime-win.ps1 同款做法。
function Get-MissingBinaryStrings {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [string[]]$Names = $ExpectedComponents
    )
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $text = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $missing = @()
    foreach ($name in $Names) {
        if ($text.IndexOf($name, [System.StringComparison]::Ordinal) -lt 0) { $missing += $name }
    }
    return $missing
}

function Get-LibrimeVersion {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return 'unknown' }
    $raw = Get-Content -LiteralPath $Path -Raw -Encoding UTF8
    $m = [regex]::Match($raw, '(?m)^\s*set\(rime_version\s+([0-9][0-9A-Za-z.\-]*)\s*\)')
    if ($m.Success) { return $m.Groups[1].Value }
    return 'unknown'
}

function Invoke-GitLine {
    param([string[]]$GitArgs)
    try {
        $out = & git -C $script:Repo @GitArgs 2>$null
        if ($LASTEXITCODE -ne 0) { return '' }
        return ([string]($out | Select-Object -First 1)).Trim()
    } catch {
        return ''
    }
}

# ---------------------------------------------------------------------------
# 上下文
# ---------------------------------------------------------------------------
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot) }
$script:Repo = (Resolve-Path -LiteralPath $RepoRoot).Path
if (-not $ManifestPath) { $ManifestPath = Join-Path $script:Repo 'build\cache-manifest.json' }
$manifestFile = [System.IO.Path]::GetFullPath($ManifestPath)

$librimeDir = Join-Path $script:Repo 'third_party\librime'
$weaselDir = Join-Path $script:Repo 'third_party\weasel'
$boostDir = Join-Path $weaselDir 'deps\boost_1_84_0'
$vendorDir = Join-Path $script:Repo 'packaging\windows\vendor'
$librimeScript = Join-Path $script:Repo 'scripts\ime\build-librime-win.ps1'
$weaselScript = Join-Path $script:Repo 'scripts\ime\build-weasel.ps1'
$selfScript = $PSCommandPath

function Resolve-RepoPath {
    param([string]$Rel)
    return (Join-Path $script:Repo ($Rel -replace '/', '\'))
}

# 缓存里必须存在的产物（相对仓库根，统一用 / 分隔）。
function Get-RequiredArtifacts {
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($a in @('third_party/librime/dist/x64/lib/rime.dll',
                     'third_party/librime/dist/x64/lib/rime.lib',
                     'third_party/librime/dist/x64/include/rime_api.h',
                     'third_party/weasel/output/rime.dll',
                     'third_party/weasel/output/WeaselServer.exe',
                     'third_party/weasel/output/WeaselDeployer.exe',
                     'third_party/weasel/lib64/rime.lib',
                     'third_party/weasel/include/rime_api.h')) {
        $list.Add($a)
    }
    if (-not $SkipWin32) {
        foreach ($a in @('third_party/librime/dist/Win32/lib/rime.dll',
                         'third_party/librime/dist/Win32/lib/rime.lib',
                         'third_party/librime/dist/Win32/include/rime_api.h',
                         'third_party/weasel/output/Win32/rime.dll',
                         'third_party/weasel/output/Win32/WeaselServer.exe',
                         'third_party/weasel/output/Win32/WeaselDeployer.exe',
                         'third_party/weasel/lib/rime.lib')) {
            $list.Add($a)
        }
    }
    return $list
}

# 需要在里面搜明文组件名的二进制。
function Get-ComponentBinaries {
    $list = New-Object System.Collections.Generic.List[string]
    $list.Add('third_party/librime/dist/x64/lib/rime.dll')
    $list.Add('third_party/weasel/output/rime.dll')
    if (-not $SkipWin32) {
        $list.Add('third_party/librime/dist/Win32/lib/rime.dll')
        $list.Add('third_party/weasel/output/Win32/rime.dll')
    }
    return $list
}

# Weasel 安装包：产物在 third_party/weasel/output/archives/，Publish-Artifact 会拷一份到
# packaging\windows\vendor\（NSIS 的 !define RIMESYNC_WEASEL_EXE 指向它）。
function Find-WeaselInstaller {
    $archives = Join-Path $weaselDir 'output\archives'
    if (-not (Test-Path -LiteralPath $archives)) { return $null }
    $hits = @(Get-ChildItem -LiteralPath $archives -Filter 'weasel-*-installer.exe' -File -ErrorAction SilentlyContinue)
    if ($hits.Count -ne 1) { return $null }
    return $hits[0]
}

function Get-ExpectedInputs {
    # 源码指纹一律取 git 树对象 id（HEAD:<path>）：构建过程会往 third_party/librime 里写
    # env.bat、往 third_party/weasel 里写 msbuild\ 与 output\，用「目录内容哈希」会在
    # 构建前后得到不同值（缓存就永远不命中）。树对象 id 只看入库内容，与构建产物无关。
    #
    # 这里只放**真正决定产物**的东西：改了它们，重建出来的 rime.dll / Weasel 才不一样。
    # 仓库的 HEAD（repo_commit）和本脚本自身的哈希**不放这里**：它们跟产物无关，
    # 却会让「改一句 CHANGELOG」也把 40 分钟的自建缓存作废 —— 0.6.23→0.6.24 三轮
    # 每次都全量重编 Windows 就是这么来的（cache=invalid reason=inputs-changed:repo_commit）。
    # 这两项改成写进清单当来源信息（Get-ProvenanceInputs）：只记录、不参与比对。
    $inputs = [ordered]@{
        platform              = 'windows'
        librime_version       = (Get-LibrimeVersion -Path (Join-Path $librimeDir 'CMakeLists.txt'))
        librime_tree_sha      = (Invoke-GitLine -GitArgs @('rev-parse', 'HEAD:third_party/librime'))
        weasel_tree_sha       = (Invoke-GitLine -GitArgs @('rev-parse', 'HEAD:third_party/weasel'))
        librime_plugin_sha256 = (Get-TreeFingerprint -Path (Join-Path $librimeDir 'plugins\rimesync-extra'))
        librime_script_sha256 = (Get-FileSha256 -Path $librimeScript)
        weasel_script_sha256  = (Get-FileSha256 -Path $weaselScript)
        boost_version         = $BoostVersion
        platform_toolset      = $PlatformToolset
    }
    return $inputs
}

function Get-ProvenanceInputs {
    # 来源信息：写进清单方便事后排查，但**不参与命中比对**（它们变了也不作废缓存）。
    return [ordered]@{
        repo_commit          = (Invoke-GitLine -GitArgs @('rev-parse', 'HEAD'))
        verify_script_sha256 = (Get-FileSha256 -Path $selfScript)
    }
}

function Format-Header {
    param([string]$Mode)
    Write-Host 'rime-sync · Windows 构建缓存校验'
    Write-Host ("mode={0}  repo={1}" -f $Mode, $script:Repo)
    Write-Host ("manifest={0}" -f $manifestFile)
}

# ---------------------------------------------------------------------------
# 判定：输入指纹
# ---------------------------------------------------------------------------
function Get-InputMismatches {
    param($Manifest, $Expected)
    $bad = New-Object System.Collections.Generic.List[string]
    $recorded = Get-JsonValue $Manifest 'inputs'
    if ($null -eq $recorded) { return @('manifest-no-inputs') }
    foreach ($name in $Expected.Keys) {
        $expectedValue = [string]$Expected[$name]
        $recordedValue = [string](Get-JsonValue $recorded $name)
        if ($expectedValue -eq '') { continue }
        if ($recordedValue -eq '') { $bad.Add($name); continue }
        if ($recordedValue -ne $expectedValue) { $bad.Add($name) }
    }
    return $bad
}

# ---------------------------------------------------------------------------
# 判定：产物（存在性 + 哈希 + 组件名）
# ---------------------------------------------------------------------------
function Test-Artifacts {
    param($Manifest)
    $problems = New-Object System.Collections.Generic.List[string]
    $recorded = Get-JsonValue $Manifest 'artifacts'
    foreach ($rel in (Get-RequiredArtifacts)) {
        $abs = Resolve-RepoPath $rel
        if (-not (Test-Path -LiteralPath $abs)) { $problems.Add('artifact-missing:' + $rel); continue }
        $expectedHash = [string](Get-JsonValue (Get-JsonValue $recorded $rel) 'sha256')
        if ($expectedHash -eq '') { $problems.Add('manifest-artifact-unrecorded:' + $rel); continue }
        if ((Get-FileSha256 -Path $abs) -ne $expectedHash) { $problems.Add('artifact-changed:' + $rel) }
    }
    foreach ($rel in (Get-ComponentBinaries)) {
        $abs = Resolve-RepoPath $rel
        if (-not (Test-Path -LiteralPath $abs)) { continue }
        foreach ($missing in (Get-MissingBinaryStrings -Path $abs)) {
            $problems.Add(('plugin-missing:{0}:{1}' -f $rel, $missing))
        }
    }
    return $problems
}

function Test-Installer {
    param($Manifest)
    $problems = New-Object System.Collections.Generic.List[string]
    $installer = Find-WeaselInstaller
    if ($null -eq $installer) { $problems.Add('installer-missing-in-output'); return $problems }
    $expectedHash = [string](Get-JsonValue (Get-JsonValue $Manifest 'installer') 'sha256')
    if ($expectedHash -eq '') { $problems.Add('manifest-installer-unrecorded'); return $problems }
    $actualHash = Get-FileSha256 -Path $installer.FullName
    if ($actualHash -ne $expectedHash) {
        $problems.Add('installer-changed:' + $installer.Name)
        return $problems
    }
    if ($installer.Length -lt $MinInstallerBytes) {
        $problems.Add(('installer-too-small:{0}' -f $installer.Name))
    }
    $vendorCopy = Join-Path $vendorDir $installer.Name
    if ((-not (Test-Path -LiteralPath $vendorCopy)) -or ((Get-FileSha256 -Path $vendorCopy) -ne $actualHash)) {
        New-Item -ItemType Directory -Force -Path $vendorDir | Out-Null
        Copy-Item -LiteralPath $installer.FullName -Destination $vendorCopy -Force
        Write-Host ("[hydrate] {0} <- {1}" -f $vendorCopy, $installer.FullName)
    }
    return $problems
}

# ---------------------------------------------------------------------------
# 缓存作废：只删产物目录，保留下游 vendored 文件
# ---------------------------------------------------------------------------
function Remove-CacheProducts {
    $targets = @(
        'third_party/librime/dist',
        'third_party/weasel/output',
        'third_party/weasel/lib',
        'third_party/weasel/lib64',
        'third_party/weasel/include',
        'third_party/weasel/deps/boost_1_84_0'
    )
    $existing = @()
    foreach ($rel in $targets) {
        $abs = Resolve-RepoPath $rel
        if (Test-Path -LiteralPath $abs) { $existing += $rel }
    }
    $cleaned = $false
    if ($existing.Count -gt 0) {
        # git clean -fdx 只删未跟踪/被忽略的文件：third_party/weasel/output 里入库的
        # 7z.exe / curl.exe / *.bat 是脚手架，删了 makensis 就编不出来。
        $gitArgs = @('-C', $script:Repo, 'clean', '-fdxq', '--') + $existing
        & git @gitArgs 2>$null
        if ($LASTEXITCODE -eq 0) {
            $cleaned = $true
        }
        # 缓存是用旧内容盖掉入库文件的可能来源（output\ 与 include\ 都有入库文件）：
        # git clean 不碰已跟踪文件，所以逐个 checkout 回 HEAD，免得旧脚手架混进构建。
        foreach ($rel in $existing) {
            & git -C $script:Repo checkout -- $rel 2>$null
        }
    }
    if (-not $cleaned) {
        foreach ($rel in $existing) {
            Remove-Item -LiteralPath (Resolve-RepoPath $rel) -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path -LiteralPath $manifestFile) { Remove-Item -LiteralPath $manifestFile -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $vendorDir) {
        Get-ChildItem -LiteralPath $vendorDir -Filter 'weasel-*-installer.exe' -File -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    }
    Write-Host ("[wipe] 已删除缓存产物目录：{0}" -f (($existing -join ', ')))
}

# ---------------------------------------------------------------------------
# write 模式
# ---------------------------------------------------------------------------
function Invoke-WriteMode {
    Format-Header -Mode 'write'
    $inputs = Get-ExpectedInputs
    # 来源信息并进清单（只记录，不参与比对）
    $provenance = Get-ProvenanceInputs
    foreach ($k in $provenance.Keys) { $inputs[$k] = $provenance[$k] }
    $artifacts = [ordered]@{}
    foreach ($rel in (Get-RequiredArtifacts)) {
        $abs = Resolve-RepoPath $rel
        if (-not (Test-Path -LiteralPath $abs)) {
            Write-Host ("::error::全量构建结束后缺少产物 {0}：缓存不可写（只有完整产物才允许进缓存）" -f $rel)
            exit 1
        }
        $item = Get-Item -LiteralPath $abs
        $artifacts[$rel] = [ordered]@{
            bytes  = $item.Length
            sha256 = (Get-FileSha256 -Path $abs)
        }
    }
    foreach ($rel in (Get-ComponentBinaries)) {
        $abs = Resolve-RepoPath $rel
        $missing = @(Get-MissingBinaryStrings -Path $abs)
        if ($missing.Count -gt 0) {
            Write-Host ("::error::{0} 里搜不到 {1}：插件没编进去，不能进缓存" -f $rel, ($missing -join ', '))
            exit 1
        }
    }
    $installer = Find-WeaselInstaller
    if ($null -eq $installer) {
        Write-Host '::error::third_party/weasel/output/archives 下没有唯一的 weasel-*-installer.exe，缓存不可写'
        exit 1
    }
    if ($installer.Length -lt $MinInstallerBytes) {
        Write-Host ("::error::Weasel 安装包只有 {0:N1} MiB（< {1:N0} MiB），缓存不可写" -f ($installer.Length / 1MB), ($MinInstallerBytes / 1MB))
        exit 1
    }
    $installerHash = Get-FileSha256 -Path $installer.FullName
    New-Item -ItemType Directory -Force -Path $vendorDir | Out-Null
    $vendorCopy = Join-Path $vendorDir $installer.Name
    Copy-Item -LiteralPath $installer.FullName -Destination $vendorCopy -Force

    $manifest = [ordered]@{
        schema       = $ManifestSchema
        platform     = 'windows'
        runner_os    = [string]$env:RUNNER_OS
        cache_key    = [string]$env:RIME_CACHE_KEY
        created_at   = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        generated_by = 'scripts/ime/verify-win-build-cache.ps1'
        inputs       = $inputs
        artifacts    = $artifacts
        installer    = [ordered]@{
            name   = $installer.Name
            bytes  = $installer.Length
            sha256 = $installerHash
            vendor = 'packaging/windows/vendor/' + $installer.Name
            source = 'third_party/weasel/output/archives/' + $installer.Name
        }
    }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $manifestFile) | Out-Null
    ($manifest | ConvertTo-Json -Depth 8) | Set-Content -LiteralPath $manifestFile -Encoding utf8

    Write-Host ("[write] manifest: {0}" -f $manifestFile)
    Write-Host ("[write] artifacts: {0} 个；installer: {1} ({2:N1} MiB)" -f $artifacts.Count, $installer.Name, ($installer.Length / 1MB))
    Write-Host ("[write] librime {0} / plugin {1}" -f $inputs['librime_version'], $inputs['librime_plugin_sha256'])
    Add-Summary @(
        '### Windows 构建缓存已写入',
        '',
        ('- manifest：build/cache-manifest.json（librime {0}，插件 {1}）' -f $inputs['librime_version'], $inputs['librime_plugin_sha256']),
        ('- 产物：{0} 个 + Weasel 安装包 {1}（{2:N1} MiB）' -f $artifacts.Count, $installer.Name, ($installer.Length / 1MB))
    )
    exit 0
}

# ---------------------------------------------------------------------------
# verify 模式
# ---------------------------------------------------------------------------
function Invoke-VerifyMode {
    Format-Header -Mode 'verify'

    $manifest = $null
    if (-not (Test-Path -LiteralPath $manifestFile)) {
        Write-Host '[verify] 没有 cache-manifest.json：缓存未命中，或是一份没有清单的旧缓存'
        # 没有清单就没法判断产物对不对；哪怕 dist/ 里的 rime.dll 看着能用也必须删掉，
        # 否则 build-librime-win.ps1 的 Test-ArchArtifacts 会拿它跳过编译。
        if (-not $KeepInvalid) { Remove-CacheProducts }
        Set-ActionOutput -Name 'valid' -Value 'false'
        Set-ActionOutput -Name 'reason' -Value 'no-manifest'
        Write-Host 'cache=invalid reason=no-manifest'
        Write-Host '::notice::cache=invalid reason=no-manifest（按缓存未命中处理，走全量构建）'
        exit 0
    }
    try {
        $manifest = Get-Content -LiteralPath $manifestFile -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        $manifest = $null
    }
    $problems = New-Object System.Collections.Generic.List[string]
    if ($null -eq $manifest) {
        $problems.Add('manifest-unreadable')
    } else {
        if ([int](Get-JsonValue $manifest 'schema') -ne $ManifestSchema) {
            $problems.Add(('manifest-schema-{0}' -f (Get-JsonValue $manifest 'schema')))
        }
        foreach ($name in (Get-InputMismatches -Manifest $manifest -Expected (Get-ExpectedInputs))) {
            $problems.Add('inputs-changed:' + $name)
        }
        foreach ($p in (Test-Artifacts -Manifest $manifest)) { $problems.Add($p) }
        foreach ($p in (Test-Installer -Manifest $manifest)) { $problems.Add($p) }
    }

    if ($problems.Count -eq 0) {
        $installer = Find-WeaselInstaller
        $inputs = Get-JsonValue $manifest 'inputs'
        Write-Host '[verify] 输入指纹一致，产物哈希与组件名齐全'
        Write-Host ("[verify] rime.dll: {0}" -f ((Get-ComponentBinaries) -join ', '))
        Write-Host ("[verify] weasel installer: {0}" -f $installer.Name)
        Write-Host 'cache=hit'
        Add-Summary @(
            '### Windows 构建缓存命中（cache=hit）',
            '',
            ('- librime {0}，插件 {1}' -f [string](Get-JsonValue $inputs 'librime_version'), [string](Get-JsonValue $inputs 'librime_plugin_sha256')),
            ('- rime.dll 含 {0}；WeaselServer.exe / WeaselDeployer.exe 齐全' -f ($ExpectedComponents -join ' / ')),
            '- 跳过：bootstrap boost、build-librime-win.ps1、build-weasel.ps1'
        )
        Set-ActionOutput -Name 'valid' -Value 'true'
        Set-ActionOutput -Name 'reason' -Value ''
        exit 0
    }

    $reason = $problems[0]
    Write-Host '[verify] 缓存不可用：'
    foreach ($p in $problems) { Write-Host ('  - ' + $p) }
    Write-Host ('cache=invalid reason={0}' -f $reason)
    Write-Host ('::warning::cache=invalid reason={0}（共 {1} 条：{2}）——删掉缓存产物，走全量构建' -f $reason, $problems.Count, ($problems -join ', '))
    if (-not $KeepInvalid) {
        Remove-CacheProducts
    } else {
        Write-Host '[verify] -KeepInvalid：保留现场不删除'
    }
    Set-ActionOutput -Name 'valid' -Value 'false'
    Set-ActionOutput -Name 'reason' -Value $reason
    Add-Summary @(
        '### Windows 构建缓存作废（cache=invalid）',
        '',
        ('- reason：{0}' -f $reason),
        ('- 明细：{0}' -f (($problems | Select-Object -First 6) -join '; ')),
        '- 已删除缓存的产物目录，本次走全量构建（产物不受缓存影响）'
    )
    exit 0
}

# verify 出任何意外（脚本自身 bug、IO 抖动）都按 invalid 处理：缓存问题只允许让这次重建，
# 不允许把发布卡住 —— 反过来 write 失败要红，因为那说明这次构建的产物本身不完整。
try {
    if ($Mode -eq 'write') { Invoke-WriteMode } else { Invoke-VerifyMode }
} catch {
    $message = $_.Exception.Message
    if ($Mode -eq 'write') {
        Write-Host ("::error::写缓存清单失败：{0}" -f $message)
        exit 1
    }
    Write-Host ("::warning::缓存校验过程出错，按 invalid 处理：{0}" -f $message)
    Write-Host 'cache=invalid reason=verify-error'
    if (-not $KeepInvalid) {
        try { Remove-CacheProducts } catch { Write-Host ("[wipe] 清理失败（忽略）：{0}" -f $_.Exception.Message) }
    }
    Set-ActionOutput -Name 'valid' -Value 'false'
    Set-ActionOutput -Name 'reason' -Value 'verify-error'
    exit 0
}
