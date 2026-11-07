<#
.SYNOPSIS
  Windows e2e：模拟「安装器装完程序后，程序首次启动」的输入法自举。

.DESCRIPTION
  B 方案：NSIS 安装器只交付程序本体，输入法由程序首次启动时
  ensure(Mode::Install) 装好。本步骤在 CI 上用 CLI 等价子命令跑一遍：
    rime-sync-cli --rime-dir <fake> ensure-ime --mode install --json --report <file>

  断言（全部来自 JSON 报告，不解析 stdout 文本）：
    a) 退出码 0、result=ok、mode=install、build_ok=true；
    b) build/luna_pinyin_simp.schema.yaml 含 rimesync_vnum（是我们这套产物）；
    c) 报告带 weasel_* 与 config_seeded 字段（安装版首次启动的完整语义）。

  输入法本体：CI 机器上不装真实小狼毫（没有随包 12 MB 安装器，也不该改 runner）。
  用 RIMESYNC_INSTALL_IME=0 显式关闭安装器调用 —— 这正是「用户手动关闭自动安装」
  的同一条代码路径，weasel_install 会如实报告 disabled，其余步骤（模板/prebuilt/
  data/部署/assess）全部照常执行。
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

# 随包预编译产物（最小形态：一个主方案 + 一个 bin，schema.yaml 带插件标记）。
$marked = @"
patch:
  rimesync_vnum: '['
  rimesync_punct: ']'
"@
$plainConfig = @"
patch:
  schema_list: []
"@
Set-Content -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.schema.yaml') -Value $marked -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.prism.bin') -Value 'prism' -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'config\default.custom.yaml') -Value $plainConfig -NoNewline

# 只碰临时目录：不装输入法、不 taskkill、不 prune、不重启任何服务。
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

Assert-Equal $code 0 'ensure-ime --mode install 退出码'
Assert-Equal $json.result 'ok' '首次启动 result'
Assert-Equal $json.ok $true '首次启动 ok'
Assert-Equal $json.mode 'install' '入口身份 mode'
Assert-Equal $json.build_ok $true '首次启动 build_ok'
# 安装器被显式关闭：这是「自动安装关闭」的合法路径，不是失败。
Assert-Equal $json.weasel_install 'disabled' 'weasel_install（RIMESYNC_INSTALL_IME=0）'

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

Write-Host 'windows e2e ok：首次启动自举（ensure-ime --mode install）result=ok 且 build 含 rimesync_vnum'
