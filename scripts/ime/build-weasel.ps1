<#
.SYNOPSIS
  用 MSVC 从 third_party/weasel（vendored rime/weasel 0.17.4）源码构建小狼毫（Weasel）
  安装包，产物写入 packaging/windows/vendor/。

.DESCRIPTION
  构建链（与上游 rime/weasel 的 .github/workflows/release-ci.yml 对齐）：

    boost 1.84.0  --(b2, static runtime)-->  deps/boost_1_84_0/stage/lib
    librime       --(上游 get-rime.ps1) --->  lib/lib64/*.lib + output[rime.dll] + include/rime_*.h
    weasel.sln    --(msbuild Release) ------>  output\*.dll|*.ime|*.exe + output\Win32\*
    install.nsi   --(makensis 3.08) -------->  output\archives\weasel-0.17.4.0-installer.exe

  为什么不是 CMake：Weasel 0.17.4 上游在 Weasel 层没有 CMakeLists.txt，官方构建脚本是
  build.bat（msbuild weasel.sln）或 xbuild.bat（xmake）；CMake 只出现在 librime 子模块。
  本仓库 vendored 的 third_party/weasel 里 librime/ 与 plum/ 是空子模块（见 FORK.md），
  因此这里采用上游发行 CI 的同一套组合：librime 用 rime/librime 官方预编译 msvc 包
  （get-rime.ps1，x64 + x86 各一份），Weasel 本体用 msbuild 编译。

  依赖版本（上游 release-ci 的取值）：
    - Visual Studio 2022 + v143 工具集（env.vs2022.bat：PLATFORM_TOOLSET=v143 / BJAM_TOOLSET=msvc-14.3）
    - boost 1.84.0（b2 静态运行时；链接目录 $(BOOST_ROOT)\stage\lib）
    - librime 预编译包（默认 pin 1.13.1；可用 -LibrimeTag 覆盖）
    - NSIS 3.08（上游 install.nsi 就是用它）

.PARAMETER BoostVersion
  boost 源码版本，默认 1.84.0（上游 CI 取值）。

.PARAMETER BoostRoot
  已有 boost 的根目录（含 boost/ 与 stage/lib）。默认 third_party/weasel/deps/boost_<ver>。

.PARAMETER LibrimeTag
  rime/librime 的 release tag，默认 1.13.1（weasel 0.17.4 发布前最近的版本）。
  传 latest 表示跟随上游最新发布。

.PARAMETER SkipWin32
  只编译 x64。注意：上游 install.nsi 在编译期就会 File Win32\* 与 weasel.dll/weasel.ime
  （32 位 TSF 组件，64 位系统上的 32 位程序要用），跳过 Win32 会导致 makensis 失败。

.PARAMETER WithRimeData
  额外用 plum 拉取 Rime 方案数据（prelude：essay + luna_pinyin 等）写入 output\data。
  默认关闭：vendored 树里 plum/ 为空，且这一步要联网、依赖 bash；关闭时安装包只含
  上游自带的 weasel.yaml + preview 配色 + librime 的 opencc 数据，不含输入方案。

.PARAMETER Check
  只做前置检查与打印计划，不下载、不编译。用于本地/CI 的依赖自检。

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/build-weasel.ps1 -Check

.EXAMPLE
  pwsh -NoProfile -File scripts/ime/build-weasel.ps1
#>
[CmdletBinding()]
param(
    [string]$RepoRoot = '',
    [string]$WeaselRoot = '',
    [string]$OutputDir = '',
    [string]$BoostVersion = '1.84.0',
    [string]$BoostRoot = '',
    [switch]$RebuildBoost,
    [string]$LibrimeTag = '1.13.1',
    [string]$LibrimeBuildVariant = 'msvc',
    [switch]$SkipLibrimeDownload,
    [switch]$SkipWin32,
    [switch]$WithRimeData,
    [string]$RimeRecipes = '',
    [int]$MinInstallerMB = 8,
    [switch]$Check
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 由 Resolve-BuildContext / Get-VsInstallation / Ensure-Boost 填充（StrictMode 下必须先声明）
$script:VsInstallation = $null
$script:BoostSource = 'unknown'

# ---- 固定版本（改版本时必须同步 packaging/nsis-hooks.nsh 的 RIMESYNC_WEASEL_*）----
$WeaselVersion   = '0.17.4'
$WeaselBuild     = '0'
$ProductVersion  = "$WeaselVersion.$WeaselBuild"      # 0.17.4.0
$PlatformToolset = 'v143'
$BjamToolset     = 'msvc-14.3'
$InstallerName   = "weasel-$ProductVersion-installer.exe"

# ---------------------------------------------------------------------------
# 基础工具
# ---------------------------------------------------------------------------
function Write-Step {
    param([string]$Text)
    Write-Host ''
    Write-Host "==> $Text"
}

function Get-LastExitCode {
    try { return [int]$LASTEXITCODE } catch { return 0 }
}

function Get-HostExePath {
    try {
        $module = [System.Diagnostics.Process]::GetCurrentProcess().MainModule
        if ($module -and $module.FileName) { return $module.FileName }
    } catch { }
    return 'pwsh.exe'
}

function Get-TextEncodingName {
    if ($PSVersionTable.PSVersion.Major -ge 6) { return 'oem' }
    return 'Default'
}

function Assert-File {
    param([string]$Path, [string]$Hint = '')
    if (-not (Test-Path -LiteralPath $Path)) {
        $msg = "缺少文件：$Path"
        if ($Hint) { $msg = $msg + [Environment]::NewLine + $Hint }
        throw $msg
    }
}

function Invoke-Native {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @(),
        [string]$WorkDir = '',
        [switch]$AllowFailure
    )
    $old = $null
    if ($WorkDir) {
        $old = (Get-Location).Path
        Push-Location -LiteralPath $WorkDir
    }
    try {
        & $FilePath @Arguments 2>&1 | Out-Host
        $code = Get-LastExitCode
    } finally {
        if ($old) { Pop-Location }
    }
    if (($code -ne 0) -and (-not $AllowFailure)) {
        throw "命令失败（exit $code）：$FilePath $($Arguments -join ' ')"
    }
    return $code
}

# 在 VS 开发者环境（VsDevCmd/vcvars）里跑一条命令行。
# 用临时 .cmd 而不是把环境导回 PowerShell：b2 需要 cl.exe 所在的那个“视图”，
# x86 与 x64 各要一次，导出/覆盖 env 反而容易串味。
function Invoke-InDevShell {
    param(
        [Parameter(Mandatory = $true)][string]$Arch,        # amd64 / x86
        [Parameter(Mandatory = $true)][string]$CommandLine,
        [Parameter(Mandatory = $true)][string]$WorkDir,
        [switch]$AllowFailure
    )
    $vs = Get-VsInstallation
    $bat = Join-Path ([System.IO.Path]::GetTempPath()) ("rime-sync-weasel-{0}.cmd" -f ([guid]::NewGuid().ToString('N')))
    $lines = @(
        '@echo off',
        'setlocal',
        ('call "' + $vs.VsDevCmd + '" -arch=' + $Arch + ' -host_arch=amd64 -no_logo'),
        'if errorlevel 1 exit /b 9001',
        # boost 1.84 的 build.bat 靠这三个变量定位工具集（tools/build/src/engine/guess_toolset.bat、
        # config_toolset.bat）。VsDevCmd 只保证 VSINSTALLDIR/VCINSTALLDIR 正确，VS170COMNTOOLS 它不设；
        # 这里把 vswhere 已经解析好的安装路径显式喂进去，bootstrap/b2 就不必再自己猜。
        ('set "VSINSTALLDIR=' + $vs.InstallPath + '"'),
        ('set "VCINSTALLDIR=' + $vs.VcDir + '"'),
        ('set "VS170COMNTOOLS=' + $vs.CommonTools + '"'),
        ('cd /d "' + $WorkDir + '"'),
        $CommandLine,
        'set "RIMESYNC_RC=%ERRORLEVEL%"',
        'exit /b %RIMESYNC_RC%'
    )
    Set-Content -LiteralPath $bat -Value $lines -Encoding (Get-TextEncodingName)
    try {
        & cmd.exe /c $bat 2>&1 | Out-Host
        $code = Get-LastExitCode
    } finally {
        Remove-Item -LiteralPath $bat -Force -ErrorAction SilentlyContinue
    }
    if (($code -ne 0) -and (-not $AllowFailure)) {
        throw "开发者环境命令失败（exit $code）：$CommandLine"
    }
    return $code
}

function Get-VsInstallation {
    if ($null -ne $script:VsInstallation) { return $script:VsInstallation }

    $installPath = ''
    $pf86 = ${env:ProgramFiles(x86)}
    $vswhere = $null
    if ($pf86) { $vswhere = Join-Path $pf86 'Microsoft Visual Studio\Installer\vswhere.exe' }
    if ($vswhere -and (Test-Path -LiteralPath $vswhere)) {
        Write-Host "[vs] vswhere: $vswhere"
        $found = @(& $vswhere -latest -products '*' -requires 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -property installationPath 2>$null)
        foreach ($line in $found) {
            $candidate = ([string]$line).Trim()
            if ($candidate -and (Test-Path -LiteralPath (Join-Path $candidate 'Common7\Tools\VsDevCmd.bat'))) {
                $installPath = $candidate
                break
            }
        }
        if (-not $installPath) {
            Write-Warning '[vs] vswhere 没给出带 C++ 工作负载的安装，回退到目录扫描'
        }
    }

    if (-not $installPath) {
        $roots = @(${env:ProgramFiles}, ${env:ProgramFiles(x86)})
        foreach ($root in $roots) {
            if (-not $root) { continue }
            $vsRoot = Join-Path $root 'Microsoft Visual Studio'
            if (-not (Test-Path -LiteralPath $vsRoot)) { continue }
            $versions = Get-ChildItem -LiteralPath $vsRoot -Directory -ErrorAction SilentlyContinue
            foreach ($ver in $versions) {
                $editions = Get-ChildItem -LiteralPath $ver.FullName -Directory -ErrorAction SilentlyContinue
                foreach ($ed in $editions) {
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

    # 带尾部分隔符：boost 的 msvc.jam / config_toolset.bat 会直接做字符串拼接。
    $vcDir = (Join-Path $installPath 'VC') + '\'
    $script:VsInstallation = [pscustomobject]@{
        InstallPath = $installPath + '\'
        VcDir       = $vcDir
        VsDevCmd    = Join-Path $installPath 'Common7\Tools\VsDevCmd.bat'
        VcVarsAll   = Join-Path $vcDir 'Auxiliary\Build\vcvarsall.bat'
        CommonTools = (Join-Path $installPath 'Common7\Tools') + '\'
    }
    return $script:VsInstallation
}

function Get-SevenZip {
    param([string]$WeaselDir)
    $candidates = @(Join-Path $WeaselDir 'output\7z.exe')
    if (${env:ProgramFiles}) { $candidates += Join-Path ${env:ProgramFiles} '7-Zip\7z.exe' }
    if (${env:ProgramFiles(x86)}) { $candidates += Join-Path ${env:ProgramFiles(x86)} '7-Zip\7z.exe' }
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    $cmd = Get-Command '7z.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

function Get-Download {
    param([string]$Url, [string]$Destination, [int]$Attempts = 3)
    for ($i = 1; $i -le $Attempts; $i++) {
        try {
            Write-Host "[download] ($i/$Attempts) $Url"
            if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
            Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing -TimeoutSec 1800
            return
        } catch {
            if ($i -eq $Attempts) {
                throw "下载失败：$Url" + [Environment]::NewLine + $($_.Exception.Message)
            }
            Write-Warning "[download] 第 $i 次失败，重试：$($_.Exception.Message)"
            Start-Sleep -Seconds (5 * $i)
        }
    }
}

# ---------------------------------------------------------------------------
# 上下文
# ---------------------------------------------------------------------------
function Resolve-BuildContext {
    if (-not $RepoRoot) {
        $RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    }
    $script:RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
    if (-not $WeaselRoot) { $WeaselRoot = Join-Path $script:RepoRoot 'third_party\weasel' }
    $script:WeaselDir = (Resolve-Path -LiteralPath $WeaselRoot).Path
    if (-not $OutputDir) { $OutputDir = Join-Path $script:RepoRoot 'packaging\windows\vendor' }
    $script:VendorDir = [System.IO.Path]::GetFullPath($OutputDir)
    if (-not $BoostRoot) {
        $BoostRoot = Join-Path $script:WeaselDir ('deps\boost_' + ($BoostVersion -replace '\.', '_'))
    }
    $script:BoostDir = [System.IO.Path]::GetFullPath($BoostRoot)
}

function Test-Scaffolding {
    Assert-File (Join-Path $script:WeaselDir 'weasel.sln') 'third_party/weasel 未入库或为空时 CI 无法自编译；另见 packaging/windows/README.md。'
    Assert-File (Join-Path $script:WeaselDir 'output\install.nsi')
    Assert-File (Join-Path $script:WeaselDir 'get-rime.ps1')
    Assert-File (Join-Path $script:WeaselDir 'render.js')
    Assert-File (Join-Path $script:WeaselDir 'weasel.props.template')
    # install.nsi 在编译期 File 这些上游脚手架文件；它们被 third_party/weasel/.gitignore
    # 的 /output/*.exe、/output/*.txt、/output/*.bat 规则忽略，入库时必须 git add -f。
    $required = @(
        'output\7z.exe', 'output\7z.dll', 'output\curl.exe', 'output\WinSparkle.dll',
        'output\7-zip-license.txt', 'output\COPYING-curl.txt', 'output\curl-ca-bundle.crt',
        'output\rime-install-config.bat', 'output\start_service.bat', 'output\stop_service.bat',
        'output\Win32\WinSparkle.dll', 'resource\weasel.ico'
    )
    $missing = @()
    foreach ($rel in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $script:WeaselDir $rel))) { $missing += $rel }
    }
    if ($missing.Count -gt 0) {
        $sep = [Environment]::NewLine
        throw ("third_party/weasel 缺安装包脚手架文件：" + $sep + "  " + ($missing -join ($sep + '  ')) + $sep +
               "这些文件被 vendored 树的 .gitignore 忽略（output/*.exe、output/*.txt、output/*.bat）。" +
               "入库时请用 git add -f third_party/weasel。")
    }
}

# ---------------------------------------------------------------------------
# boost
# ---------------------------------------------------------------------------
function Test-BoostReady {
    param([string]$Bits)
    $stage = Join-Path $script:BoostDir 'stage\lib'
    $versionHpp = Join-Path $script:BoostDir 'boost\version.hpp'
    if (-not (Test-Path -LiteralPath $stage)) { return $false }
    if (-not (Test-Path -LiteralPath $versionHpp)) { return $false }
    $pattern = "libboost_filesystem-vc*-mt-s-x$Bits-*.lib"
    $hit = Get-ChildItem -LiteralPath $stage -Filter $pattern -ErrorAction SilentlyContinue
    return ($null -ne $hit)
}

# dev shell 里到底有没有 cl.exe —— 没有的话 bootstrap/b2/msbuild 会以各种难懂的方式失败，
# 所以先单独探一次，并把 VS 路径打出来。
function Test-DevShellCompiler {
    param([string]$WorkDir = '')
    if (-not $WorkDir) { $WorkDir = $script:WeaselDir }
    $code = Invoke-InDevShell -Arch 'amd64' -WorkDir $WorkDir -CommandLine 'where cl.exe' -AllowFailure
    return ($code -eq 0)
}

function Assert-DevShellCompiler {
    # boost 还没解压时用 weasel 源码目录做 cwd，免得 cd 失败被误判成“没有 cl.exe”
    $probeDir = $script:WeaselDir
    if (Test-Path -LiteralPath $script:BoostDir) { $probeDir = $script:BoostDir }
    if (Test-DevShellCompiler -WorkDir $probeDir) { return }
    $vs = Get-VsInstallation
    Write-Host '[vs] 诊断：'
    Write-Host ("  VSINSTALLDIR  = " + $vs.InstallPath)
    Write-Host ("  VCINSTALLDIR  = " + $vs.VcDir)
    Write-Host ("  VsDevCmd      = " + $vs.VsDevCmd)
    Invoke-InDevShell -Arch 'amd64' -WorkDir $probeDir -AllowFailure -CommandLine (
        'echo VSINSTALLDIR=%VSINSTALLDIR%& echo VCINSTALLDIR=%VCINSTALLDIR%& ' +
        'echo VCToolsVersion=%VCToolsVersion%& where cl.exe& where msbuild.exe') | Out-Null
    throw ('开发者环境（VsDevCmd）里拿不到 cl.exe。' + [Environment]::NewLine +
           '需要 runner 装 VS2022 + “使用 C++ 的桌面开发”工作负载；若已安装，检查 vswhere 是否指向了另一份安装。')
}

# boost 1.84 的 bootstrap.bat 没拿到显式 toolset 时会走 guess_toolset.bat：它只认 VS2017 的
# 硬编码路径（外加一层 vswhere 包装），在「只有 VS2022」的 runner 上会打印
# "### unable to detect your toolset installation" 并让 bootstrap 以 1 退出 —— v0.6.1 就死在这里
# （build.bat 自己都提示 "You can specify the toolset as the argument, i.e. .\build.bat msvc"）。
# 显式传 vc143 会直接进 config_toolset.bat 的 Config_VC143，跳过 guess。
function Initialize-B2 {
    $b2 = Join-Path $script:BoostDir 'b2.exe'
    if (Test-Path -LiteralPath $b2) {
        $code = Invoke-InDevShell -Arch 'amd64' -WorkDir $script:BoostDir -AllowFailure -CommandLine ('call "' + $b2 + '" --version')
        if ($code -eq 0) {
            $script:BoostSource = 'reused-b2'
            Write-Host "[boost] 复用已存在的 b2.exe：$b2"
            return
        }
        Write-Warning "[boost] 已存在的 b2.exe 跑不起来（exit $code），重新 bootstrap"
        Remove-Item -LiteralPath $b2 -Force -ErrorAction SilentlyContinue
    }

    Write-Step "bootstrap boost（$script:BoostDir，显式 toolset vc143）"
    $code = Invoke-InDevShell -Arch 'amd64' -WorkDir $script:BoostDir -CommandLine 'call bootstrap.bat vc143' -AllowFailure
    if (($code -eq 0) -and (Test-Path -LiteralPath $b2)) {
        $script:BoostSource = 'bootstrap-vc143'
    } else {
        # bootstrap.bat 本身只做三件事：cd tools\build\src\engine、build.bat <toolset>、把 b2.exe
        # 拷回根目录。直接复刻这一步，绕开 bootstrap.bat 自己的路径处理。
        Write-Warning "[boost] bootstrap.bat vc143 失败（exit $code），改走 tools\build\src\engine\build.bat vc143"
        $engineDir = Join-Path $script:BoostDir 'tools\build\src\engine'
        Assert-File (Join-Path $engineDir 'build.bat') 'boost 源码树不完整：缺 tools\build\src\engine\build.bat（7z 解压失败？）'
        $code = Invoke-InDevShell -Arch 'amd64' -WorkDir $engineDir -CommandLine 'call build.bat vc143' -AllowFailure
        $engineB2 = Join-Path $engineDir 'b2.exe'
        if (($code -ne 0) -or (-not (Test-Path -LiteralPath $engineB2))) {
            throw ('boost 的 b2 引擎构建失败（bootstrap 与 build.bat 都不是 0）：b2.exe 没生成。' + [Environment]::NewLine +
                   '排查：① runner 是否缺 VS2022 C++ 工作负载（上面 "where cl.exe" 的结果）；' + [Environment]::NewLine +
                   '      ② 在 VS2022 x64 Native Tools 提示符里 cd ' + $script:BoostDir + ' 后手动执行 bootstrap.bat vc143；' + [Environment]::NewLine +
                   '      ③ 若只能离线构建，把一份 /MT（-mt-s- 名字）的自建 boost 放到 -BoostRoot 指向的目录。')
        }
        Copy-Item -LiteralPath $engineB2 -Destination $b2 -Force
        Set-Content -LiteralPath (Join-Path $script:BoostDir 'project-config.jam') -Value @(
            '# generated by scripts/ime/build-weasel.ps1 (bootstrap fallback)',
            'import option ;',
            'using msvc ;',
            'option.set keep-going : false ;'
        ) -Encoding (Get-TextEncodingName)
        $script:BoostSource = 'engine-build-vc143'
    }

    $code = Invoke-InDevShell -Arch 'amd64' -WorkDir $script:BoostDir -AllowFailure -CommandLine ('call "' + $b2 + '" --version')
    if ($code -ne 0) { throw "b2.exe 生成了但无法执行（exit $code）：$b2" }
    Write-Host "[boost] b2 就绪：$b2（来源 $script:BoostSource）"
}

function Ensure-Boost {
    $needX64 = (-not (Test-BoostReady -Bits '64')) -or $RebuildBoost
    $needX86 = (-not $SkipWin32) -and (((-not (Test-BoostReady -Bits '32'))) -or $RebuildBoost)
    if ((-not $needX64) -and (-not $needX86)) {
        $script:BoostSource = 'cache'
        Write-Host "[boost] 已就绪（缓存命中，跳过下载/bootstrap/b2）：$script:BoostDir"
        Write-Host ("[boost] 来源=cache；x64={0}；x86={1}" -f (Test-BoostReady -Bits '64'), (Test-BoostReady -Bits '32'))
        return
    }

    # 真正要编译 boost 时先确认 dev shell 里真有 cl.exe：比等 120 MB 下载 + b2 报错快得多
    Assert-DevShellCompiler

    $boostTag = $BoostVersion -replace '\.', '_'
    $depsDir = Join-Path $script:WeaselDir 'deps'
    $archive = Join-Path $depsDir "boost_$boostTag.7z"
    $versionHpp = Join-Path $script:BoostDir 'boost\version.hpp'
    if (-not (Test-Path -LiteralPath $versionHpp)) {
        if (-not (Test-Path -LiteralPath $archive)) {
            New-Item -ItemType Directory -Force -Path $depsDir | Out-Null
            $url = "https://archives.boost.io/release/$BoostVersion/source/boost_$boostTag.7z"
            Get-Download -Url $url -Destination $archive
        }
        $sevenZip = Get-SevenZip -WeaselDir $script:WeaselDir
        if (-not $sevenZip) {
            throw '需要 7z 解压 boost 源码：third_party/weasel/output/7z.exe 不存在，且 PATH 里没有 7z。'
        }
        Write-Step "解压 boost $BoostVersion"
        Invoke-Native -FilePath $sevenZip -Arguments @('x', $archive, "-o$depsDir", '-y') | Out-Null
    }

    # 注意：不能用 Boost 官方的预编译二进制包（boost_1_84_0-msvc-14.3-*.exe）顶替。
    # 那套 .lib 是 /MD 运行时（名字里没有 -s），而 Weasel 全树是 /MT
    # （weasel.vcxproj: RuntimeLibrary=MultiThreaded），boost 的自动链接只会去找
    # libboost_*-mt-s-*；/MD 的 .lib 会被 MSVC 直接拒掉（LNK1104 / LNK2038）。
    Assert-File (Join-Path $script:BoostDir 'bootstrap.bat') 'BOOST_ROOT 必须指向 boost 源码树（含 bootstrap.bat）；自建的 /MT boost（-mt-s- 名字）请放到 <root>\boost 与 <root>\stage\lib。'
    if ($needX64 -or $needX86) {
        Initialize-B2
    }

    $b2 = Join-Path $script:BoostDir 'b2.exe'
    Assert-File $b2
    $jobs = 4
    if (${env:NUMBER_OF_PROCESSORS}) { $jobs = [int]${env:NUMBER_OF_PROCESSORS} }

    $common = @(
        '--with-filesystem', '--with-json', '--with-locale', '--with-regex',
        '--with-serialization', '--with-system', '--with-thread',
        'define=BOOST_USE_WINAPI_VERSION=0x0603',
        "toolset=$BjamToolset",
        'link=static', 'runtime-link=static', 'variant=release',
        'architecture=x86', 'stage', "-j$jobs"
    )

    if ($needX64) {
        Write-Step 'build boost x64（static runtime，等价于上游 build.bat boost）'
        $line = 'call "' + $b2 + '" ' + (($common + @('address-model=64')) -join ' ')
        Invoke-InDevShell -Arch 'amd64' -WorkDir $script:BoostDir -CommandLine $line | Out-Null
    }
    if ($needX86) {
        Write-Step 'build boost x86（static runtime）'
        $line = 'call "' + $b2 + '" ' + (($common + @('address-model=32')) -join ' ')
        Invoke-InDevShell -Arch 'x86' -WorkDir $script:BoostDir -CommandLine $line | Out-Null
    }

    if (-not (Test-BoostReady -Bits '64')) {
        throw 'boost x64 构建后仍找不到 stage\lib\libboost_filesystem-vc*-mt-s-x64-*.lib'
    }
    if ((-not $SkipWin32) -and (-not (Test-BoostReady -Bits '32'))) {
        throw 'boost x86 构建后仍找不到 stage\lib\libboost_filesystem-vc*-mt-s-x86-*.lib'
    }
    $x86Text = 'skipped'
    if (-not $SkipWin32) { $x86Text = [string](Test-BoostReady -Bits '32') }
    Write-Host ("[boost] 来源={0}；x64={1}；x86={2}；root={3}" -f $script:BoostSource, (Test-BoostReady -Bits '64'), $x86Text, $script:BoostDir)
}

# ---------------------------------------------------------------------------
# librime（预编译包，上游 get-rime.ps1）
# ---------------------------------------------------------------------------
function Test-LibrimeReady {
    $checks = @('lib64\rime.lib', 'output\rime.dll', 'include\rime_api.h')
    if (-not $SkipWin32) { $checks += @('lib\rime.lib', 'output\Win32\rime.dll') }
    foreach ($rel in $checks) {
        if (-not (Test-Path -LiteralPath (Join-Path $script:WeaselDir $rel))) { return $false }
    }
    return $true
}

function Ensure-Librime {
    if (Test-LibrimeReady) {
        Write-Host '[librime] 预编译包已就绪（lib / lib64 / output / include）'
        return
    }
    if ($SkipLibrimeDownload) {
        throw '缺少 librime 预编译包，且指定了 -SkipLibrimeDownload。请先跑一次 get-rime.ps1 -use dev。'
    }

    # get-rime.ps1 解压时要求 PATH 里有 7z；优先用 vendored 的 output\7z.exe
    $env:PATH = (Join-Path $script:WeaselDir 'output') + ';' + ${env:PATH}

    # 限流时用 GitHub token：get-rime.ps1 从 ~/.get-rime.conf.ps1 读取 $global:authorization
    if (${env:GITHUB_TOKEN}) {
        $conf = Join-Path ${env:USERPROFILE} '.get-rime.conf.ps1'
        if (-not (Test-Path -LiteralPath $conf)) {
            Set-Content -LiteralPath $conf -Value ('$global:authorization = "' + ${env:GITHUB_TOKEN} + '"') -Encoding utf8
            Write-Host '[librime] 已写入 ~/.get-rime.conf.ps1（GitHub token，仅本次 runner）'
        }
    }

    $getRimeArgs = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $script:WeaselDir 'get-rime.ps1'),
        '-use', 'dev'
    )
    if ($LibrimeTag) { $getRimeArgs += @('-tag', $LibrimeTag) }
    if ($LibrimeBuildVariant) { $getRimeArgs += @('-build_variant', $LibrimeBuildVariant) }

    Write-Step "拉取 librime 预编译包（tag=$LibrimeTag, variant=$LibrimeBuildVariant）"
    $code = Invoke-Native -FilePath (Get-HostExePath) -Arguments $getRimeArgs -WorkDir $script:WeaselDir -AllowFailure
    if (($code -ne 0) -or (-not (Test-LibrimeReady))) {
        throw ("get-rime.ps1 未能准备好 librime（exit $code）。" +
               '常见原因：api.github.com 限流/网络不可达、release 资产命名变化、7z 不可用。')
    }
}

# ---------------------------------------------------------------------------
# data / props
# ---------------------------------------------------------------------------
function Prepare-Data {
    $output = Join-Path $script:WeaselDir 'output'
    New-Item -ItemType Directory -Force -Path $output | Out-Null

    # install.nsi 无条件 File 这几个文件；上游 build.bat data 会从源码树复制
    Copy-Item -LiteralPath (Join-Path $script:WeaselDir 'LICENSE.txt') -Destination (Join-Path $output 'LICENSE.txt') -Force
    Copy-Item -LiteralPath (Join-Path $script:WeaselDir 'README.md')   -Destination (Join-Path $output 'README.txt') -Force

    $rimeInstallBat = Join-Path $output 'rime-install.bat'
    if (-not (Test-Path -LiteralPath $rimeInstallBat)) {
        $plumBat = Join-Path $script:WeaselDir 'plum\rime-install.bat'
        if (Test-Path -LiteralPath $plumBat) {
            Copy-Item -LiteralPath $plumBat -Destination $rimeInstallBat -Force
        } else {
            # plum 是空子模块：不伪造“能装方案”的脚本，只留一条明确提示（ASCII 以免代码页问题）
            Set-Content -LiteralPath $rimeInstallBat -Encoding ascii -Value @(
                '@echo off',
                'echo rime-install.bat: this build has no plum, cannot install Rime schemas online.',
                'echo Use packaging/windows/fetch-weasel.ps1 (upstream installer) instead,',
                'echo or rebuild with -WithRimeData (requires git clone rime/plum).',
                'exit /b 1'
            )
        }
    }

    $opencc = Join-Path $output 'data\opencc'
    if (-not (Test-Path -LiteralPath $opencc)) {
        New-Item -ItemType Directory -Force -Path $opencc | Out-Null
        Write-Warning "[data] $opencc 为空：install.nsi 的 File data\opencc\*.json 会给出 warning。"
    }

    if ($WithRimeData) {
        $plumDir = Join-Path $script:WeaselDir 'plum'
        if (-not (Test-Path -LiteralPath (Join-Path $plumDir 'rime-install'))) {
            Write-Step '克隆 rime/plum（-WithRimeData）'
            if (Test-Path -LiteralPath $plumDir) { Remove-Item -LiteralPath $plumDir -Recurse -Force }
            Invoke-Native -FilePath 'git' -Arguments @('clone', '--depth', '1', 'https://github.com/rime/plum.git', $plumDir) | Out-Null
        }
        $bash = Get-Command 'bash.exe' -ErrorAction SilentlyContinue
        if (-not $bash) { throw '-WithRimeData 需要 bash（Git for Windows）。' }
        Write-Step '构建 Rime 方案数据（plum/rime-install）'
        ${env:plum_dir} = $plumDir
        ${env:rime_dir} = Join-Path $output 'data'
        $recipeArgs = @('plum/rime-install')
        if ($RimeRecipes) { $recipeArgs += ($RimeRecipes -split '\s+') }
        Invoke-Native -FilePath $bash.Source -Arguments $recipeArgs -WorkDir $script:WeaselDir | Out-Null
    }
}

function Render-Props {
    ${env:BOOST_ROOT}       = $script:BoostDir
    ${env:PLATFORM_TOOLSET} = $PlatformToolset
    ${env:VERSION_MAJOR}    = '0'
    ${env:VERSION_MINOR}    = '17'
    ${env:VERSION_PATCH}    = '4'
    ${env:PRODUCT_VERSION}  = $ProductVersion
    ${env:FILE_VERSION}     = $ProductVersion
    Write-Step '生成 weasel.props'
    Invoke-Native -FilePath 'cscript.exe' -WorkDir $script:WeaselDir -Arguments @(
        '//nologo', 'render.js', 'weasel.props',
        'BOOST_ROOT', 'PLATFORM_TOOLSET',
        'VERSION_MAJOR', 'VERSION_MINOR', 'VERSION_PATCH',
        'PRODUCT_VERSION', 'FILE_VERSION'
    ) | Out-Null
    Assert-File (Join-Path $script:WeaselDir 'weasel.props')
}

# ---------------------------------------------------------------------------
# msbuild
# ---------------------------------------------------------------------------
function Invoke-WeaselBuild {
    param([string]$Platform)
    Write-Step "msbuild weasel.sln（Release | $Platform）"
    $line = 'msbuild.exe weasel.sln /t:Build /m /nologo /v:minimal /p:Configuration=Release /p:Platform=' + $Platform
    Invoke-InDevShell -Arch 'amd64' -WorkDir $script:WeaselDir -CommandLine $line | Out-Null
}

function Assert-BuildOutputs {
    $output = Join-Path $script:WeaselDir 'output'
    $required = @(
        'rime.dll', 'WinSparkle.dll',
        'weaselx64.dll', 'weaselx64.ime', 'weasel.ime',
        'WeaselServer.exe', 'WeaselDeployer.exe', 'WeaselSetup.exe'
    )
    if (-not $SkipWin32) {
        $required += @('weasel.dll', 'Win32\rime.dll', 'Win32\WinSparkle.dll',
                       'Win32\WeaselServer.exe', 'Win32\WeaselDeployer.exe')
    }
    $missing = @()
    foreach ($rel in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $output $rel))) { $missing += $rel }
    }
    if ($missing.Count -gt 0) {
        $sep = [Environment]::NewLine
        throw ("msbuild 结束但产物缺失：" + $sep + "  " + ($missing -join ($sep + '  ')))
    }
    Write-Host '[weasel] 产物齐全（x64 + Win32）'
}

# ---------------------------------------------------------------------------
# NSIS
# ---------------------------------------------------------------------------
function Find-Makensis {
    $cmd = Get-Command 'makensis.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @()
    if (${env:LOCALAPPDATA}) { $candidates += Join-Path ${env:LOCALAPPDATA} 'tauri\NSIS\makensis.exe' }
    if (${env:LOCALAPPDATA}) { $candidates += Join-Path ${env:LOCALAPPDATA} 'Programs\NSIS\makensis.exe' }
    if (${env:ProgramFiles}) { $candidates += Join-Path ${env:ProgramFiles} 'NSIS\makensis.exe' }
    if (${env:ProgramFiles(x86)}) { $candidates += Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    return $null
}

function Ensure-Nsis {
    $found = Find-Makensis
    if ($found) { return $found }

    Write-Step '安装 NSIS 3.08'
    $choco = Get-Command 'choco.exe' -ErrorAction SilentlyContinue
    if ($choco) {
        & $choco.Source install nsis --version=3.08 -y --no-progress 2>&1 | Out-Host
        $null = Get-LastExitCode
    }
    $found = Find-Makensis
    if ($found) { return $found }

    $setup = Join-Path ([System.IO.Path]::GetTempPath()) 'nsis-3.08-setup.exe'
    Get-Download -Url 'https://downloads.sourceforge.net/project/nsis/NSIS%203/3.08/nsis-3.08-setup.exe' -Destination $setup
    $proc = Start-Process -FilePath $setup -ArgumentList '/S' -Wait -PassThru
    if ($proc.ExitCode -ne 0) { throw "NSIS 静默安装失败（exit $($proc.ExitCode)）" }
    $found = Find-Makensis
    if (-not $found) { throw 'NSIS 安装后仍找不到 makensis.exe（把 NSIS\Bin 加进 PATH 后重试）。' }
    return $found
}

function New-InstallerScript {
    param([string]$OutputDir)
    # 上游 install.nsi 用 File "data\opencc\*.json" / "*.ocd*" 强制要求 OpenCC 数据；
    # 但 librime 的 Windows 预编译包不含 share/opencc（那是 librime 构建时生成的），
    # vendored 树里 librime/ 又是空子模块。NSIS 对“通配符没有匹配文件”是**硬错误**，
    # 所以这里在构建期生成一份把这两行改成 /nonfatal 的脚本（不改 vendored 源码）。
    $src = Join-Path $OutputDir 'install.nsi'
    $dst = Join-Path $OutputDir 'install.rime-sync.nsi'
    $text = Get-Content -LiteralPath $src -Raw -Encoding UTF8
    foreach ($glob in @('data\opencc\*.json', 'data\opencc\*.ocd*')) {
        $needle = 'File "' + $glob + '"'
        $replacement = 'File /nonfatal "' + $glob + '"'
        if ($text.Contains($needle)) {
            $text = $text.Replace($needle, $replacement)
        } else {
            Write-Warning "[nsis] install.nsi 里没找到预期的行：$needle（上游脚本可能已改版）"
        }
    }
    # install.nsi 带 UTF-8 BOM（里面有中文 LangString），必须原样保留
    $utf8Bom = New-Object System.Text.UTF8Encoding($true)
    [System.IO.File]::WriteAllText($dst, $text, $utf8Bom)
    Write-Host "[nsis] 已生成 $dst（OpenCC 数据缺失时不再中断打包）"
    return $dst
}

function Invoke-Makensis {
    param([string]$Makensis)
    if ($SkipWin32) {
        throw '-SkipWin32 只适合调试编译：上游 install.nsi 在编译期就 File Win32\* 与 32 位 TSF 组件，缺了无法产出安装包。'
    }
    $output = Join-Path $script:WeaselDir 'output'
    Write-Step 'makensis output/install.rime-sync.nsi'
    $nsisArgs = @(
        '/V2',
        "/DWEASEL_VERSION=$WeaselVersion",
        "/DWEASEL_BUILD=$WeaselBuild",
        "/DPRODUCT_VERSION=$ProductVersion",
        (Split-Path -Leaf (New-InstallerScript -OutputDir $output))
    )
    Invoke-Native -FilePath $Makensis -Arguments $nsisArgs -WorkDir $output | Out-Null
    $installer = Join-Path $output ("archives\$InstallerName")
    Assert-File $installer 'install.nsi 的 OutFile 是 archives\weasel-<PRODUCT_VERSION>-installer.exe'
    return $installer
}

function Publish-Artifact {
    param([string]$Installer)
    New-Item -ItemType Directory -Force -Path $script:VendorDir | Out-Null
    $dest = Join-Path $script:VendorDir $InstallerName
    Copy-Item -LiteralPath $Installer -Destination $dest -Force
    $item = Get-Item -LiteralPath $dest
    $hash = (Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash.ToLowerInvariant()
    $mib = [math]::Round($item.Length / 1MB, 1)
    if ($item.Length -lt ($MinInstallerMB * 1MB)) {
        throw "自编译的 Weasel 安装包只有 $mib MiB（阈值 $MinInstallerMB MiB），疑似构建不完整。"
    }
    Write-Host ''
    Write-Host "[ok] $dest"
    Write-Host "[ok] $($item.Length) 字节（$mib MiB）"
    Write-Host "[ok] sha256=$hash"

    if (${env:GITHUB_STEP_SUMMARY}) {
        $win32 = '已构建'
        if ($SkipWin32) { $win32 = '跳过（makensis 会缺 32 位组件）' }
        $lines = @(
            '### Weasel 自编译产物',
            '',
            '- 来源：third_party/weasel（rime/weasel 0.17.4，commit 9cc96e20）',
            "- 产物：$InstallerName（$($item.Length) 字节，$mib MiB）",
            "- SHA-256：$hash",
            "- boost：$BoostVersion；librime：$LibrimeTag；工具集：$PlatformToolset",
            "- Win32：$win32"
        )
        Add-Content -LiteralPath ${env:GITHUB_STEP_SUMMARY} -Value ($lines -join [Environment]::NewLine) -Encoding utf8
    }
}

# ---------------------------------------------------------------------------
# 前置检查（-Check）
# ---------------------------------------------------------------------------
function Invoke-Preflight {
    $sevenZip = Get-SevenZip -WeaselDir $script:WeaselDir
    $makensis = Find-Makensis
    $vsText = 'missing'
    $vsPathText = 'missing'
    try {
        $vs = Get-VsInstallation
        $vsText = $vs.VsDevCmd
        $vsPathText = $vs.InstallPath
    } catch { $vsText = 'missing: ' + $_.Exception.Message }
    $clText = 'unknown'
    if ($vsText -notlike 'missing*') {
        try {
            if (Test-DevShellCompiler -WorkDir $script:WeaselDir) { $clText = 'ok' }
            else { $clText = 'missing（VS 缺 C++ 工作负载？）' }
        } catch { $clText = 'error: ' + $_.Exception.Message }
    }
    $b2Text = '缺失（会 bootstrap vc143）'
    if (Test-Path -LiteralPath (Join-Path $script:BoostDir 'b2.exe')) { $b2Text = '已存在（复用）' }
    $sevenZipText = '缺失'
    if ($sevenZip) { $sevenZipText = $sevenZip }
    $makensisText = '缺失（会自动安装 3.08）'
    if ($makensis) { $makensisText = $makensis }
    $rows = @(
        [pscustomobject]@{ Name = 'repo root';        Value = $script:RepoRoot },
        [pscustomobject]@{ Name = 'weasel source';    Value = $script:WeaselDir },
        [pscustomobject]@{ Name = 'vendor output';    Value = $script:VendorDir },
        [pscustomobject]@{ Name = 'boost root';       Value = $script:BoostDir },
        [pscustomobject]@{ Name = 'boost x64 ready';  Value = (Test-BoostReady -Bits '64') },
        [pscustomobject]@{ Name = 'boost x86 ready';  Value = (Test-BoostReady -Bits '32') },
        [pscustomobject]@{ Name = 'librime ready';    Value = (Test-LibrimeReady) },
        [pscustomobject]@{ Name = '7z';               Value = $sevenZipText },
        [pscustomobject]@{ Name = 'VS 安装';          Value = $vsPathText },
        [pscustomobject]@{ Name = 'VsDevCmd';         Value = $vsText },
        [pscustomobject]@{ Name = 'cl.exe (devshell)'; Value = $clText },
        [pscustomobject]@{ Name = 'b2.exe';           Value = $b2Text },
        [pscustomobject]@{ Name = 'makensis';         Value = $makensisText }
    )
    $rows | Format-Table -AutoSize | Out-String | Write-Host
    $platforms = 'Release x64'
    if (-not $SkipWin32) { $platforms = 'Release x64 + Win32' }
    Write-Host "计划：boost $BoostVersion -> librime $LibrimeTag -> msbuild $platforms -> makensis -> $InstallerName"
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------
Write-Host 'rime-sync · 从 vendored 源码构建 Weasel'
Write-Host "weasel=$WeaselVersion ($ProductVersion)  toolset=$PlatformToolset  boost=$BoostVersion"

Resolve-BuildContext
Test-Scaffolding

if ($Check) {
    Invoke-Preflight
    Write-Host '[check] 前置检查通过（未下载、未编译）'
    exit 0
}

Ensure-Boost
Ensure-Librime
Prepare-Data
Render-Props

Invoke-WeaselBuild -Platform 'x64'
if (-not $SkipWin32) { Invoke-WeaselBuild -Platform 'Win32' }
Assert-BuildOutputs

$makensis = Ensure-Nsis
$installer = Invoke-Makensis -Makensis $makensis
Publish-Artifact -Installer $installer

Write-Host ''
Write-Host '[done] Weasel 安装包已就绪，可执行 pnpm tauri build --bundles nsis。'
