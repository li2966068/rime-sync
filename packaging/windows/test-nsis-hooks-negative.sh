#!/usr/bin/env bash
# test-nsis-hooks-negative.sh —— 变异测试：证明 check-nsis-hooks.sh 的「安装器不碰输入法」
# 断言不是空转的。
#
# 做法：把真实的 Weasel 安装调用插回 NSIS_HOOK_PREINSTALL（以及一个禁用函数调用），
# 用 RIMESYNC_HOOKS 指到变异副本再跑同一个检查脚本 —— 必须变红，而且失败原因必须
# 命中注入的那一条。CI 里与 check-nsis-hooks.sh --strict 成对执行（.github/workflows/ci.yml）。
#
# 用法：bash packaging/windows/test-nsis-hooks-negative.sh
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
CHECK="$HERE/check-nsis-hooks.sh"
SRC="$HERE/../nsis-hooks.nsh"

[ -f "$CHECK" ] || { echo "FAIL negative  找不到 $CHECK" >&2; exit 1; }
[ -f "$SRC" ] || { echo "FAIL negative  找不到 $SRC" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# 在 NSIS_HOOK_PREINSTALL 的第一行之后插入一行（模拟「把安装调用搬回钩子」）。
inject() { # <输出文件> <要插入的 NSIS 行>
  awk -v line="$2" '
    /^!macro NSIS_HOOK_PREINSTALL$/ { print; print line; next }
    { print }
  ' "$SRC" >"$1"
}

fail=0

expect_red() { # <说明> <变异文件> <失败原因必须命中的正则>
  local label=$1 mutated=$2 pattern=$3
  local log="$WORK/$label.log"
  if RIMESYNC_HOOKS="$mutated" bash "$CHECK" >"$log" 2>&1; then
    printf 'FAIL negative  %s：注入后 check-nsis-hooks 仍然全绿（断言空转）\n' "$label" >&2
    tail -10 "$log" >&2
    fail=1
    return
  fi
  if ! grep -qE -- "$pattern" "$log"; then
    printf 'FAIL negative  %s：变红了但原因不对（期望命中 /%s/）\n' "$label" "$pattern" >&2
    tail -10 "$log" >&2
    fail=1
    return
  fi
  printf 'ok   negative  %s（注入后被拦下）\n' "$label"
}

# 1) 真实安装调用：随包安装器 + /S /NOREBOOT（就是 B 方案明令禁止的那件事）。
inject "$WORK/installer.nsh" \
  '  ExecWait '"'"'"$INSTDIR\resources\weasel\weasel-0.17.4.0-installer.exe" /S /NOREBOOT'"'"''
expect_red "installer" "$WORK/installer.nsh" 'NOREBOOT|resources\\weasel|weasel-0\.17\.4\.0-installer|ExecWait'

# 2) 只把禁用函数调用搬回来（哪怕不写安装器路径也必须红）。
inject "$WORK/function.nsh" '  Call RimeSyncInstallWeaselPre'
expect_red "function" "$WORK/function.nsh" 'RimeSyncInstallWeaselPre'

# 3) 换一种写法：nsExec 起安装器（没有 ExecWait，但仍有安装器名字与 /NOREBOOT）。
inject "$WORK/nsexec.nsh" \
  '  nsExec::ExecToLog '"'"'"$INSTDIR\resources\weasel\weasel-0.17.4.0-installer.exe" /S /NOREBOOT'"'"''
expect_red "nsexec" "$WORK/nsexec.nsh" 'NOREBOOT|resources\\weasel|weasel-0\.17\.4\.0-installer'


# 4) 纯注册表注册 IME（不用安装器、不出现 Weasel 字样也必须红）。
#    注意：**单反斜杠** —— NSIS 的注册表路径就是单反斜杠；这里写双反斜杠的话，
#    断言会匹配不上（曾经就是这样空转的：注入了双反斜杠，检查器也只查双反斜杠）。
inject "$WORK/registry.nsh" '  WriteRegStr HKLM "SOFTWARE\Microsoft\CTF\TIP\{0DEE1F7B}" "name" "Rime"'
expect_red "registry" "$WORK/registry.nsh" 'WriteRegStr|CTF|InProcServer32'

# 5) 自定义函数名调用（不在 gone 枚举里也必须红）。
inject "$WORK/customfn.nsh" '  Call RimeSyncInstallWeasel'
expect_red "customfn" "$WORK/customfn.nsh" 'RimeSyncInstallWeasel|Weasel'

# 6) 搬运 rime.dll（不写注册表、不起安装器，只是把引擎塞进安装目录也必须红）。
inject "$WORK/rimedll.nsh" '  File /oname=rime.dll "$INSTDIR\resources\rime.dll"'
expect_red "rimedll" "$WORK/rimedll.nsh" 'rime\.dll'

if [ "$fail" -ne 0 ]; then
  echo "test-nsis-hooks-negative: 变异测试失败（断言没拦住注入）" >&2
  exit 1
fi
# 变异总数从脚本自身统计（别再写死数字：写死过一次 3/3，实际已经是 5 个）。
mutations=$(grep -c '^expect_red ' "$0")
echo "test-nsis-hooks-negative: $mutations/$mutations 变异均被 check-nsis-hooks 拦下"
