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
    e) 只读判据（check-ime，NSIS 自检用的就是它）与 ensure 结论一致；
    f) 227 形状的旧 custom.yaml（vselect、无 numpad）：ensure 把它修好，
       常驻小键盘判定报出 artifact-stale（结论 partial），check-ime 只读地
       报出同一个缺口（非零退出、用户目录一个字节都不变）。

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

# 随包产物的真实形态：engine/processors 里带 numpad（小键盘归一化处理器）。
# 少了 numpad 的假产物会被「常驻小键盘判定」当成「引擎还没重编译」→ 误报成
# partial，把 a~c 里「result 期望 ok/noop」的断言带崩。
# 正确做法是让假数据更真（跟真实随包产物一致），不是放宽断言。
$marked = @"
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
engine:
  processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - numpad
    - selector
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

# 与 Invoke-ImeCli 流程完全相同，唯一差别：rime 目录可指定。
# case f 必须用它自己的临时 rime 目录（a~e 那份 $rime 的状态不许被影响）。
function Invoke-ImeCliIn {
    param(
        [Parameter(Mandatory = $true)][string]$RimeDir,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$ReportPath
    )
    Remove-Item -LiteralPath $ReportPath -Force -ErrorAction SilentlyContinue
    $full = @('--rime-dir', $RimeDir) + $Arguments + @('--json', '--report', $ReportPath)
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

# --- f) 227 形状：旧 custom.yaml → 修好 + 常驻小键盘判定 ------------------------
# 单独用自己的临时目录：绝不污染上面那份 $rime（a~e 的状态不许被影响）。
$fRoot = Join-Path $root 'case-f'
$fPayload = Join-Path $fRoot 'payload'
$fRime = Join-Path $fRoot 'rime'
$fData = Join-Path $fRoot 'data'
foreach ($dir in @((Join-Path $fPayload 'build'), $fRime, $fData)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

# 227 形状的编译产物：有 rime-sync 定制，但**没有** numpad（= 引擎还没重编译）。
$staleArtifact = @"
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
engine:
  processors:
    - ascii_composer
    - rimesync_vselect
    - selector
"@

# 227 形状的用户 custom.yaml：带 rime-sync 标记、有 rimesync_vselect、**没有 numpad**。
$oldCustom = @"
# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）
# schema: wubi86
patch:
  engine/processors:
    - ascii_composer
    - recognizer
    - rimesync_punct
    - rimesync_vselect
    - key_binder
    - speller
    - punctuator
    - selector
  engine/translators:
    - rimesync_vnum
    - punct_translator
"@

foreach ($s in $schemas) {
    Set-Content -LiteralPath (Join-Path $fPayload "build\$s.schema.yaml") -Value $staleArtifact -NoNewline
    Set-Content -LiteralPath (Join-Path $fPayload "build\$s.prism.bin") -Value 'prism' -NoNewline
    Set-Content -LiteralPath (Join-Path $fData "$s.schema.yaml") -Value "schema:`n  schema_id: $s`n" -NoNewline
}
$fCustom = Join-Path $fRime 'wubi86.custom.yaml'
Set-Content -LiteralPath $fCustom -Value $oldCustom -NoNewline

# bootstrap 模式：铺产物但不跑部署（部署一跑就会重编译，227 形状就没了）。
$fEnsure = Invoke-ImeCliIn -RimeDir $fRime -Arguments @('ensure-ime', '--mode', 'bootstrap', '--no-data-patch', '--payload', $fPayload) -ReportPath (Join-Path $root 'case-f-ensure.json')

# 1) 文件被修好了：numpad 紧跟 rimesync_vselect，而且只有一份。
$fLines = Get-Content -LiteralPath $fCustom
$vselectMatch = $fLines | Select-String -SimpleMatch '- rimesync_vselect' | Select-Object -First 1
$numpadMatch = $fLines | Select-String -SimpleMatch '- numpad' | Select-Object -First 1
if ($null -eq $vselectMatch -or $null -eq $numpadMatch) {
    throw "custom.yaml 没修好（缺 rimesync_vselect 或 numpad）：$($fLines -join '|')"
}
$vselectLine = $vselectMatch.LineNumber
$numpadLine = $numpadMatch.LineNumber
if ($numpadLine -ne ($vselectLine + 1)) {
    throw "numpad 没紧跟 rimesync_vselect（vselect=$vselectLine numpad=$numpadLine）：$($fLines -join '|')"
}
$numpadCount = @($fLines | Select-String -SimpleMatch '- numpad').Count
if ($numpadCount -ne 1) {
    throw "numpad 必须只有一份（实际 $numpadCount）：$($fLines -join '|')"
}

# 2) actions 里有 custom-repair（且 changed=true：改了才算数）。
$repair = @($fEnsure.Json.actions | Where-Object { $_.kind -eq 'custom-repair' })
if ($repair.Count -lt 1) {
    throw "ensure-ime 的 actions 里没有 custom-repair：$($fEnsure.Json.actions | ConvertTo-Json -Compress)"
}
if (-not $repair[0].changed) {
    throw 'custom-repair 必须 changed=true（改了文件才算修过）'
}

# 3) 报告里有 numpad 段，且结论是 partial + detail 指名 artifact-stale。
if (-not ($fEnsure.Json.PSObject.Properties.Name -contains 'numpad')) {
    throw 'ensure-ime 的报告里没有 numpad 段'
}
Assert-Equal $fEnsure.Json.numpad.ok $false 'ensure-ime #f numpad.ok'
if (@($fEnsure.Json.numpad.missing) -notcontains 'wubi86=artifact-stale') {
    throw "numpad.missing 不含 wubi86=artifact-stale：$($fEnsure.Json.numpad.missing -join ',')"
}
Assert-Equal $fEnsure.Json.result 'partial' 'ensure-ime #f result'
if ([string]$fEnsure.Json.detail -notmatch 'artifact-stale') {
    throw "ensure-ime #f 的 detail 没点名 artifact-stale：$($fEnsure.Json.detail)"
}

# 4) check-ime：只读地报出同一个缺口，非零退出，用户目录一个字节都不变。
$beforeBytes = [System.IO.File]::ReadAllBytes($fCustom)
$beforeTime = (Get-Item -LiteralPath $fCustom).LastWriteTimeUtc
$fCheck = Invoke-ImeCliIn -RimeDir $fRime -Arguments @('check-ime', '--payload', $fPayload) -ReportPath (Join-Path $root 'case-f-check.json')
if ($fCheck.Code -eq 0) {
    throw 'check-ime 报出小键盘缺口时必须非零退出'
}
Assert-Equal $fCheck.Json.result 'partial' 'check-ime #f result'
Assert-Equal $fCheck.Json.numpad.ok $false 'check-ime #f numpad.ok'
if (@($fCheck.Json.numpad.missing) -notcontains 'wubi86=artifact-stale') {
    throw "check-ime 的 numpad.missing 不含 wubi86=artifact-stale：$($fCheck.Json.numpad.missing -join ',')"
}
if ((@($fCheck.Json.numpad.missing) -join ',') -ne (@($fEnsure.Json.numpad.missing) -join ',')) {
    throw "check-ime 与 ensure-ime 的结论必须一致：$($fCheck.Json.numpad.missing -join ',') vs $($fEnsure.Json.numpad.missing -join ',')"
}
$afterBytes = [System.IO.File]::ReadAllBytes($fCustom)
$afterTime = (Get-Item -LiteralPath $fCustom).LastWriteTimeUtc
if ($beforeBytes.Length -ne $afterBytes.Length) {
    throw "check-ime 改了用户目录的文件长度：$($beforeBytes.Length) -> $($afterBytes.Length)"
}
for ($i = 0; $i -lt $beforeBytes.Length; $i++) {
    if ($beforeBytes[$i] -ne $afterBytes[$i]) {
        throw "check-ime 改了用户目录的文件字节（偏移 $i）"
    }
}
if ($beforeTime -ne $afterTime) {
    throw "check-ime 改了用户目录的文件 mtime：$beforeTime -> $afterTime"
}

Write-Host 'windows e2e ok：ensure-ime 判据 / 修补 / 就绪退出码 / 失败退出码 / check-ime 一致性 / 227 形状的小键盘缺口'

# GitHub 的 pwsh step 是把脚本 dot-source 进会话再跑，结束时按 $LASTEXITCODE 退出。
# 脚本里调过 CLI（没就绪时退出码是 1，这是**预期**），不显式 exit 0 会被它带成
# 「步骤失败」—— 断言明明全过。失败时上面的 throw 会让脚本非零退出，不用管。
exit 0
