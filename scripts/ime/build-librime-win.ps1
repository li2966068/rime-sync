<#
.SYNOPSIS
  在 Windows 上从 vendored 源码自建 librime（third_party/librime，1.17.0），把
  plugins/rimesync-extra 一起编进 rime.dll，并整理成 Weasel 需要的目录布局。

.DESCRIPTION
  为什么自建：上游预编译包（get-rime.ps1 拉的 1.13.1）里没有外部插件加载逻辑
  （librime 的 plugins/PluginManager 在 _WIN32 下是 TODO，current_module_path() 直接返回空
  路径），所以我们的 rimesync-extra（v 模式 / 标点 Shift 备选 / a-h 选词）在 Windows 上
  完全不生效。唯一可行的办法是把插件以 **合并** 方式（BUILD_MERGED_PLUGINS=ON，librime 的
  默认值）编进 rime.dll —— 合并进来的模块名会进 src/rime/setup.cc 的 kDefaultModules，
  Weasel 不用任何改动就能拿到 rimesync_vnum / rimesync_punct / rimesync_vselect。

  流程（每一步都能单独复跑）：
    1. deps 子目录为空 → 按 pin 的 tag 浅克隆到 third_party/librime/deps/<name>
    2. 写 third_party/librime/env.bat —— 上游 README-windows.md 指定的唯一定制点。
       注意：仓库里的 env.bat.template 把 ARCH/CMAKE_GENERATOR/PLATFORM_TOOLSET 写死成
       Win32 + VS2019 + v142，而且用的是普通 set（不是 if not defined），build.bat 会无条件
       call 它 —— 直接 copy 模板会把外部传进去的 ARCH 之类覆盖掉，VS2022/v143 的 runner 就
       编出 32 位 + 旧工具集的产物。所以这里自己生成 env.bat。
    3. cmd /c build.bat deps nologging   （x64 / Win32 各一遍，deps 静态，独立安装前缀）
    4. cmd /c build.bat librime          （BUILD_MERGED_PLUGINS=ON → 带上 rimesync-extra）
    5. 断言两个 rime.dll 里都有 rimesync_vnum / rimesync_punct / rimesync_vselect 明文
       （组件名是普通 string 常量，UTF-8 即 ASCII，直接搜字节即可），顺手校验 PE 架构
    6. 按上游 get-rime.ps1 -use dev 的同一套映射摆进 third_party/weasel：
         dist\x64\lib\rime.lib       -> <Weasel>\lib64\rime.lib
         dist\x64\lib\rime.dll       -> <Weasel>\output\rime.dll       (+ rime.pdb)
         dist\Win32\lib\rime.lib     -> <Weasel>\lib\rime.lib
         dist\Win32\lib\rime.dll     -> <Weasel>\output\Win32\rime.dll (+ rime.pdb)
         dist\x64\include\*.h        -> <Weasel>\include\
         build\deps-x64\share\opencc -> <Weasel>\output\data\opencc\  (简繁转换数据)

  产物位置全部落在 third_party/librime/{build,dist} 下（两者都在上游 .gitignore 里），
  仓库工作区不会多出未跟踪文件。

.PARAMETER BoostRoot
  Boost 源码根目录（里面有 boost\ 与 stage\lib）。默认取 Weasel 已经建好的
  third_party/weasel/deps/boost_1_84_0，让 Weasel 与 librime 用同一份 boost。

.PARAMETER Rebuild
  忽略已存在的 build\ / dist\ 产物，从零重编（deps 也重编）。

.PARAMETER SkipDeps
  复用已编好的 deps 安装前缀（build\deps-<arch>），只重编 librime。

.PARAMETER WithTests
  克隆真正的 googletest 并让 build.bat 编它（BUILD_TEST 用）。默认跳过：build.bat deps
  会无条件编 googletest，这里放一个最小占位工程让它空转。

.PARAMETER NoDeploy
  只编到 third_party/librime/dist，不往 third_party/weasel 摆产物。

.PARAMETER Check
  只打印依赖/工具链状态与 pin 表，不克隆、不编译。

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/build-librime-win.ps1 -Check

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/build-librime-win.ps1
#>
[CmdletBinding()]
param(
    [string]$RepoRoot = '',
    [string]$LibrimeRoot = '',
    [string]$WeaselRoot = '',
    [string]$BoostRoot = '',
    [string]$Generator = 'Visual Studio 17 2022',
    [string]$PlatformToolset = 'v143',
    [switch]$SkipWin32,
    [switch]$SkipDeps,
    [switch]$Rebuild,
    [switch]$WithTests,
    [switch]$NoDeploy,
    [switch]$Check
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---- deps pin：与 third_party/librime/.gitmodules 声明的 6 个路径一一对应 ----
# tag 已按上游仓库的实际命名核对过（marisa-trie 是 v0.2.6、OpenCC 是 ver.1.1.7）。
# googletest 只在 -WithTests 时真克隆；其余 5 个是 rime.dll 的必需依赖。
# 版本 = librime 1.17.0 的 .gitmodules 里锁定的子模块提交（与上游完全一致）。
# 用提交而不是 tag：marisa-trie 的 v0.2.6 没有 CMakeLists.txt（只有 autotools），
# 上游锁定的提交才带 CMake 支持，build.bat 才能 cmake 它。
$script:DepsPins = [ordered]@{
    'glog'        = @{ Url = 'https://github.com/google/glog.git';        Tag = 'v0.6.0';      Commit = '7b134a5c82c0c0b5698bb6bf7a835b230c5638e4' }
    'leveldb'     = @{ Url = 'https://github.com/google/leveldb.git';     Tag = '1.23';        Commit = '99b3c03b3284f5886f9ef9a4ef703d57373e61be' }
    'yaml-cpp'    = @{ Url = 'https://github.com/jbeder/yaml-cpp.git';    Tag = '0.8.0';       Commit = '2f86d13775d119edbb69af52e5f566fd65c6953b' }
    'marisa-trie' = @{ Url = 'https://github.com/s-yata/marisa-trie.git'; Tag = 'v0.2.6+cmake'; Commit = '3e87d53b78e15f2f43783d5e376561a8c9722051' }
    'opencc'      = @{ Url = 'https://github.com/BYVoid/OpenCC.git';      Tag = 'ver.1.1.7';   Commit = '556ed22496d650bd0b13b6c163be9814637970ae' }
}
$script:GoogletestPin = @{ Url = 'https://github.com/google/googletest.git'; Tag = 'v1.14.0'; Commit = 'f8d7d77c06936315286eb55f8de22cd23c188571' }

# 组件名 = plugins/rimesync-extra/rimesync_extra_module.cc 里 registry 注册的字符串
$script:LibrimeComponents = @('rimesync_vnum', 'rimesync_punct', 'rimesync_vselect')
$script:PlaceholderMarker = 'rimesync-googletest-placeholder.txt'

$script:RepoRoot = ''
$script:LibrimeDir = ''
$script:DistDir = ''
$script:WeaselDir = ''
$script:BoostDir = ''
$script:LibrimeVersion = ''
$script:VsDevCmd = ''

# ---------------------------------------------------------------------------
# 基础工具
# ---------------------------------------------------------------------------
function Write-Step {
    param([string]$Text)
    Write-Host ''
    Write-Host "==> $Text"
}

function Assert-File {
    param([string]$Path, [string]$Hint = '')
    if (-not (Test-Path -LiteralPath $Path)) {
        $msg = "缺少文件：$Path"
        if ($Hint) { $msg = $msg + [Environment]::NewLine + $Hint }
        throw $msg
    }
}

function Get-TextEncodingName {
    if ($PSVersionTable.PSVersion.Major -ge 6) { return 'oem' }
    return 'Default'
}

# ---------------------------------------------------------------------------
# VS 开发者环境
# 这里单独实现一份（build-weasel.ps1 里有同样的一份）：两个脚本各自可独立运行，
# 不共享 dot-source 的库文件，避免改一处把另一条已经跑通的 CI 路径带崩。
# ---------------------------------------------------------------------------
function Get-VsDevCmd {
    if ($script:VsDevCmd) { return $script:VsDevCmd }

    $installPath = ''
    $pf86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    $pf = [Environment]::GetEnvironmentVariable('ProgramFiles')
    if ($pf86) {
        $vswhere = Join-Path $pf86 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path -LiteralPath $vswhere) {
            $found = @(& $vswhere -latest -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property installationPath 2>$null)
            foreach ($line in $found) {
                $candidate = ([string]$line).Trim()
                if ($candidate -and (Test-Path -LiteralPath (Join-Path $candidate 'Common7\Tools\VsDevCmd.bat'))) {
                    $installPath = $candidate
                    break
                }
            }
        }
    }

    if (-not $installPath) {
        foreach ($root in @($pf, $pf86)) {
            if (-not $root) { continue }
            $vsRoot = Join-Path $root 'Microsoft Visual Studio'
            if (-not (Test-Path -LiteralPath $vsRoot)) { continue }
            foreach ($ver in @(Get-ChildItem -LiteralPath $vsRoot -Directory -ErrorAction SilentlyContinue)) {
                foreach ($ed in @(Get-ChildItem -LiteralPath $ver.FullName -Directory -ErrorAction SilentlyContinue)) {
                    if (Test-Path -LiteralPath (Join-Path $ed.FullName 'Common7\Tools\VsDevCmd.bat')) {
                        $installPath = $ed.FullName
                        break
                    }
                }
            }
        }
    }

    if (-not $installPath) {
        throw '找不到 Visual Studio 的 VsDevCmd.bat（需要 VS2022 + “使用 C++ 的桌面开发”）。'
    }
    $script:VsDevCmd = Join-Path $installPath 'Common7\Tools\VsDevCmd.bat'
    return $script:VsDevCmd
}

# 在 dev shell 里跑一组 cmd 命令；输出直通日志，返回退出码。
function Invoke-DevShell {
    param(
        [Parameter(Mandatory = $true)][string]$Arch,      # amd64 / x86
        [Parameter(Mandatory = $true)][string]$WorkDir,
        [Parameter(Mandatory = $true)][string[]]$Lines,
        [switch]$AllowFailure,
        [string]$Label = ''
    )
    $devCmd = Get-VsDevCmd
    $bat = Join-Path ([System.IO.Path]::GetTempPath()) ("rime-sync-librime-{0}.cmd" -f ([guid]::NewGuid().ToString('N')))
    $content = @('@echo off', 'setlocal') +
               @('call "' + $devCmd + '" -arch=' + $Arch + ' -host_arch=amd64 -no_logo') +
               @('if errorlevel 1 exit /b 9001') +
               @('cd /d "' + $WorkDir + '"') +
               $Lines +
               @('set "RIMESYNC_RC=%ERRORLEVEL%"', 'exit /b %RIMESYNC_RC%')
    Set-Content -LiteralPath $bat -Value $content -Encoding (Get-TextEncodingName)
    try {
        & cmd.exe /c $bat 2>&1 | Out-Host
        $code = 0
        try { $code = [int]$LASTEXITCODE } catch { $code = 0 }
    } finally {
        Remove-Item -LiteralPath $bat -Force -ErrorAction SilentlyContinue
    }
    if (($code -ne 0) -and (-not $AllowFailure)) {
        $text = '开发者环境命令失败（exit ' + $code + '）'
        if ($Label) { $text = $text + '：' + $Label }
        throw $text
    }
    return $code
}

# ---------------------------------------------------------------------------
# 二进制断言（明文组件名 / PE 架构）
# ---------------------------------------------------------------------------
function Get-MissingComponents {
    param(
        [Parameter(Mandatory = $true)][string]$Dll,
        [string[]]$Names = $script:LibrimeComponents
    )
    $bytes = [System.IO.File]::ReadAllBytes($Dll)
    # ISO-8859-1 = 字节到字符 1:1，二进制里搜 ASCII 明文就用它
    $text = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $missing = @()
    foreach ($name in $Names) {
        if ($text.IndexOf($name, [System.StringComparison]::Ordinal) -lt 0) { $missing += $name }
    }
    return $missing
}

function Get-PeMachine {
    param([Parameter(Mandatory = $true)][string]$Path)
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $br = New-Object System.IO.BinaryReader($fs)
        if ($br.ReadUInt16() -ne 0x5A4D) { return 'not-pe' }
        $fs.Position = 0x3C
        $peOffset = $br.ReadInt32()
        if ($peOffset -le 0 -or $peOffset -ge ($fs.Length - 6)) { return 'not-pe' }
        $fs.Position = $peOffset
        if ($br.ReadUInt32() -ne 0x00004550) { return 'not-pe' }
        $machine = $br.ReadUInt16()
        switch ($machine) {
            0x8664 { return 'x64' }
            0x014C { return 'x86' }
            0xAA64 { return 'arm64' }
            default { return ('0x{0:x4}' -f $machine) }
        }
    } finally {
        $fs.Dispose()
    }
}

# ---------------------------------------------------------------------------
# 上下文
# ---------------------------------------------------------------------------
function Get-LibrimeVersion {
    $cml = Join-Path $script:LibrimeDir 'CMakeLists.txt'
    if (Test-Path -LiteralPath $cml) {
        $raw = Get-Content -LiteralPath $cml -Raw -Encoding UTF8
        $m = [regex]::Match($raw, '(?m)^\s*set\(rime_version\s+([0-9][0-9A-Za-z.\-]*)\s*\)')
        if ($m.Success) { return $m.Groups[1].Value }
    }
    return 'unknown'
}

function Resolve-BuildContext {
    if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot) }
    $script:RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
    if (-not $LibrimeRoot) { $LibrimeRoot = Join-Path $script:RepoRoot 'third_party\librime' }
    $script:LibrimeDir = (Resolve-Path -LiteralPath $LibrimeRoot).Path
    if (-not $WeaselRoot) { $WeaselRoot = Join-Path $script:RepoRoot 'third_party\weasel' }
    $script:WeaselDir = [System.IO.Path]::GetFullPath($WeaselRoot)
    $script:DistDir = Join-Path $script:LibrimeDir 'dist'
    if (-not $BoostRoot) { $BoostRoot = Join-Path $script:WeaselDir 'deps\boost_1_84_0' }
    $script:BoostDir = [System.IO.Path]::GetFullPath($BoostRoot)
    $script:LibrimeVersion = Get-LibrimeVersion
}

function Test-Scaffolding {
    Assert-File (Join-Path $script:LibrimeDir 'build.bat') 'third_party/librime 未入库或为空：见 FORK.md。'
    Assert-File (Join-Path $script:LibrimeDir 'CMakeLists.txt')
    Assert-File (Join-Path $script:LibrimeDir 'cmake\cxx_flag_overrides.cmake')
    Assert-File (Join-Path $script:LibrimeDir 'plugins\rimesync-extra\CMakeLists.txt') '插件目录丢了：自建 librime 的唯一目的就是把 rimesync-extra 合进 rime.dll。'
    Assert-File (Join-Path $script:LibrimeDir 'plugins\rimesync-extra\rimesync_extra_module.cc')
}

function Get-ArchInfo {
    param([Parameter(Mandatory = $true)][string]$Name)   # x64 / Win32
    if ($Name -eq 'x64') {
        return [pscustomobject]@{
            Name          = 'x64'
            VsArch        = 'amd64'
            Platform      = 'x64'
            BuildDir      = 'build\x64'
            DepsPrefix    = 'build\deps-x64'
            RimePrefix    = 'dist\x64'
            ExpectMachine = 'x64'
        }
    }
    return [pscustomobject]@{
        Name          = 'Win32'
        VsArch        = 'x86'
        Platform      = 'Win32'
        BuildDir      = 'build\win32'
        DepsPrefix    = 'build\deps-win32'
        RimePrefix    = 'dist\Win32'
        ExpectMachine = 'x86'
    }
}

# ---------------------------------------------------------------------------
# deps：按 pin 的 tag 浅克隆
# ---------------------------------------------------------------------------
function Get-DepDir {
    param([Parameter(Mandatory = $true)][string]$Name)
    return (Join-Path (Join-Path $script:LibrimeDir 'deps') $Name)
}

function Test-DepCloned {
    param([Parameter(Mandatory = $true)][string]$Name)
    $dir = Get-DepDir -Name $Name
    if (Test-Path -LiteralPath (Join-Path $dir 'CMakeLists.txt')) { return $true }
    # 少数依赖的某个历史 tag 只有 autotools；只要目录里有源码入口就算克隆成功，
    # 真正的可用性由后面的 cmake configure 判断。
    return (Test-Path -LiteralPath (Join-Path $dir 'configure.ac'))
}

function Get-GitHead {
    param([Parameter(Mandatory = $true)][string]$Dir)
    if (-not (Test-Path -LiteralPath (Join-Path $Dir '.git'))) { return '' }
    $old = (Get-Location).Path
    Push-Location -LiteralPath $Dir
    try {
        return ([string](& git rev-parse HEAD 2>$null)).Trim()
    } catch {
        return ''
    } finally {
        Pop-Location
        if ((Get-Location).Path -ne $old) { Set-Location -LiteralPath $old }
    }
}

function Ensure-DepSource {
    param([Parameter(Mandatory = $true)][string]$Name)
    $dir = Get-DepDir -Name $Name
    if (Test-DepCloned -Name $Name) {
        Write-Host "[deps] $Name 已存在（复用 $dir）"
        return
    }
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }

    if ($Name -eq 'googletest' -and -not $WithTests) {
        # build.bat deps 会无条件 pushd deps\googletest 并 cmake+install；
        # 给一个最小占位工程让它空转（googletest 只有 BUILD_TEST=ON 时才被真正用到）。
        $markerPath = Join-Path $dir $script:PlaceholderMarker
        $cmlPath = Join-Path $dir 'CMakeLists.txt'
        if (Test-Path -LiteralPath $markerPath) {
            Write-Host '[deps] googletest 占位工程已存在（未开 -WithTests，跳过真实克隆）'
            return
        }
        if (Test-Path -LiteralPath $cmlPath) {
            Write-Host '[deps] googletest 目录里已有真实源码（保留，让 build.bat 编它）'
            return
        }
        $stamp = (Join-Path $dir $script:PlaceholderMarker) -replace '\\', '/'
        Set-Content -LiteralPath $cmlPath -Encoding ascii -Value @(
            '# Generated by scripts/ime/build-librime-win.ps1 (do not edit).',
            '# Purpose: upstream build.bat deps always builds googletest; this stub makes it a no-op.',
            '# For a real googletest clone run this script with -WithTests.',
            'cmake_minimum_required(VERSION 3.12)',
            'project(rimesync_googletest_placeholder NONE)',
            ('install(FILES "' + $stamp + '" DESTINATION "share/rimesync-placeholder")')
        )
        Set-Content -LiteralPath $markerPath -Encoding ascii -Value 'placeholder; not googletest'
        Write-Host '[deps] googletest 用占位工程（不编 gtest；-WithTests 可改为真实克隆）'
        return
    }

    if ($Name -eq 'googletest') {
        $pin = $script:GoogletestPin
    } else {
        $pin = $script:DepsPins[$Name]
    }
    Write-Step ("克隆 deps/$Name（" + $pin.Tag + '）')
    # 浅克隆到 tag。这 6 个仓库里只有 leveldb 声明了 submodule（gtest/benchmark），
    # 而两者都只在 LEVELDB_BUILD_TESTS / BENCHMARKS 打开时才 add_subdirectory，
    # 我们把两个都关掉，所以不需要 --recurse-submodules。
    $code = 0
    if ($pin.Commit) {
        # 按提交取：git init + fetch --depth 1 <sha> + checkout FETCH_HEAD（GitHub 支持按 SHA 浅取）。
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Push-Location -LiteralPath $dir
        try {
            & git init -q . 2>&1 | Out-Host
            & git remote add origin $pin.Url 2>&1 | Out-Host
            & git fetch --depth 1 origin $pin.Commit 2>&1 | Out-Host
            try { $code = [int]$LASTEXITCODE } catch { $code = 0 }
            if ($code -eq 0) { & git checkout -q FETCH_HEAD 2>&1 | Out-Host }
        } finally { Pop-Location }
    }
    if (($code -ne 0) -or (-not (Test-DepCloned -Name $Name))) {
        # 退路：整库克隆再切到目标提交。
        Write-Warning ("[deps] $Name 按提交浅取失败（exit $code），改为完整克隆 " + $pin.Url)
        if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
        & git clone $pin.Url $dir 2>&1 | Out-Host
        try { $code = [int]$LASTEXITCODE } catch { $code = 0 }
        if ($code -eq 0 -and $pin.Commit) {
            Push-Location -LiteralPath $dir
            try { & git checkout -q $pin.Commit 2>&1 | Out-Host } finally { Pop-Location }
        }
        if (($code -ne 0) -or (-not (Test-DepCloned -Name $Name))) {
            throw ("deps/$Name 克隆失败（exit $code）：" + $pin.Url + ' @ ' + $pin.Commit + ' -> ' + $dir)
        }
    }
    Write-Host ("[deps] $Name = " + $pin.Tag + ' @ ' + (Get-GitHead -Dir $dir))
}

function Remove-GoogletestPlaceholder {
    $dir = Get-DepDir -Name 'googletest'
    if (-not (Test-Path -LiteralPath (Join-Path $dir $script:PlaceholderMarker))) { return }
    # 编完就删：留一个假的 googletest 会让后来手工跑 build.bat deps 的人以为 gtest 已经编过
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host '[deps] 已清理 googletest 占位工程'
}

# ---------------------------------------------------------------------------
# env.bat：build.bat 唯一认可的定制点
# ---------------------------------------------------------------------------
function Write-EnvBat {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    $path = Join-Path $script:LibrimeDir 'env.bat'
    $lines = @(
        '@echo off',
        'rem Generated by scripts/ime/build-librime-win.ps1 (do not edit).',
        'rem Upstream env.bat.template hardcodes ARCH=Win32 / VS2019 / v142, so we write our own.',
        'set RIME_ROOT=%CD%',
        ('set BOOST_ROOT=' + $script:BoostDir),
        ('set ARCH=' + $ArchInfo.Platform),
        ('set CMAKE_GENERATOR="' + $Generator + '"'),
        ('set PLATFORM_TOOLSET=' + $PlatformToolset),
        'set BJAM_TOOLSET=msvc-14.3'
    )
    Set-Content -LiteralPath $path -Value $lines -Encoding (Get-TextEncodingName)
    Write-Host "[env] env.bat: ARCH=$($ArchInfo.Platform) generator=$Generator toolset=$PlatformToolset"
    return $path
}

function Test-BoostReady {
    $boostHpp = Join-Path $script:BoostDir 'boost\version.hpp'
    $stage = Join-Path $script:BoostDir 'stage\lib'
    return ((Test-Path -LiteralPath $boostHpp) -and (Test-Path -LiteralPath $stage))
}

# ---------------------------------------------------------------------------
# 编译
# ---------------------------------------------------------------------------
function Get-ArchArtifacts {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    $prefix = Join-Path $script:LibrimeDir $ArchInfo.RimePrefix
    return [pscustomobject]@{
        Prefix = $prefix
        Dll    = (Join-Path $prefix 'lib\rime.dll')
        Lib    = (Join-Path $prefix 'lib\rime.lib')
        Header = (Join-Path $prefix 'include\rime_api.h')
        Pdb    = (Join-Path $prefix 'lib\rime.pdb')
    }
}

function Test-ArchArtifacts {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    $art = Get-ArchArtifacts -ArchInfo $ArchInfo
    foreach ($p in @($art.Dll, $art.Lib, $art.Header)) {
        if (-not (Test-Path -LiteralPath $p)) { return $false }
    }
    $missing = @(Get-MissingComponents -Dll $art.Dll)
    if ($missing.Count -gt 0) {
        Write-Host ("[librime/$($ArchInfo.Name)] 已有产物缺组件（" + ($missing -join ', ') + '）：视为不可用，重编')
        return $false
    }
    return $true
}

function Invoke-DepsBuild {
    param(
        [Parameter(Mandatory = $true)][pscustomobject]$ArchInfo,
        [switch]$Clean
    )
    $depsPrefix = Join-Path $script:LibrimeDir $ArchInfo.DepsPrefix
    Write-Step "编译 deps（$($ArchInfo.Name) -> $($ArchInfo.DepsPrefix)）"
    $build = 'call build.bat deps nologging'
    if ($Clean) { $build = 'call build.bat deps clean nologging' }
    $lines = @(
        ('set "deps_install_prefix=' + $depsPrefix + '"'),
        ('set "build_dir=' + $ArchInfo.BuildDir + '"'),
        $build
    )
    $code = Invoke-DevShell -Arch $ArchInfo.VsArch -WorkDir $script:LibrimeDir -Lines $lines -Label ("build.bat deps（" + $ArchInfo.Name + '）') -AllowFailure
    if ($code -ne 0) {
        throw ("build.bat deps 失败（exit $code，" + $ArchInfo.Name + '）。' +
               '若报错来自 deps\googletest（占位工程在 VS 生成器下没给出 install 目标），' +
               '用 -WithTests 让它真实克隆并编译 googletest；deps 已经编好时可用 -SkipDeps 跳过。')
    }
}

function Invoke-LibrimeBuild {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    $depsPrefix = Join-Path $script:LibrimeDir $ArchInfo.DepsPrefix
    $rimePrefix = Join-Path $script:LibrimeDir $ArchInfo.RimePrefix
    if (-not (Test-Path -LiteralPath (Join-Path $depsPrefix 'lib'))) {
        throw "deps 安装前缀不存在：$depsPrefix（先跑一次不带 -SkipDeps 的构建）"
    }
    if ($Rebuild) {
        Remove-Item -LiteralPath (Join-Path $script:LibrimeDir $ArchInfo.BuildDir) -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $rimePrefix -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Step "编译 librime（$($ArchInfo.Name)，BUILD_MERGED_PLUGINS=ON）"
    $lines = @(
        ('set "deps_install_prefix=' + $depsPrefix + '"'),
        ('set "rime_install_prefix=' + $rimePrefix + '"'),
        ('set "build_dir=' + $ArchInfo.BuildDir + '"'),
        'call build.bat librime nologging'
    )
    Invoke-DevShell -Arch $ArchInfo.VsArch -WorkDir $script:LibrimeDir -Lines $lines -Label ("build.bat librime（" + $ArchInfo.Name + '）') | Out-Null
}

function Assert-ArchArtifacts {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    $art = Get-ArchArtifacts -ArchInfo $ArchInfo
    Assert-File $art.Dll "build.bat librime 结束但没有 $($ArchInfo.RimePrefix)\lib\rime.dll"
    Assert-File $art.Lib "build.bat librime 结束但没有 $($ArchInfo.RimePrefix)\lib\rime.lib"
    Assert-File $art.Header "build.bat librime 结束但没有 $($ArchInfo.RimePrefix)\include\rime_api.h"

    $machine = Get-PeMachine -Path $art.Dll
    if ($machine -ne $ArchInfo.ExpectMachine) {
        throw ("$($art.Dll) 的 PE 架构是 $machine，期望 $($ArchInfo.ExpectMachine)：" +
               'ARCH 没生效（env.bat 被 env.bat.template 覆盖了？）')
    }

    $missing = @(Get-MissingComponents -Dll $art.Dll)
    if ($missing.Count -gt 0) {
        $sep = [Environment]::NewLine
        throw ("$($art.Dll) 里搜不到插件组件名：" + ($missing -join ', ') + $sep +
               '含义：rime.dll 没有合并 plugins/rimesync-extra —— Windows 上的 v 模式 / 标点 Shift 备选 / a-h 选词 不会生效。' + $sep +
               '排查：①build.bat librime 的 cmake 输出里应有 "Found plugin: rimesync-extra"；' +
               '②BUILD_MERGED_PLUGINS 默认 ON，别被 -DBUILD_MERGED_PLUGINS=OFF 关掉；' +
               '③确认 plugins/rimesync-extra 还在（上游 .gitignore 有 !plugins/rimesync-extra/ 例外）。')
    }
    Write-Host ("[librime/$($ArchInfo.Name)] $($art.Dll) = $machine；" + ($script:LibrimeComponents -join ' / ') + ' 全部命中')
}

# ---------------------------------------------------------------------------
# 摆进 Weasel 布局（映射与上游 get-rime.ps1 -use dev 完全一致）
# ---------------------------------------------------------------------------
function Deploy-ToWeasel {
    param([Parameter(Mandatory = $true)][pscustomobject]$ArchInfo)
    if (-not (Test-Path -LiteralPath (Join-Path $script:WeaselDir 'weasel.sln'))) {
        throw ("$($script:WeaselDir) 不是 Weasel 源码树（缺 weasel.sln）：" +
               '用 -NoDeploy 只编 librime，或补上 third_party/weasel。')
    }

    $art = Get-ArchArtifacts -ArchInfo $ArchInfo
    $output = Join-Path $script:WeaselDir 'output'

    if ($ArchInfo.Name -eq 'x64') {
        $includeDir = Join-Path $script:WeaselDir 'include'
        New-Item -ItemType Directory -Force -Path $includeDir | Out-Null
        $srcInclude = Join-Path $art.Prefix 'include'
        foreach ($h in @(Get-ChildItem -LiteralPath $srcInclude -Filter '*.h' -File)) {
            Copy-Item -LiteralPath $h.FullName -Destination $includeDir -Force
        }
        $headerCount = @(Get-ChildItem -LiteralPath $includeDir -Filter 'rime_*.h' -File).Count
        Write-Host "[deploy] 头文件 -> include\（$headerCount 个 rime_*.h）"

        $lib64 = Join-Path $script:WeaselDir 'lib64'
        New-Item -ItemType Directory -Force -Path $lib64 | Out-Null
        Copy-Item -LiteralPath $art.Lib -Destination (Join-Path $lib64 'rime.lib') -Force

        New-Item -ItemType Directory -Force -Path $output | Out-Null
        Copy-Item -LiteralPath $art.Dll -Destination (Join-Path $output 'rime.dll') -Force
        if (Test-Path -LiteralPath $art.Pdb) { Copy-Item -LiteralPath $art.Pdb -Destination (Join-Path $output 'rime.pdb') -Force }

        # opencc 词典数据（self-built opencc 的 install 目标会带 share\opencc）
        $openccSrc = Join-Path (Join-Path $script:LibrimeDir $ArchInfo.DepsPrefix) 'share\opencc'
        $openccDst = Join-Path $output 'data\opencc'
        if (Test-Path -LiteralPath $openccSrc) {
            New-Item -ItemType Directory -Force -Path $openccDst | Out-Null
            $count = 0
            foreach ($f in @(Get-ChildItem -LiteralPath $openccSrc -File)) {
                Copy-Item -LiteralPath $f.FullName -Destination $openccDst -Force
                $count++
            }
            Write-Host "[deploy] opencc 数据 -> output\data\opencc\（$count 个文件）"
        } else {
            Write-Warning "[deploy] 没找到 $openccSrc：安装包将不含简繁/异体转换数据（install.nsi 的可选组会自动关掉）。"
        }
        return
    }

    $lib = Join-Path $script:WeaselDir 'lib'
    New-Item -ItemType Directory -Force -Path $lib | Out-Null
    Copy-Item -LiteralPath $art.Lib -Destination (Join-Path $lib 'rime.lib') -Force

    $outWin32 = Join-Path $output 'Win32'
    New-Item -ItemType Directory -Force -Path $outWin32 | Out-Null
    Copy-Item -LiteralPath $art.Dll -Destination (Join-Path $outWin32 'rime.dll') -Force
    if (Test-Path -LiteralPath $art.Pdb) { Copy-Item -LiteralPath $art.Pdb -Destination (Join-Path $outWin32 'rime.pdb') -Force }
}

function Assert-WeaselLayout {
    $checks = @('lib64\rime.lib', 'output\rime.dll', 'include\rime_api.h')
    $dlls = @((Join-Path $script:WeaselDir 'output\rime.dll'))
    if (-not $SkipWin32) {
        $checks += @('lib\rime.lib', 'output\Win32\rime.dll')
        $dlls += (Join-Path $script:WeaselDir 'output\Win32\rime.dll')
    }
    foreach ($rel in $checks) {
        Assert-File (Join-Path $script:WeaselDir $rel) "部署后仍缺 $rel（Weasel 的 msbuild 会链接失败）"
    }
    foreach ($dll in $dlls) {
        $missing = @(Get-MissingComponents -Dll $dll)
        if ($missing.Count -gt 0) {
            throw ("部署到 Weasel 的 $dll 里搜不到：" + ($missing -join ', ') + ' —— 那里装的还是预编译包？')
        }
    }
    Write-Host ("[deploy] Weasel 布局就绪：" + ($checks -join ', ') + '（rime.dll 均含插件组件名）')
}

function Write-BuildRecord {
    param([object[]]$Arches)
    $deps = @()
    foreach ($name in $script:DepsPins.Keys) {
        $pin = $script:DepsPins[$name]
        $deps += [ordered]@{
            name   = $name
            tag    = $pin.Tag
            url    = $pin.Url
            commit = (Get-GitHead -Dir (Get-DepDir -Name $name))
        }
    }
    $record = [ordered]@{
        librime_version = $script:LibrimeVersion
        source           = 'self-built (merged rimesync-extra)'
        librime_root     = $script:LibrimeDir
        boost_root       = $script:BoostDir
        generator        = $Generator
        platform_toolset = $PlatformToolset
        components       = $script:LibrimeComponents
        deps             = $deps
        arches           = @($Arches)
        built_at         = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    }
    New-Item -ItemType Directory -Force -Path $script:DistDir | Out-Null
    $path = Join-Path $script:DistDir 'LIBRIME-BUILD.json'
    $record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $path -Encoding utf8
    Write-Host "[record] $path"
    return $record
}

# ---------------------------------------------------------------------------
# 前置检查（-Check）
# ---------------------------------------------------------------------------
function Invoke-Preflight {
    $devCmd = 'missing'
    try { $devCmd = Get-VsDevCmd } catch { $devCmd = 'missing: ' + $_.Exception.Message }
    $rows = @(
        [pscustomobject]@{ Name = 'librime root';     Value = $script:LibrimeDir },
        [pscustomobject]@{ Name = 'librime version';  Value = $script:LibrimeVersion },
        [pscustomobject]@{ Name = 'weasel root';      Value = $script:WeaselDir },
        [pscustomobject]@{ Name = 'boost root';       Value = $script:BoostDir },
        [pscustomobject]@{ Name = 'boost ready';      Value = (Test-BoostReady) },
        [pscustomobject]@{ Name = 'VsDevCmd';         Value = $devCmd },
        [pscustomobject]@{ Name = 'generator';        Value = ($Generator + ' / ' + $PlatformToolset) },
        [pscustomobject]@{ Name = 'plugin dir';       Value = (Test-Path -LiteralPath (Join-Path $script:LibrimeDir 'plugins\rimesync-extra\CMakeLists.txt')) },
        [pscustomobject]@{ Name = 'skip Win32';       Value = [bool]$SkipWin32 },
        [pscustomobject]@{ Name = 'dist x64 ready';   Value = (Test-ArchArtifacts -ArchInfo (Get-ArchInfo 'x64')) },
        [pscustomobject]@{ Name = 'dist Win32 ready'; Value = (Test-ArchArtifacts -ArchInfo (Get-ArchInfo 'Win32')) }
    )
    $rows | Format-Table -AutoSize | Out-String | Write-Host

    Write-Host 'deps pin（.gitmodules 路径 -> 本次使用的 tag）：'
    foreach ($name in $script:DepsPins.Keys) {
        $pin = $script:DepsPins[$name]
        $state = '待克隆'
        if (Test-DepCloned -Name $name) { $state = '已存在' }
        Write-Host ("  {0,-12} {1,-11} {2}  [{3}]" -f $name, $pin.Tag, $pin.Url, $state)
    }
    $gtState = '占位（不编）'
    if ($WithTests) { $gtState = '真实克隆 + 编译' }
    Write-Host ("  {0,-12} {1,-11} {2}  [{3}]" -f 'googletest', $script:GoogletestPin.Tag, $script:GoogletestPin.Url, $gtState)
    Write-Host ''
    Write-Host '计划：build.bat deps -> build.bat librime（x64 + Win32，合并 rimesync-extra）-> 断言组件名 -> 摆进 third_party/weasel'
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
Write-Host 'rime-sync · 自建 librime（合并 rimesync-extra）'
Resolve-BuildContext
Test-Scaffolding
Write-Host ("librime={0}  插件=rimesync-extra  deps pin={1} 个" -f $script:LibrimeVersion, $script:DepsPins.Count)

if ($Check) {
    Invoke-Preflight
    Write-Host '[check] 前置检查通过（未克隆、未编译）'
    exit 0
}

if (-not (Test-BoostReady)) {
    throw ("BOOST_ROOT 不可用：$($script:BoostDir)（需要 boost\version.hpp 与 stage\lib）。" +
           '先跑 scripts/ime/build-weasel.ps1（它会自建 boost），或用 -BoostRoot 指定已有的 boost 源码树。')
}

$arches = @((Get-ArchInfo 'x64'))
if (-not $SkipWin32) { $arches += (Get-ArchInfo 'Win32') }

foreach ($name in $script:DepsPins.Keys) { Ensure-DepSource -Name $name }
# googletest 目录必须存在：build.bat deps 会无条件 pushd 它。
# -WithTests 时真实克隆，否则写一个最小占位工程（由函数内部决定）。
Ensure-DepSource -Name 'googletest'

foreach ($arch in $arches) {
    if ($SkipDeps) {
        Write-Host ("[deps] -SkipDeps：复用 " + (Join-Path $script:LibrimeDir $arch.DepsPrefix))
        continue
    }
    Write-EnvBat -ArchInfo $arch | Out-Null
    Invoke-DepsBuild -ArchInfo $arch -Clean:$Rebuild
}
# 两个架构都编完再清理占位工程：build.bat deps 每轮都会 pushd deps\googletest，
# 提前删掉会让第二轮在错误的目录里跑 cmake。
Remove-GoogletestPlaceholder

$built = @()
foreach ($arch in $arches) {
    if (-not $Rebuild -and (Test-ArchArtifacts -ArchInfo $arch)) {
        Write-Host "[librime/$($arch.Name)] dist 产物已就绪且含插件组件名，跳过编译（-Rebuild 可强制重编）"
    } else {
        Write-EnvBat -ArchInfo $arch | Out-Null
        Invoke-LibrimeBuild -ArchInfo $arch
        Assert-ArchArtifacts -ArchInfo $arch
    }
    $art = Get-ArchArtifacts -ArchInfo $arch
    $built += [ordered]@{ arch = $arch.Name; dll = $art.Dll; machine = (Get-PeMachine -Path $art.Dll) }
}

$null = Write-BuildRecord -Arches $built

if ($NoDeploy) {
    Write-Host '[deploy] -NoDeploy：产物只在 third_party/librime/dist，未改动 third_party/weasel'
} else {
    foreach ($arch in $arches) { Deploy-ToWeasel -ArchInfo $arch }
    Assert-WeaselLayout
}

if ($env:GITHUB_STEP_SUMMARY) {
    $lines = @(
        '### 自建 librime',
        '',
        ('- 版本：{0}（third_party/librime，BUILD_MERGED_PLUGINS=ON）' -f $script:LibrimeVersion),
        ('- 插件组件：{0}' -f ($script:LibrimeComponents -join ', ')),
        ('- 架构：{0}' -f (($built | ForEach-Object { $_.arch }) -join ' + ')),
        ('- boost：{0}' -f $script:BoostDir),
        ('- 工具集：{0} / {1}' -f $Generator, $PlatformToolset),
        ('- deps：{0}' -f (($script:DepsPins.Keys | ForEach-Object { $_ + '=' + $script:DepsPins[$_].Tag }) -join ', '))
    )
    Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value ($lines -join [Environment]::NewLine) -Encoding utf8
}

Write-Host ''
Write-Host ('[done] librime {0} 自建完成（含 rimesync-extra），Weasel 布局已就绪。' -f $script:LibrimeVersion)
