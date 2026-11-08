<#
.SYNOPSIS
  Windows e2e：ensure-ime / check-ime 的判据、修补与退出码。

.DESCRIPTION
  不安装任何输入法，也不需要 librime：只伪造 user dir + payload + data，验证
    a) 没有输入法（自动安装被显式关掉）：产物照铺，但**不能报成功**——
       ok=false、退出码非 0、detail 说清楚是「自动安装已关闭」；
    b) 删掉一个 .bin 再跑：repaired 含该方案（补回来了），产物仍可用；
    c) 用户手上有输入法（假 Weasel：注册表 + WeaselServer.exe + rime.dll）：
       ok=true、退出码 0 —— 退出码 0 的含义是「输入法现在就绪」；
    d) payload 缺文件：result=Failed、退出码非 0、detail 非空；
    e) 只读判据（check-ime，NSIS 自检用的就是它）与 ensure 结论一致。

  假数据必须覆盖全部三个目标方案（luna_pinyin_simp / wubi_pinyin / wubi86）：
  只造一个方案时 targets 会退化成 1，config 门禁又因 data 里没有 wubi86 而跳过
  default.custom.yaml，结论必然是 partial —— 那样测到的不是「产物铺好了」。

  所有断言都从 JSON 报告里取字段（ConvertFrom-Json），不靠 stdout 文本。
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$cli = Join-Path $env:GITHUB_WORKSPACE 'target\debug\rime-sync-cli.exe'
if (-not (Test-Path -LiteralPath $cli)) {
    throw "找不到 rime-sync-cli：$cli（先跑 cargo build -p rime-sync-cli）"
}

$root = Join-Path $env:RUNNER_TEMP ('rime-sync-ime-e2e-' + [guid]::NewGuid().ToString('N'))
$payload = Join-Path $root 'payload'
$rime = Join-Path $root 'rime'
$data = Join-Path $root 'data'
$state = Join-Path $root 'state'
foreach ($dir in @((Join-Path $payload 'build'), (Join-Path $payload 'config'), $rime, $data, $state)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

$marked = @"
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
"@
$plainConfig = @"
patch:
  schema_list: []
"@
$weaselConfig = @"
patch:
  style:
    horizontal: true
"@

# payload：三个目标方案各一份 schema.yaml + bin；config 里放两份模板。
# data：同名 schema.yaml —— config 门禁看的就是 data 里有没有这些方案。
$schemas = @('luna_pinyin_simp', 'wubi_pinyin', 'wubi86')
foreach ($s in $schemas) {
    Set-Content -LiteralPath (Join-Path $payload "build\$s.schema.yaml") -Value $marked -NoNewline
    Set-Content -LiteralPath (Join-Path $payload "build\$s.prism.bin") -Value 'prism' -NoNewline
    Set-Content -LiteralPath (Join-Path $data "$s.schema.yaml") -Value "schema:`n  schema_id: $s`n" -NoNewline
}
Set-Content -LiteralPath (Join-Path $payload 'config\default.custom.yaml') -Value $plainConfig -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'config\weasel.custom.yaml') -Value $weaselConfig -NoNewline

# 只碰临时目录；绝不装输入法、绝不 taskkill 用户输入法、绝不清理方案。
# 自动安装关掉：CI runner 上没有随包安装器，也不该真去装输入法。
$env:RIMESYNC_PREBUILT_DIR = $payload
$env:RIMESYNC_WEASEL_DATA = $data
$env:RIMESYNC_INSTALL_IME = '0'
$env:RIMESYNC_PRUNE_SCHEMAS = '0'
$env:RIMESYNC_DEPLOY_NO_RESTART = '1'

function Invoke-ImeCli {
    param(
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$ReportPath
    )
    Remove-Item -LiteralPath $ReportPath -Force -ErrorAction SilentlyContinue
    $full = @('--rime-dir', $rime) + $Arguments + @('--json', '--report', $ReportPath)
    & $cli @full | Out-Null
    $code = $LASTEXITCODE
    if (-not (Test-Path -LiteralPath $ReportPath)) {
        throw "CLI 没有写 JSON 报告（exit=$code）：$ReportPath"
    }
    $json = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json
    [pscustomobject]@{ Code = $code; Json = $json; Path = $ReportPath }
}

function Assert-Equal {
    param($Actual, $Expected, [string]$What)
    if ($Actual -ne $Expected) {
        throw "$What 期望 $Expected，实际 $Actual"
    }
}

# ensure-ime 的结论断言。口径与 core 的 ImeReport::ready 一致（GUI 同一处）：
#   产物侧：result ∈ ok/noop、build_ok=true、config 没被门禁跳过；
#   就绪侧：ok 字段 ⟺ 退出码 0（退出码 0 = 「输入法现在就绪」）。
function Assert-Ensure {
    param($Json, [int]$Code, [string]$What, [bool]$ExpectReady)
    Assert-Equal $Json.build_ok $true "$What build_ok"
    if ($null -ne $Json.config_skipped -and @($Json.config_skipped).Count -ne 0) {
        throw "$What config 被门禁跳过（data 里的方案不齐）：$($Json.config_skipped -join ',')"
    }
    if ($Json.result -notin @('ok', 'noop')) {
        throw "$What result 期望 ok/noop，实际 $($Json.result)（detail=$($Json.detail)）"
    }
    Assert-Equal $Json.ok $ExpectReady "$What ok（就绪口径）"
    if ($ExpectReady) {
        Assert-Equal $Code 0 "$What 退出码"
        return
    }
    if ($Code -eq 0) {
        throw "$What 退出码是 0（没就绪必须非零）"
    }
    if ([string]$Json.detail -notmatch '自动安装已关闭') {
        throw "$What detail 没说明为什么没就绪：$($Json.detail)"
    }
}

Write-Host "e2e root = $root"

# --- a) 没有输入法：产物照铺，但必须说「没就绪」 --------------------------------
$first = Invoke-ImeCli -Arguments @('ensure-ime', '--mode', 'install') -ReportPath (Join-Path $root 'ensure1.json')
Assert-Ensure $first.Json $first.Code 'ensure-ime #1（无输入法）' $false
Assert-Equal $first.Json.weasel_install 'disabled' 'weasel_install（自动安装关闭）'
Assert-Equal $first.Json.weasel_present $false 'weasel_present'
$schemaPath = Join-Path $rime 'build\luna_pinyin_simp.schema.yaml'
if (-not (Test-Path -LiteralPath $schemaPath)) {
    throw "build 里没有 schema.yaml：$schemaPath"
}
$schemaText = Get-Content -LiteralPath $schemaPath -Raw
if ($schemaText -notmatch 'rimesync_vnum') {
    throw "schema.yaml 不含 rimesync_vnum：$schemaPath"
}
if (@($first.Json.build_repaired) -notcontains 'luna_pinyin_simp') {
    throw "build_repaired 不含 luna_pinyin_simp：$($first.Json.build_repaired -join ',')"
}
if (@($first.Json.actions).Count -lt 1) {
    throw 'actions 为空（不能既说铺好了又什么都没做）'
}

# --- b) 删掉一个 .bin → 再跑 → 补回来 -------------------------------------------
Remove-Item -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin') -Force
$second = Invoke-ImeCli -Arguments @('ensure-ime', '--mode', 'install') -ReportPath (Join-Path $root 'ensure2.json')
Assert-Ensure $second.Json $second.Code 'ensure-ime #2（修补）' $false
if (@($second.Json.build_repaired) -notcontains 'luna_pinyin_simp') {
    throw "build_repaired 不含 luna_pinyin_simp：$($second.Json.build_repaired -join ',')"
}
if (-not (Test-Path -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin'))) {
    throw '删掉的 .bin 没有被补回来'
}

# --- c) 用户手上有输入法（假 Weasel）→ 就绪 → 退出码 0 -------------------------
# 判据的第一来源是注册表（WeaselRoot/InstallDir），CI runner 上本来没有；测完删掉。
$fakeWeasel = Join-Path $root 'weasel'
New-Item -ItemType Directory -Force -Path $fakeWeasel | Out-Null
Set-Content -LiteralPath (Join-Path $fakeWeasel 'WeaselServer.exe') -Value 'MZ fake server' -NoNewline
Set-Content -LiteralPath (Join-Path $fakeWeasel 'rime.dll') -Value 'MZ rimesync_vnum rimesync_punct' -NoNewline
$regKey = 'HKLM\Software\WOW6432Node\Rime\Weasel'
reg add $regKey /v InstallDir /t REG_SZ /d $fakeWeasel /f | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "写注册表失败（CI runner 应当是管理员）"
}
try {
    $third = Invoke-ImeCli -Arguments @('ensure-ime', '--mode', 'install') -ReportPath (Join-Path $root 'ensure3.json')
    Assert-Ensure $third.Json $third.Code 'ensure-ime #3（有输入法）' $true
    Assert-Equal $third.Json.weasel_present $true 'weasel_present'
    Assert-Equal $third.Json.weasel_install 'already-present' 'weasel_install'
} finally {
    reg delete $regKey /f | Out-Null
}

# --- d) payload 缺文件 → failed + 非零退出码 -----------------------------------
Remove-Item -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.prism.bin') -Force
Remove-Item -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin') -Force -ErrorAction SilentlyContinue
$fourth = Invoke-ImeCli -Arguments @('ensure-ime', '--mode', 'install') -ReportPath (Join-Path $root 'ensure4.json')
Assert-Equal $fourth.Json.result 'failed' 'ensure-ime #4 result'
Assert-Equal $fourth.Json.build_ok $false 'ensure-ime #4 build_ok'
if ($fourth.Code -eq 0) {
    throw 'ensure-ime #4 退出码是 0（payload 缺文件必须非零）'
}
if ([string]::IsNullOrWhiteSpace([string]$fourth.Json.detail)) {
    throw 'ensure-ime #4 detail 为空'
}

# --- e) 只读判据（NSIS 自检用的命令）必须给出同一个结论 ------------------------
$check = Invoke-ImeCli -Arguments @('check-ime') -ReportPath (Join-Path $root 'check.json')
if ($check.Code -eq 0) {
    throw 'check-ime 在 build 不可用时退出码是 0'
}
Assert-Equal $check.Json.result 'failed' 'check-ime result'
Assert-Equal $check.Json.build_ok $false 'check-ime build_ok'
if (@($check.Json.missing) -notcontains 'luna_pinyin_simp') {
    throw "check-ime missing 不含 luna_pinyin_simp：$($check.Json.missing -join ',')"
}

Write-Host 'windows e2e ok：ensure-ime 判据 / 修补 / 就绪退出码 / 失败退出码 / check-ime 一致性'
