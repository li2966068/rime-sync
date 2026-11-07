<#
.SYNOPSIS
  Windows e2e：ime_state::ensure 的判据 / 修补 / 报告一致性。

.DESCRIPTION
  不安装任何输入法，也不需要 librime：只伪造 user dir + payload，验证
    a) ensure-ime 第一次：result=Ok，build 里有带 rimesync_vnum 的 schema；
    b) 删掉一个 .bin 再跑：result=Ok 且 repaired 含该方案（补回来了）；
    c) payload 缺文件：result=Failed、退出码非 0、detail 非空。
  另加一条只读判据（check-ime，NSIS 自检用的就是它）与 ensure 结论一致。

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
Set-Content -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.schema.yaml') -Value $marked -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.prism.bin') -Value 'prism' -NoNewline
Set-Content -LiteralPath (Join-Path $payload 'config\default.custom.yaml') -Value $plainConfig -NoNewline

# 只碰临时目录；绝不装输入法、绝不 taskkill 用户输入法、绝不清理方案。
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

Write-Host "e2e root = $root"

# --- a) 首次 ensure：Ok + build 里是带标记的 schema -------------------------------
$first = Invoke-ImeCli -Arguments @('ensure-ime') -ReportPath (Join-Path $root 'ensure1.json')
Assert-Equal $first.Code 0 'ensure-ime #1 退出码'
Assert-Equal $first.Json.result 'ok' 'ensure-ime #1 result'
Assert-Equal $first.Json.build_ok $true 'ensure-ime #1 build_ok'
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
    throw 'Ok 的 actions 为空（不能既说 ok 又什么都没做）'
}

# --- b) 删掉一个 .bin → 再跑 → repaired 且 Ok ------------------------------------
Remove-Item -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin') -Force
$second = Invoke-ImeCli -Arguments @('ensure-ime') -ReportPath (Join-Path $root 'ensure2.json')
Assert-Equal $second.Code 0 'ensure-ime #2 退出码'
Assert-Equal $second.Json.result 'ok' 'ensure-ime #2 result'
Assert-Equal $second.Json.build_ok $true 'ensure-ime #2 build_ok'
if (@($second.Json.repaired) -notcontains 'luna_pinyin_simp') {
    throw "repaired 不含 luna_pinyin_simp：$($second.Json.repaired -join ',')"
}
if (-not (Test-Path -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin'))) {
    throw 'prism.bin 没有被补回来'
}

# --- c) payload 缺文件 → Failed + 非零退出码 + detail 非空 ------------------------
Remove-Item -LiteralPath (Join-Path $payload 'build\luna_pinyin_simp.prism.bin') -Force
Remove-Item -LiteralPath (Join-Path $rime 'build\luna_pinyin_simp.prism.bin') -Force
$third = Invoke-ImeCli -Arguments @('ensure-ime') -ReportPath (Join-Path $root 'ensure3.json')
if ($third.Code -eq 0) {
    throw 'ensure-ime #3 退出码是 0（payload 缺文件必须非零）'
}
Assert-Equal $third.Json.result 'failed' 'ensure-ime #3 result'
Assert-Equal $third.Json.build_ok $false 'ensure-ime #3 build_ok'
if ([string]::IsNullOrWhiteSpace([string]$third.Json.detail)) {
    throw 'failed 的 detail 为空'
}

# --- 只读判据（NSIS 自检用的命令）必须给出同一个结论 -----------------------------
$check = Invoke-ImeCli -Arguments @('check-ime') -ReportPath (Join-Path $root 'check.json')
if ($check.Code -eq 0) {
    throw 'check-ime 在 build 不可用时退出码是 0'
}
Assert-Equal $check.Json.result 'failed' 'check-ime result'
Assert-Equal $check.Json.build_ok $false 'check-ime build_ok'
if (@($check.Json.missing) -notcontains 'luna_pinyin_simp') {
    throw "check-ime missing 不含 luna_pinyin_simp：$($check.Json.missing -join ',')"
}

Write-Host 'windows e2e ok：ensure-ime 判据 / 修补 / 失败退出码 / check-ime 一致性'
