#!/usr/bin/env bash
# check-nsis-hooks.sh —— 在 Linux/macOS 上单独编译 packaging/nsis-hooks*.nsh。
#
# 为什么需要：钩子文件靠 Tauri 模板提供的 PRODUCTNAME / MAINBINARYNAME /
# MAINBINARYSRCPATH 与两个 NSIS_HOOK_* 插入点才能编译，平时只能在 CI 的 Windows
# runner（pnpm tauri build）里被间接验证。本脚本把这些桩起来，用 makensis -WX
# （告警即错误）编译「在线」与「离线」两套钩子，几分钟内就能发现语法/宏错误。
#
# 用法：
#   bash packaging/windows/check-nsis-hooks.sh            # 没有 makensis 时打印 SKIP 并退出 0
#   bash packaging/windows/check-nsis-hooks.sh --strict   # 没有 makensis 视为失败（CI 用）
#
# 验证范围：NSIS 编译期（语法、标签、!ifdef 分支、File 内嵌路径、未引用函数告警）。
# 不验证：运行期行为（注册表探测、弹框、ExecWait 退出码）—— 那需要真 Windows。
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PKG=$(cd "$HERE/.." && pwd)
STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

# ---------------------------------------------------------------------------
# 1) 静态结构断言（不需要 makensis）
#    编译只能证明「语法对」，证明不了「卸载命令长得对 / 超时在 / 失败不 Abort」。
#    这里按函数体做文本断言，改坏了会直接指出是哪一条（--strict 与非 strict 都跑）。
#    0.6.11 起：卸载旧版 → 全新安装，POSTINSTALL 按新版无条件写配置（覆盖前备份）。
#    0.6.13 起：Weasel 就绪判定（注册表 + exe）、未就绪自动修复且失败硬退出、
#               配置写进 current 用户目录、POSTINSTALL 自检（部署 + build 产物）。
#    0.6.14 起：判定只认「目录里真有 WeaselServer.exe」（tsf 只写日志）、先判定再卸/装、
#               未就绪改名留证 + 重装失败回滚（Rime.bak-<ts> 改回原名）、
#               UNINSTALL-OLD 不再卸 Weasel、SELF-CHECK 硬性检查失败 → 退出码 5。
#    0.6.15 起：预编译 build 产物兜底 —— 部署没产出（或产出的不是我们这套）时，
#               把 $INSTDIR\resources\prebuilt 铺进用户目录，SELF-CHECK 增加 build-source=。
# ---------------------------------------------------------------------------
# RIMESYNC_HOOKS：给「反向验证」用（对临时改坏的副本跑同一套断言，不动工作区文件）
HOOKS="${RIMESYNC_HOOKS:-$PKG/nsis-hooks.nsh}"
static_fail=0
static_count=0

assert_has() { # <说明> <固定字符串>
  static_count=$((static_count + 1))
  if ! grep -qF -- "$2" "$HOOKS"; then
    printf 'FAIL static  %s\n           缺少：%s\n' "$1" "$2" >&2
    static_fail=1
  fi
}

fn_body() { # <函数名> → 该函数体（到同列 FunctionEnd 为止）
  awk -v fn="$1" '
    $0 == "Function " fn { inside = 1 }
    inside { print }
    inside && $0 == "FunctionEnd" { exit }
  ' "$HOOKS"
}

assert_fn_has() { # <函数名> <说明> <固定字符串>
  static_count=$((static_count + 1))
  if ! fn_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           函数 %s 里缺少：%s\n' "$2" "$1" "$3" >&2
    static_fail=1
  fi
}

assert_count() { # <说明> <固定字符串> <最少出现次数>（整文件，用于宏体里的调用）
  static_count=$((static_count + 1))
  n=$(grep -cF -- "$2" "$HOOKS" || true)
  if [ "${n:-0}" -lt "$3" ]; then
    printf 'FAIL static  %s\n           整个钩子文件里 "%s" 只出现 %s 处（应 ≥ %s）\n' "$1" "$2" "${n:-0}" "$3" >&2
    static_fail=1
  fi
}

assert_fn_count() { # <函数名> <说明> <固定字符串> <最少出现次数>
  static_count=$((static_count + 1))
  n=$(fn_body "$1" | grep -cF -- "$3" || true)
  if [ "${n:-0}" -lt "$4" ]; then
    printf 'FAIL static  %s\n           函数 %s 里 "%s" 只出现 %s 处（应 ≥ %s）\n' "$2" "$1" "$3" "${n:-0}" "$4" >&2
    static_fail=1
  fi
}

assert_fn_not_has() { # <函数名> <说明> <禁止出现的固定字符串>
  static_count=$((static_count + 1))
  if fn_body "$1" | grep -qF -- "$3"; then
    printf 'FAIL static  %s\n           函数 %s 里不该有：%s\n' "$2" "$1" "$3" >&2
    static_fail=1
  fi
}

assert_fn_no_abort() { # <函数名>
  static_count=$((static_count + 1))
  if fn_body "$1" | grep -qE '^[[:space:]]*Abort\b'; then
    printf 'FAIL static  %s 里出现 Abort（卸载/写配置失败只许记日志）\n' "$1" >&2
    static_fail=1
  elif [ -z "$(fn_body "$1")" ]; then
    printf 'FAIL static  找不到函数 %s\n' "$1" >&2
    static_fail=1
  fi
}

assert_absent() { # <说明> <固定字符串>：整文件里必须已经不存在
  static_count=$((static_count + 1))
  if grep -qF -- "$2" "$HOOKS"; then
    printf 'FAIL static  %s\n           不该再出现：%s\n' "$1" "$2" >&2
    static_fail=1
  fi
}

assert_fn_order() { # <函数名> <先出现> <后出现> <说明>
  static_count=$((static_count + 1))
  a=$(fn_body "$1" | grep -nF -- "$2" | head -1 | cut -d: -f1)
  b=$(fn_body "$1" | grep -nF -- "$3" | head -1 | cut -d: -f1)
  if [ -z "$a" ] || [ -z "$b" ] || [ "$a" -ge "$b" ]; then
    printf 'FAIL static  %s\n           %s 里 "%s"(行 %s) 必须先于 "%s"(行 %s)\n' \
      "$4" "$1" "$2" "${a:-缺}" "$3" "${b:-缺}" >&2
    static_fail=1
  fi
}

assert_tsf_log_only() { # tsf= 只许出现在 phase=TSF-EVIDENCE 证据行（注释行除外）
  static_count=$((static_count + 1))
  bad=$(grep -n 'tsf=' "$HOOKS" | grep -v 'phase=TSF-EVIDENCE' | grep -vE '^[0-9]+:;' || true)
  if [ -n "$bad" ]; then
    printf 'FAIL static  tsf 参与了判定/日志分支（只许出现在 phase=TSF-EVIDENCE 行）\n%s\n' "$bad" >&2
    static_fail=1
  fi
}

macro_body() { # <宏名> → 宏体（到 !macroend 为止）
  awk -v m="!macro $1" '
    $0 == m { inside = 1 }
    inside { print }
    inside && $0 == "!macroend" { exit }
  ' "$HOOKS"
}

assert_macro_order() { # <宏名> <先出现> <后出现> <说明>
  static_count=$((static_count + 1))
  a=$(macro_body "$1" | grep -nF -- "$2" | head -1 | cut -d: -f1)
  b=$(macro_body "$1" | grep -nF -- "$3" | head -1 | cut -d: -f1)
  if [ -z "$a" ] || [ -z "$b" ] || [ "$a" -ge "$b" ]; then
    printf 'FAIL static  %s\n           %s 里 "%s"(行 %s) 必须先于 "%s"(行 %s)\n' \
      "$4" "$1" "$2" "${a:-缺}" "$3" "${b:-缺}" >&2
    static_fail=1
  fi
}

# 卸载旧版：抽成函数、静默 + 原地卸载（_?=）、120 秒超时、先停进程
assert_has "PREINSTALL 调用卸载旧版"                "Call RimeSyncUninstallOldVersion"
assert_has "旧 rime-sync：路径带引号 + /S + 原地 _?=" '"$R9" /S _?=$0'
assert_has "卸载前先 taskkill 同步程序"           "taskkill /IM rime-sync-app.exe /F"
assert_has "卸载前先 taskkill 输入法服务"         "taskkill /IM WeaselServer.exe /F"
assert_has "卸载等待有 120 秒超时"                '!define RIMESYNC_UNINSTALL_TIMEOUT_MS "120000"'
assert_has "超时通过 nsExec /TIMEOUT 生效"        'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_UNINSTALL_TIMEOUT_MS}'
assert_has "阶段日志 UNINSTALL-OLD"               "phase=UNINSTALL-OLD"
assert_has "/SKIPIME 记 weasel-kept"              'StrCpy $RimeSyncWeaselKept "1"'

# 0.6.14 修复点 3：UNINSTALL-OLD 只卸同步程序，不得连带卸 Weasel
assert_fn_not_has RimeSyncUninstallOldVersion "UNINSTALL-OLD 不再卸 Weasel" 'Call RimeSyncUninstallOldWeasel'
assert_absent "旧 Weasel 卸载函数整套删除" 'Function RimeSyncUninstallOldWeasel'
assert_absent "旧 Weasel 卸载命令变量删除" 'RimeSyncOldWeaselCmd'

# 0.6.14 修复点 2：顺序 = 先判定 → 再卸/重装
assert_macro_order NSIS_HOOK_PREINSTALL 'Call RimeSyncWeaselPreflight' 'Call RimeSyncUninstallOldVersion' \
  "PREINSTALL 必须先判就绪再卸旧版"
assert_has "PREINSTALL 有独立的只读 preflight" 'Call RimeSyncWeaselPreflight'
assert_fn_has RimeSyncWeaselPreflight "preflight 只做就绪判定" 'Call RimeSyncWeaselReady'
assert_has "preflight 日志阶段名" 'phase=IME-CHECK stage=preflight'
assert_fn_order RimeSyncInstallWeaselPre 'Call RimeSyncWeaselReady' 'Call RimeSyncBackupLeftoverWeasel' \
  "必须判定后才能改名"
assert_fn_order RimeSyncInstallWeaselPre 'Call RimeSyncBackupLeftoverWeasel' 'Call RimeSyncRunWeaselInstaller' \
  "先改名留证再跑安装器"
assert_fn_order RimeSyncInstallWeaselPre 'Call RimeSyncRunWeaselInstaller' 'Call RimeSyncRollbackWeasel' \
  "安装并复查之后才回滚"
assert_fn_order RimeSyncInstallWeaselPre 'Call RimeSyncRollbackWeasel' 'Call RimeSyncAbortWeasel' \
  "回滚必须在硬失败之前"

# 0.6.14 修复点 1：就绪判定唯一硬条件 = 注册表值指向的目录里真有 WeaselServer.exe
assert_fn_count RimeSyncWeaselReady "就绪判定读 InstallDir（64+32 两个视图）" \
  'ReadRegStr $R5 HKLM "Software\Rime\Weasel" "InstallDir"' 2
assert_fn_count RimeSyncWeaselReady "就绪判定读 WeaselRoot（64+32 两个视图）" \
  'ReadRegStr $R5 HKLM "Software\Rime\Weasel" "WeaselRoot"' 2
assert_fn_has RimeSyncWeaselReady "就绪判定同时读 32 位视图" 'SetRegView 32'
assert_fn_has RimeSyncWeaselReadyFromDir "命中条件：目录里有 WeaselServer.exe" \
  'Call RimeSyncWeaselRootFromDir'
assert_fn_has RimeSyncWeaselReadyFromDir "探测时不冲掉依据标签（\$1 压栈）" 'Push $1'
assert_has "就绪判定要求 WeaselServer.exe 真的存在" 'IfFileExists "$R5\WeaselServer.exe"'
assert_has "任意版本号目录（weasel-*）也认" 'FindFirst $0 $1 "$R5\weasel-*"'
assert_has "注册表有值但没有 exe 也算未就绪" 'no-WeaselServer.exe'
assert_has "空目录也算未就绪" 'StrCpy $RimeSyncWeaselReadyReason "empty-dir"'
assert_has "未就绪原因是 registry-missing" 'StrCpy $RimeSyncWeaselReadyReason "registry-missing"'
assert_has "已就绪分支写清 reason=exe-present" 'action=skip reason=exe-present"'
# TSF 只写日志：它一旦出现在判定/汇总行，这条断言立刻变红
assert_tsf_log_only
assert_fn_has RimeSyncWeaselLogEvidence "TSF/进程证据单独一行写日志" 'phase=TSF-EVIDENCE'
assert_fn_has RimeSyncWeaselLogEvidence "证据行标注不参与判定" 'log-only-not-used-for-readiness'
assert_fn_not_has RimeSyncInstallWeaselPre "IME-CHECK 分支不再带 tsf" 'tsf='

# 0.6.14 修复点 4：未就绪 → 停进程 → 改名留证（非空且 exe 在）→ 内嵌安装器（300s）→ 复查
assert_fn_has RimeSyncInstallWeaselPre "未就绪走修复流程" 'Call RimeSyncBackupLeftoverWeasel'
assert_fn_has RimeSyncInstallWeaselPre "修复前先停输入法服务" "taskkill /IM WeaselServer.exe /F"
assert_fn_count RimeSyncInstallWeaselPre "修复走带 300 秒超时的等待" 'Call RimeSyncRunWeaselInstaller' 1
assert_fn_has RimeSyncInstallWeaselPre "内嵌安装器静默 + 防重启" '"$R0" /S /NOREBOOT'
assert_fn_has RimeSyncInstallWeaselPre "安装后必须复查就绪" 'Call RimeSyncWeaselReady'
assert_fn_has RimeSyncInstallWeaselPre "未就绪分支写清原因" 'phase=IME-CHECK ready=no'
assert_has "残留目录改名成 Rime.bak-<时间戳>" '.bak-$R2'
assert_fn_has RimeSyncBackupLeftoverDir "只改非空目录" 'IfFileExists "$9\*.*" 0 rimesync_bk_skip'
assert_fn_has RimeSyncBackupLeftoverDir "只改 exe 真的在的目录" 'StrCmp $R8 "" rimesync_bk_skip'
assert_fn_has RimeSyncBackupLeftoverDir "改名前的 exe 检查真的去探测" 'Call RimeSyncWeaselRootFromDir'
assert_has "跳过改名也要写日志" 'rename skipped dir='
assert_has "改名成功写进日志" 'phase=IME-REPAIR rename ok'
assert_has "安装器等待上限 300 秒" '!define RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS "300000"'
assert_has "300 秒超时真的传给 nsExec" \
  'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_WEASEL_INSTALL_TIMEOUT_MS}'
assert_fn_has RimeSyncInstallWeaselPre "缺少载荷也走硬失败" 'StrCpy $R7 "payload-missing"'
assert_fn_has RimeSyncInstallWeaselPre "缺载荷写清日志" 'phase=IME-INSTALL payload=missing'

# 0.6.14 修复点 2/5：复查失败 → 回滚（Rime.bak-<ts> 改回原名）→ 硬失败
assert_fn_has RimeSyncInstallWeaselPre "复查失败写 WEASEL-INSTALL failed" \
  'WEASEL-INSTALL failed reason=$R7 installer-exit=$R4 rollback=$RimeSyncWeaselRollback'
assert_fn_has RimeSyncRollbackWeasel "回滚把备份目录改回原名" \
  'Rename "$RimeSyncWeaselBackupDir" "$RimeSyncWeaselBackupOrig"'
assert_fn_has RimeSyncRollbackWeasel "回滚结果 ok/failed/none" 'StrCpy $RimeSyncWeaselRollback "ok"'
assert_fn_has RimeSyncRollbackWeasel "改名失败必须记 failed（不许假报 ok）" 'StrCpy $RimeSyncWeaselRollback "failed"'
assert_fn_has RimeSyncRollbackWeasel "回滚日志可复核" 'phase=WEASEL-ROLLBACK result='
assert_fn_has RimeSyncRollbackWeasel "回滚成功判据是 exe 真的回来" 'Call RimeSyncWeaselRootFromDir'
assert_fn_has RimeSyncAbortWeasel "硬失败设非零退出码" 'SetErrorLevel 5'
assert_fn_has RimeSyncAbortWeasel "硬失败必须 Abort" 'Abort'
assert_has "硬失败路径在安装器之后可达" 'Call RimeSyncAbortWeasel'

# 0.6.13：配置播种必须写进 current 用户目录（不能是 ProgramData）
assert_fn_count RimeSyncSeedRimeConfig "播种切到 current 上下文" 'SetShellVarContext current' 1
assert_fn_has RimeSyncSeedRimeConfig "解析出的用户目录存进全局" 'StrCpy $RimeSyncRimeUserDir "$APPDATA"'
assert_fn_has RimeSyncSeedRimeConfig "默认配置写进该目录" 'CreateDirectory "$RimeSyncRimeUserDir"'
assert_fn_has RimeSyncSeedRimeConfig "weasel.custom.yaml 也写该目录" 'StrCpy $1 "$RimeSyncRimeUserDir"'
assert_fn_not_has RimeSyncSeedRimeConfig '播种里不再直接用 $APPDATA 拼路径' '$APPDATA\Rime'

# 0.6.14 修复点 6：安装结束自检 = 硬性检查（InstallDir / 默认布局 exe）+ 退出码 5
assert_fn_has RimeSyncSelfCheck "自检再判一次就绪" 'Call RimeSyncWeaselReady'
assert_fn_has RimeSyncSelfCheck "硬性检查读注册表 InstallDir（32 位视图兜底）" \
  'ReadRegStr $0 HKLM "Software\Rime\Weasel" "InstallDir"'
assert_fn_has RimeSyncSelfCheck "硬性检查默认布局 WeaselServer.exe" 'Call RimeSyncScanDefaultLayout'
assert_fn_has RimeSyncScanDefaultLayout "扫 weasel-* 里的 WeaselServer.exe" \
  'IfFileExists "$2\$1\WeaselServer.exe"'
assert_fn_has RimeSyncSelfCheck "任一不满足 → weasel-ready=no" 'phase=SELF-CHECK weasel-ready=no ready='
assert_fn_has RimeSyncSelfCheck "/SKIPIME 不因自检硬失败" 'warning=weasel-not-ready-skipped'
assert_fn_has RimeSyncSelfCheck "硬失败设退出码 5" '    SetErrorLevel 5'
assert_fn_has RimeSyncSelfCheck "非静默弹 STOP 框" 'MessageBox MB_OK|MB_ICONSTOP'
assert_fn_has RimeSyncSelfCheck "自检调用部署" 'WeaselDeployer.exe" /deploy'
assert_has "部署等待上限 20 秒" '!define RIMESYNC_DEPLOY_TIMEOUT_MS "20000"'
assert_has "20 秒超时真的传给 nsExec" 'nsExec::ExecToStack /TIMEOUT=${RIMESYNC_DEPLOY_TIMEOUT_MS}'
assert_fn_has RimeSyncSelfCheck "自检检查 build 里的 luna_pinyin_simp" 'build\luna_pinyin_simp.schema.yaml'
assert_fn_has RimeSyncSelfCheck "自检写 phase=SELF-CHECK" 'phase=SELF-CHECK'
assert_fn_has RimeSyncSelfCheck "自检报配置目录与数量" 'configdir=$0 configs=$RimeSyncSeedWritten'
assert_fn_has RimeSyncSelfCheck "自检未更新只告警" 'warning=build-not-updated'
assert_fn_has RimeSyncSelfCheck "非静默时弹简报框" 'MessageBox MB_OK|MB_ICONINFORMATION'
assert_count "POSTINSTALL 调用自检" 'Call RimeSyncSelfCheck' 1
assert_has "自检失败不写 DONE、改写 SETUP failed" 'phase=SETUP failed stage=SELF-CHECK exit=5'
assert_has "DONE 行带 install-dir / default-exe" 'install-dir=$RimeSyncWeaselRegInstallDir default-exe=$RimeSyncWeaselDefaultExe'

# 0.6.15：预编译 build 产物兜底（某些机器上 WeaselDeployer /deploy 返回 0 但什么都不写）
assert_has "预编译产物随安装包分发" 'resources\prebuilt'
assert_has "兜底判据落在 build\luna_pinyin_simp.schema.yaml" 'build\luna_pinyin_simp.schema.yaml'
assert_fn_has RimeSyncProbeBuild "判据要求插件标记 rimesync_vnum" 'StrCpy $R8 "rimesync_vnum"'
assert_fn_has RimeSyncProbeBuild "判据要求插件标记 rimesync_punct" 'StrCpy $R8 "rimesync_punct"'
assert_fn_has RimeSyncFileContains "真的逐行读文件内容" 'FileOpen $0 "$R9" r'
assert_fn_count RimeSyncEnsurePrebuiltBuild "build 与 config 两个目录都要铺" 'Call RimeSyncCopyPrebuiltDir' 2
assert_fn_has RimeSyncCopyPrebuiltDir "存在即跳过（不覆盖用户已生成的）" 'IfFileExists "$6\$2" 0 rimesync_cpd_copy'
assert_fn_has RimeSyncCopyPrebuiltDir "RIMESYNC_PREBUILT=force 才覆盖" 'StrCmp $RimeSyncPrebuiltEffMode "force" rimesync_cpd_copy'
assert_fn_has RimeSyncEnsurePrebuiltBuild "第一遍铺完不可用 → 只对 build/ 覆盖重铺" 'scope=build-only'
assert_fn_has RimeSyncEnsurePrebuiltBuild "重铺结果写进日志" 'escalated=$RimeSyncPrebuiltRecopy'
assert_fn_has RimeSyncCopyPrebuiltDir "复制失败逐个记日志" 'phase=PREBUILT copy failed file='
assert_fn_has RimeSyncPrebuiltMode "关闭开关 RIMESYNC_PREBUILT=0" 'StrCpy $RimeSyncPrebuiltMode "off"'
assert_fn_has RimeSyncPrebuiltMode "强制覆盖开关 RIMESYNC_PREBUILT=force" 'StrCpy $RimeSyncPrebuiltMode "force"'
assert_fn_has RimeSyncEnsurePrebuiltBuild "部署可用时记 source=deploy" 'PREBUILT ok source=deploy'
assert_fn_has RimeSyncEnsurePrebuiltBuild "铺制成功记 PREBUILT ok source=prebuilt" 'PREBUILT ok source=prebuilt'
assert_fn_has RimeSyncEnsurePrebuiltBuild "铺完必须复查" 'reason=probe-after-copy'
assert_fn_has RimeSyncEnsurePrebuiltBuild "复查失败记 PREBUILT failed" 'phase=PREBUILT failed'
assert_fn_has RimeSyncEnsurePrebuiltBuild "载荷缺失单独一个原因" 'reason=payload-missing'
assert_fn_has RimeSyncEnsurePrebuiltBuild "复查失败要告警（加计数）" 'IntOp $RimeSyncSelfCheckWarn $RimeSyncSelfCheckWarn + 1'
assert_fn_order RimeSyncSelfCheck 'WeaselDeployer.exe" /deploy' 'Call RimeSyncEnsurePrebuiltBuild' \
  "先尝试 /deploy，再用预编译产物兜底"
assert_fn_has RimeSyncSelfCheck "兜底成功不再报 build-not-updated" 'StrCmp $RimeSyncBuildSource "prebuilt" rimesync_sc_build_ok'
assert_fn_has RimeSyncSelfCheck "SELF-CHECK 增加 build-source 字段" 'build-source=$RimeSyncBuildSource'
assert_has "DONE 行也带 build-source" 'build=$RimeSyncBuildState build-source=$RimeSyncBuildSource'
assert_has "卸载时清掉随包产物" 'RMDir /r "$INSTDIR\resources\prebuilt"'

# ---------------------------------------------------------------------------
# 0.6.21：随包只保留 3 个方案（朙月拼音·简化字 / 五笔拼音 / 五笔86）
#   * 模板目录必须与 packaging/rime/retained-schemas.txt 逐项一致（清单是单一真相源）；
#   * POSTINSTALL 必须调用 rime-sync-cli prune-schemas（钩子不另抄一份清单）；
#   * 钩子里不许出现已移除方案的名字（否则就是把清单抄进了 NSIS）。
# ---------------------------------------------------------------------------
RETAINED_LIST="$PKG/rime/retained-schemas.txt"
static_count=$((static_count + 1))
if [ ! -f "$RETAINED_LIST" ]; then
  printf 'FAIL static  缺少保留清单 %s\n' "$RETAINED_LIST" >&2
  static_fail=1
else
  want=$(sed -e 's/#.*//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' "$RETAINED_LIST" | sed '/^$/d' | sort)
  have=$(cd "$PKG/rime/schemas" 2>/dev/null && ls *.custom.yaml 2>/dev/null | sed 's/\.custom\.yaml$//' | sort)
  if [ "$want" != "$have" ]; then
    printf 'FAIL static  方案模板与保留清单不一致\n           清单：%s\n           模板：%s\n' \
      "$(printf '%s' "$want" | tr '\n' ' ')" "$(printf '%s' "$have" | tr '\n' ' ')" >&2
    static_fail=1
  fi
  count=$(printf '%s\n' "$want" | wc -l)
  if [ "$count" -ne 3 ]; then
    printf 'FAIL static  保留清单应为 3 个方案，实际 %s\n' "$count" >&2
    static_fail=1
  fi
fi
assert_has "POSTINSTALL 调用方案清理" 'Call RimeSyncPruneSchemas'
assert_fn_has RimeSyncPruneSchemas "清理只调 CLI 子命令" 'prune-schemas'
assert_fn_has RimeSyncPruneSchemas "CLI 不存在时只跳过" 'rimesync_prune_no_cli'
assert_fn_no_abort RimeSyncPruneSchemas
assert_has "清理阶段日志 phase=PRUNE" 'phase=PRUNE'
assert_has "DONE 行带 prune 状态" 'prune=$RimeSyncPruneState'
assert_fn_has RimeSyncSeedRimeConfig "模板只从 resources\schemas 读取" 'resources\schemas'
assert_absent "钩子不把已移除方案写死" 'terra_pinyin'
assert_absent "钩子不把已移除方案写死（仓颉）" 'cangjie5'

# ---------------------------------------------------------------------------
# 0.6.20：随包输入法安装器（程序自己修复输入法，不依赖 NSIS 钩子）
#   227 实测：/S 静默安装时钩子不执行，旧 Weasel 又被卸载器卸掉，机器上没有输入法。
#   所以安装器必须真的进 Tauri resources（Windows 平台配置），且与 NSIS 钩子内嵌的
#   是同一个文件、同一个文件名；卸载时也要清掉。
# ---------------------------------------------------------------------------
REPO=$(cd "$PKG/.." && pwd)
WIN_CONF="$REPO/src-tauri/tauri.windows.conf.json"
CORE_IME="$REPO/crates/core/src/ime.rs"
WEASEL_NAME="weasel-0.17.4.0-installer.exe"

assert_file_has() { # <说明> <文件> <固定字符串>
  static_count=$((static_count + 1))
  if ! grep -qF -- "$3" "$2" 2>/dev/null; then
    printf 'FAIL static  %s\n           文件 %s 里缺少：%s\n' "$1" "$2" "$3" >&2
    static_fail=1
  fi
}

assert_file_has "Tauri resources 带随包输入法安装器（Windows 平台配置）" "$WIN_CONF" \
  '"../packaging/windows/vendor/weasel-0.17.4.0-installer.exe": "resources/weasel/weasel-0.17.4.0-installer.exe"'
assert_file_has "core 运行时从 resources/weasel 找安装器" "$CORE_IME" \
  'join("resources").join("weasel")'
assert_file_has "core 里的安装器文件名与打包一致" "$CORE_IME" \
  'pub const INSTALLER_FILE: &str = "weasel-0.17.4.0-installer.exe";'
assert_has "NSIS 卸载清掉 resources\weasel" 'RMDir /r "$INSTDIR\resources\weasel"'
assert_has "NSIS 兜底也认 resources\weasel" \
  'IfFileExists "$INSTDIR\resources\weasel\${RIMESYNC_WEASEL_INSTALLER}"'

# 12 MB 的 Windows 安装器不能塞进基础配置：Linux 的 deb/AppImage 会白白变大。
if grep -qF -- "$WEASEL_NAME" "$REPO/src-tauri/tauri.conf.json"; then
  printf 'FAIL static  基础 tauri.conf.json 不该引用 %s（放进 tauri.windows.conf.json，Linux 包不背这个体积）\n' \
    "$WEASEL_NAME" >&2
  static_fail=1
fi

# 工作区里已有 vendor 安装器时，顺带验证载荷本身（CI 的 windows job 还会再验一次）。
vendor_exe="$PKG/windows/vendor/$WEASEL_NAME"
if [ -f "$vendor_exe" ]; then
  static_count=$((static_count + 1))
  size=$(wc -c <"$vendor_exe")
  magic=$(head -c 2 "$vendor_exe")
  if [ "$size" -lt 8000000 ]; then
    printf 'FAIL static  %s 只有 %s 字节（应 ≥ 8 MiB）：下载/编译出来的多半是错误页或占位文件\n' \
      "$vendor_exe" "$size" >&2
    static_fail=1
  elif [ "$magic" != "MZ" ]; then
    printf 'FAIL static  %s 不是 PE 文件（缺少 MZ 头）\n' "$vendor_exe" >&2
    static_fail=1
  else
    printf 'ok   vendor  %s（%s 字节，MZ 头）\n' "$WEASEL_NAME" "$size"
  fi
else
  # 没有本地载荷不算错（Linux 上本来就没有）；但 Tauri 的 Windows 打包需要它，
  # 所以把「怎么来」写清楚，避免 CI 红了才去猜。
  printf 'note vendor  %s 不在工作区（Windows 打包前需先跑 fetch-weasel.ps1 / 自编译）\n' "$vendor_exe"
fi

# 失败一律只记日志：这些函数体里不能有 Abort
for fn in \
  RimeSyncRunAndWait \
  RimeSyncRunWeaselInstaller \
  RimeSyncNormalizeUninstallPath \
  RimeSyncUninstallOldRimeSync \
  RimeSyncUninstallOldVersion \
  RimeSyncWriteConfigFile \
  RimeSyncSeedRimeConfig \
  RimeSyncSelfCheck \
  RimeSyncBackupLeftoverWeasel \
  RimeSyncBackupLeftoverDir \
  RimeSyncRollbackWeasel \
  RimeSyncWeaselPreflight \
  RimeSyncWeaselLogEvidence \
  RimeSyncScanDefaultLayout \
  RimeSyncWeaselReady \
  RimeSyncFileContains \
  RimeSyncProbeBuild \
  RimeSyncPrebuiltMode \
  RimeSyncCopyPrebuiltDir \
  RimeSyncEnsurePrebuiltBuild; do
  assert_fn_no_abort "$fn"
done

if [ "$static_fail" -ne 0 ]; then
  echo "check-nsis-hooks: 静态结构断言失败（见上）" >&2
  exit 1
fi
printf 'ok   static  %s 条结构断言（就绪判定只认 exe/TSF 只写日志/先判定再卸装/改名留证/回滚/自检硬失败退出码 5/预编译产物兜底/失败不 Abort）\n' "$static_count"

if ! command -v makensis >/dev/null 2>&1; then
  if [ "$STRICT" -eq 1 ]; then
    echo "check-nsis-hooks: 缺少 makensis（apt-get install nsis / makensis.org），--strict 下视为失败" >&2
    exit 1
  fi
  echo "check-nsis-hooks: SKIP（没有 makensis；在线包/离线包的真实校验在 CI 的 windows job）"
  exit 0
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/packaging/windows/vendor" "$WORK/out"
ln -sfn "$PKG/nsis-hooks.nsh" "$WORK/packaging/nsis-hooks.nsh"
ln -sfn "$PKG/nsis-hooks-offline.nsh" "$WORK/packaging/nsis-hooks-offline.nsh"

# 假载荷：只需存在即可（NSIS 编译期 File 会读它）。
printf 'MZ fake weasel installer (harness only)\n' \
  >"$WORK/packaging/windows/vendor/weasel-0.17.4.0-installer.exe"
# WebView2 用不可压缩数据：离线包体积应当比在线包正好大一个载荷，
# 这就是「离线变体确实内嵌了 Standalone、在线变体没有内嵌」的编译期证据。
head -c 1048576 /dev/urandom >"$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" 2>/dev/null ||
  dd if=/dev/urandom of="$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" bs=1024 count=1024 2>/dev/null
printf 'MZ' | dd of="$WORK/packaging/windows/vendor/MicrosoftEdgeWebView2RuntimeInstallerX64.exe" \
  bs=1 seek=0 conv=notrunc status=none

cat >"$WORK/harness.nsi" <<'NSI'
; 最小 harness：桩掉 Tauri 模板的变量与插入点，只编译 rime-sync 的钩子文件。
Unicode true
Name "rime-sync hooks harness"
OutFile "OUTDIR/harness.exe"
InstallDir "$PROGRAMFILES64\rime-sync"
RequestExecutionLevel admin
!define PRODUCTNAME "rime-sync"
!define MAINBINARYNAME "rime-sync-app"
!define MAINBINARYSRCPATH "HARNESS"

INCLUDE_LINE

Section "Install"
  SetOutPath $INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  File "/oname=rime-sync-app.exe" "${MAINBINARYSRCPATH}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section "Uninstall"
  !insertmacro NSIS_HOOK_PREUNINSTALL
SectionEnd
NSI

fail=0
for variant in online offline; do
  hooks=nsis-hooks.nsh
  [ "$variant" = offline ] && hooks=nsis-hooks-offline.nsh
  sed -e "s|OUTDIR|$WORK/out|" \
    -e "s|HARNESS|$WORK/harness.nsi|" \
    -e "s|INCLUDE_LINE|!include \"$WORK/packaging/$hooks\"|" \
    "$WORK/harness.nsi" >"$WORK/harness-$variant.nsi"

  log="$WORK/$variant.log"
  if makensis -WX -V3 "$WORK/harness-$variant.nsi" >"$log" 2>&1; then
    size=$(wc -c <"$WORK/out/harness.exe")
    warn=$(grep -cE 'warning|Error' "$log" || true)
    printf 'ok   %-7s %9s bytes  warnings=%s\n' "$variant" "$size" "${warn:-0}"
    cp "$WORK/out/harness.exe" "$WORK/out/$variant.exe"
  else
    printf 'FAIL %-7s 编译失败：\n' "$variant"
    sed -n '1,40p' "$log"
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  online=$(wc -c <"$WORK/out/online.exe")
  offline=$(wc -c <"$WORK/out/offline.exe")
  delta=$((offline - online))
  printf 'delta    %8s bytes（离线包 - 在线包；假载荷 1048576 字节，应≈它）\n' "$delta"
  if [ "$delta" -lt 900000 ]; then
    echo "FAIL 离线包没有内嵌 WebView2 载荷（差值 $delta 太小）"
    fail=1
  fi
fi

[ "$fail" -eq 0 ] && echo "check-nsis-hooks: 两套钩子均 0 warning 0 error"
exit "$fail"
