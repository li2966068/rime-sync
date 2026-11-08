<#
.SYNOPSIS
  Windows e2e：首次启动自举（ensure-ime --mode install）的报告一致性。

.DESCRIPTION
  不装输入法、不需要 librime：只伪造 user dir + payload + data，验证首次启动自举
    1) 退出码与 ok 字段同口径（ok ⟺ 退出码 0，退出码 0 = 「输入法现在就绪」）；
    2) 产物铺进 user dir：build 里有带 rimesync_vnum 的 schema.yaml；
    3) 自动安装被显式关掉（RIMESYNC_INSTALL_IME=0）时，报告写明
       weasel_install=disabled，并且**没有**把这次自举说成「输入法已就绪」——
       ok=false、退出码非 0、detail 说清楚是「自动安装已关闭」。

  假数据必须覆盖全部三个目标方案（luna_pinyin_simp / wubi_pinyin / wubi86）：
  只造一个方案时 targets 会退化成 1，config 门禁又因 data 里没有 wubi86 而跳过
  default.custom.yaml，结论必然是 partial —— 那样测到的不是「产物铺好了」。

  判据口径与 core 的 ImeReport::ready 一致（GUI 与 CLI 共用同一处）。
#>
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$cli = Join-Path $env:GITHUB_WORKSPACE 'target\debug\rime-sync-cli.exe'
if (-not (Test-Path -LiteralPath $cli)) {
    throw "找不到 rime-sync-cli：$cli（先跑 cargo build -p rime-sync-cli）"
}

$root = Join-Path $env:RUNNER_TEMP ('rime-sync-first-start-' + [guid]::NewGuid().ToString('N'))
$payload = Join-Path $root 'payload'
$rime = Join-Path $root 'rime'
$data = Join-Path $root 'data'
$report = Join-Path $root 'first-start.json'
foreach ($dir in @((Join-Path $payload 'build'), (Join-Path $payload 'config'), $rime, $data)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

# 随包预编译产物（最小但完整的形态：三个目标方案 + 插件标记）。
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
$schemas = @('luna_pinyin_simp', 'wubi_pinyin', 'wubi86')
foreach ($s in $schemas) {
    Set-Content -LiteralPath (Join-Path $payload "build\$s.schema.yaml") -Value $marked -NoNewline
    Set-Content -LiteralPath (Join-Path $payload "build\$s.prism.bin") -Value 'prism' -NoNewline
    Set-Content -LiteralPath (Join-Path $data "$s.schema.yaml") -Value "schema:`n  schema_id: $s`n" -NoNewline
}
Set-Content -LiteralPath (Join-Path $payload 'config\default.custom.yaml') -Value $plainConfig -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'config\weasel.custom.yaml') -Value $weaselConfig -NoNewline

# 只碰临时目录：不装输入法、不 taskkill、不 prune、不重启任何服务。
# 自动安装关掉 → 这条路径验证的是「产物铺好但输入法没就绪」时的报告口径。
$env:RIMESYNC_PREBUILT_DIR = $payload
$env:RIMESYNC_WEASEL_DATA = $data
$env:RIMESYNC_INSTALL_IME = '0'
$env:RIMESYNC_PRUNE_SCHEMAS = '0'
$env:RIMESYNC_DEPLOY_NO_RESTART = '1'

Write-Host "first-start root = $root"
& $cli --rime-dir $rime ensure-ime --mode install --json --report $report
$code = $LASTEXITCODE
if (-not (Test-Path -LiteralPath $report)) {
    throw "CLI 没有写 JSON 报告（exit=$code）：$report"
}
$json = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json

function Assert-Equal {
    param($Actual, $Expected, [string]$What)
    if ($Actual -ne $Expected) {
        throw "$What 期望 $Expected，实际 $Actual"
    }
}

# 产物侧：铺好了、可用、config 没被门禁跳过。
Assert-Equal $json.result 'ok' '首次启动 result'
Assert-Equal $json.build_ok $true '首次启动 build_ok'
if ($null -ne $json.config_skipped -and @($json.config_skipped).Count -ne 0) {
    throw "首次启动 config 被门禁跳过：$($json.config_skipped -join ',')"
}
Assert-Equal $json.mode 'install' '入口身份 mode'

# 就绪侧：自动安装被关掉、机器上又没有输入法 → 不能报成功。
Assert-Equal $json.weasel_install 'disabled' 'weasel_install（RIMESYNC_INSTALL_IME=0）'
Assert-Equal $json.weasel_present $false 'weasel_present'
Assert-Equal $json.ok $false '首次启动 ok（没就绪不能报成功）'
if ($code -eq 0) {
    throw "首次启动退出码是 0（输入法没就绪，必须非零）"
}
if ([string]$json.detail -notmatch '自动安装已关闭') {
    throw "首次启动 detail 没说明为什么没就绪：$($json.detail)"
}

$schemaPath = Join-Path $rime 'build\luna_pinyin_simp.schema.yaml'
if (-not (Test-Path -LiteralPath $schemaPath)) {
    throw "首次启动后 build 里没有 schema.yaml：$schemaPath"
}
$schemaText = Get-Content -LiteralPath $schemaPath -Raw
if ($schemaText -notmatch 'rimesync_vnum') {
    throw "首次启动铺进去的 schema.yaml 不含 rimesync_vnum：$schemaPath"
}
if ($null -eq $json.config_seeded) {
    throw 'JSON 报告缺 config_seeded（安装版首次启动必须报告模板播种结果）'
}
if (@($json.actions | Where-Object { $_.kind -eq 'weasel' }).Count -lt 1) {
    throw 'actions 里没有 weasel 步骤（首次启动必须先保证输入法本体）'
}

Write-Host 'windows e2e ok：首次启动自举（ensure-ime --mode install）产物铺好、build 含 rimesync_vnum，没就绪时不说成功'

# 同上：CLI 在「没就绪」时退出码是 1（这是**预期**），但 GitHub 的 pwsh step 按
# $LASTEXITCODE 判定成败，不显式 exit 0 会被带成「步骤失败」。
exit 0
