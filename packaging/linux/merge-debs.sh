#!/usr/bin/env bash
# 把「Rime 输入法 deb」与「Rime 同步 deb」合并成一个包（包名 rime-sync）。
#
#   packaging/linux/merge-debs.sh IME_DEB SYNC_DEB OUT_DEB
#
# 为什么要合并：
#   以前用户要装两个包（先 rime-inputmethod，再 rime-sync），顺序错了就装不出输入法；
#   现在合成一个包，装一次就有「同步程序 + 输入法」两样东西。
#
# 合并规则（固定设计，不要随便改）：
#   * 包名固定 rime-sync（吸收 rime-inputmethod）；Provides 保留 fcitx5-rime。
#   * Depends / Provides / Conflicts / Replaces 一律取并集并去重。
#   * Conflicts / Replaces 在并集基础上额外加上旧包名 rime-inputmethod 与 rime-sync
#     （旧 rime-sync 与新包同名，dpkg 允许包与自己冲突，实测安装/升级都正常）。
#   * Installed-Size 按合并后的文件树重算。
#   * 文件树：两个包写同一个路径 = 硬错误，直接退出，绝不静默覆盖。
#   * 维护脚本：两个包的 postinst 都要跑（先 IME 的 localepurge 提示，再同步包的），
#     prerm / postrm 同理；一个失败不挡住另一个。
#
# 依赖：coreutils / dpkg-deb / ar / tar / awk / sed（不引入新依赖）。
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)

ime_deb=${1:?usage: merge-debs.sh IME_DEB SYNC_DEB OUT_DEB}
sync_deb=${2:?usage: merge-debs.sh IME_DEB SYNC_DEB OUT_DEB}
out=${3:?usage: merge-debs.sh IME_DEB SYNC_DEB OUT_DEB}

MERGED_PKG=rime-sync
OLD_PKGS="rime-inputmethod rime-sync"

test -f "$ime_deb" || { echo "!! 找不到输入包：$ime_deb" >&2; exit 1; }
test -f "$sync_deb" || { echo "!! 找不到输入包：$sync_deb" >&2; exit 1; }
mkdir -p "$(dirname "$out")"

say() { printf '[merge-debs] %s\n' "$*"; }
fail() { echo "!! merge-debs: $*" >&2; exit 1; }

# ---------------------------------------------------------------- 输入法探测兼容层
# 为什么需要：合并之后包名变成 rime-sync，旧的 rime-inputmethod 不再是 dpkg 里的
# 包名；而 fcitx5-rime 是虚包，dpkg-query 查虚包只会得到 not-installed。于是
# packaging/linux/postinst 里的 rime_im_installed() 永远为假 → 会去排队后台装
# 发行版 fcitx5-rime —— 而本包 Conflicts: fcitx5-rime，apt 为了装它会顺手把
# 本包自己卸掉（实测 apt 会 Remv 冲突方）。
# 做法：给 dpkg-query 包一层，问 rime-inputmethod / fcitx5-rime 的安装状态时，
# 只要我们自带的输入法组件确实在位，就回答「已安装」。
# 这等价于合并前「先装 rime-inputmethod 再装 rime-sync」时的状态。
# 影响范围：只在被 source/执行这个文件的那棵进程树内（export -f 随环境传给子
# bash），系统里其它程序用到的还是真的 dpkg-query。
# 同一份内容有两处用途：
#   1) 内联进合并后的 postinst —— 覆盖安装时 postinst 自己 + 它拉起的后台 worker；
#   2) 落成 /usr/lib/rime-sync/ime-detect-shim.sh —— 供诊断页「重试」路径
#      （pkexec 起的新 bash 会清空环境，收不到 export -f，只能靠文件里自带）。
write_ime_shim() { # <输出文件>
  cat >"$1" <<'RIME_SYNC_IME_SHIM_EOF'
# 合并包专用：输入法「是否已安装」的探测兼容层（由 merge-debs.sh 生成）。
RIME_SYNC_IME_ROOT=${RIME_SYNC_IME_ROOT:-}
export RIME_SYNC_IME_ROOT

# 我们自带的输入法组件是否在位（判据 = 文件实体，而不是 dpkg 包名）：
#   /usr/share/rime-data             —— Rime 方案与词库
#   /usr/share/fcitx5/addon/rime.conf —— fcitx5 插件描述
#   <libdir>/fcitx5/rime.so           —— fcitx5 插件本体
rime_sync_ime_present() {
    local p
    for p in usr/share/rime-data usr/share/fcitx5/addon/rime.conf; do
        [ -e "$RIME_SYNC_IME_ROOT/$p" ] && return 0
    done
    for p in "$RIME_SYNC_IME_ROOT"/usr/lib/*/fcitx5/rime.so; do
        [ -e "$p" ] && return 0
    done
    return 1
}

dpkg-query() {
    local fmt="" pkg="" arg
    for arg in "$@"; do
        case "$arg" in
            -f=*|--showformat=*) fmt="${arg#*=}" ;;
            -*) ;;
            *) pkg="$arg" ;;
        esac
    done
    case "$pkg" in
        rime-inputmethod|fcitx5-rime) ;;
        *) command dpkg-query "$@"; return $? ;;
    esac
    # 只有我们自带的输入法确实在位时，才替这两个名字「答已安装」。
    if ! rime_sync_ime_present; then
        command dpkg-query "$@"
        return $?
    fi
    case "$fmt" in
        *Status*) printf 'install ok installed\n' ;;
        *Package*) printf '%s\n' "$pkg" ;;
        *) command dpkg-query "$@"; return $? ;;
    esac
    return 0
}
export -f dpkg-query
RIME_SYNC_IME_SHIM_EOF
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/rime-sync-merge.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# ---------------------------------------------------------------- 解包
# extract_deb <deb> <目标目录>：产出 <dir>/DEBIAN（控制文件）与 <dir>/root（数据树）。
# 首选 dpkg-deb -e/-x；CI runner 上 dpkg-deb 偶发抽风时用 ar 兜底（与 prepare-sync-deb.sh 同一套思路）。
extract_deb() { # <deb> <dir>
  local deb=$1 dir=$2
  rm -rf "$dir"; mkdir -p "$dir/DEBIAN" "$dir/root"
  if dpkg-deb -e "$deb" "$dir/DEBIAN" 2>/dev/null && dpkg-deb -x "$deb" "$dir/root" 2>/dev/null; then
    return 0
  fi
  # ar 兜底
  rm -rf "$dir"; mkdir -p "$dir/DEBIAN" "$dir/root" "$dir/ar"
  ( cd "$dir/ar" && ar x "$deb" ) || return 1
  local ctl data
  ctl=$(ls -1 "$dir/ar"/control.tar.* 2>/dev/null | head -n1)
  data=$(ls -1 "$dir/ar"/data.tar.* 2>/dev/null | head -n1)
  [ -n "$ctl" ] && [ -n "$data" ] || return 1
  tar -xf "$ctl" -C "$dir/DEBIAN" || return 1
  tar -xf "$data" -C "$dir/root" || return 1
  return 0
}

say "解包 IME 包：$(basename "$ime_deb")"
extract_deb "$ime_deb" "$tmp/ime" || fail "解不开 IME 包：$ime_deb"
say "解包同步包：$(basename "$sync_deb")"
extract_deb "$sync_deb" "$tmp/sync" || fail "解不开同步包：$sync_deb"

# ---------------------------------------------------------------- 文件树合并
# 合并规则：同一个相对路径在两个包里各有一份 = 冲突（硬错误）。
# 目录与目录重叠是正常的（/usr/share/rime-sync/… 两边都写），不算冲突；
# 目录 vs 文件、文件 vs 文件 才是冲突。
copy_tree() { # <src_root> <label> <dst_root> <conflicts_file>
  local src=$1 label=$2 dst=$3 conflicts=$4 rel anc bad_anc
  while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    local target="$dst/$rel"
    if [ -e "$target" ] || [ -L "$target" ]; then
      # 目标已存在：目录 vs 目录 = 正常合并；其余一律算冲突。
      if [ -d "$target" ] && [ ! -L "$target" ] && [ -d "$src/$rel" ] && [ ! -L "$src/$rel" ]; then
        continue
      fi
      printf '%s\n' "$rel" >>"$conflicts"
      continue
    fi
    # 父路径是文件（一边把目录当文件放，另一边往里塞东西）→ 冲突
    anc=$rel
    bad_anc=0
    while [ -n "$anc" ]; do
      anc=${anc%/*}
      [ -n "$anc" ] || break
      if [ -e "$dst/$anc" ] || [ -L "$dst/$anc" ]; then
        if [ ! -d "$dst/$anc" ] || [ -L "$dst/$anc" ]; then
          printf '%s\n' "$anc" >>"$conflicts"
          bad_anc=1
        fi
      fi
      case "$anc" in */*) ;; *) break ;; esac
    done
    [ "$bad_anc" -eq 0 ] || continue
    # 目录只建目录、不整棵 cp -a（否则子文件会被复制两遍，后面的文件条目就误判成冲突）。
    if [ -d "$src/$rel" ] && [ ! -L "$src/$rel" ]; then
      mkdir -p "$target"
      continue
    fi
    mkdir -p "$(dirname "$target")"
    cp -a "$src/$rel" "$target"
  done < <(cd "$src" && find . -mindepth 1 -print | sed 's|^\./||' | LC_ALL=C sort)
}

merged_root="$tmp/root"
# 打包用的暂存目录：DEBIAN/ 与数据树必须同级（dpkg-deb --build 的目录约定）。
build_root="$tmp/build"
conflicts="$tmp/conflicts.txt"
mkdir -p "$merged_root"
: >"$conflicts"
copy_tree "$tmp/ime/root" "IME 包" "$merged_root" "$conflicts"
copy_tree "$tmp/sync/root" "同步包" "$merged_root" "$conflicts"

if [ -s "$conflicts" ]; then
  echo "!! 两个包的安装路径有冲突，合并中止（不做静默覆盖）：" >&2
  LC_ALL=C sort -u "$conflicts" | sed 's/^/     /' >&2
  exit 1
fi
say "文件树合并完成（无路径冲突）"

# ---------------------------------------------------------------- 输入法探测兼容层（落盘）
# 放在合并后的文件树里，既能被合并后的 postinst 内联使用，也能被诊断页「重试」
# 路径用到的 wrapper source（pkexec 会清空环境，收不到 export -f 的函数）。
mkdir -p "$merged_root/usr/lib/rime-sync"
write_ime_shim "$merged_root/usr/lib/rime-sync/ime-detect-shim.sh"

# ---------------------------------------------------------------- 控制字段合并
# union_field <值...>：逗号分隔的字段取并集去重，保留首次出现的顺序。
union_field() {
  local out="" v one
  for v in "$@"; do
    [ -n "$v" ] || continue
    while IFS= read -r one; do
      one=${one#"${one%%[![:space:]]*}"}
      one=${one%"${one##*[![:space:]]}"}
      [ -n "$one" ] || continue
      case ",$out," in
        *",$one,"*) continue ;;
      esac
      # 内部用 "," 分隔（不带空格），去重判断才对得上；输出时再补空格。
      out="${out:+$out,}$one"
    done <<<"$(printf '%s' "$v" | tr ',' '\n')"
  done
  printf '%s' "$out" | sed 's/,/, /g'
}

field() { # <deb> <字段名>
  dpkg-deb -f "$1" "$2" 2>/dev/null | head -n1 || true
}

ime_dep=$(field "$ime_deb" Depends)
sync_dep=$(field "$sync_deb" Depends)
ime_prov=$(field "$ime_deb" Provides)
sync_prov=$(field "$sync_deb" Provides)
ime_conf=$(field "$ime_deb" Conflicts)
sync_conf=$(field "$sync_deb" Conflicts)
ime_repl=$(field "$ime_deb" Replaces)
sync_repl=$(field "$sync_deb" Replaces)
ime_ver=$(field "$ime_deb" Version)
sync_ver=$(field "$sync_deb" Version)
sync_maint=$(field "$sync_deb" Maintainer)
sync_sect=$(field "$sync_deb" Section)
sync_arch=$(field "$sync_deb" Architecture)
ime_multi=$(field "$ime_deb" Multi-Arch)
sync_multi=$(field "$sync_deb" Multi-Arch)
ime_home=$(field "$ime_deb" Homepage)
sync_home=$(field "$sync_deb" Homepage)

# 旧包名也要 Conflicts / Replaces：装新包时旧的会被替换掉，不会两套并存打架。
old_conf=""
old_repl=""
for p in $OLD_PKGS; do
  old_conf="${old_conf:+$old_conf, }$p"
  old_repl="${old_repl:+$old_repl, }$p"
done

DEPENDS=$(union_field "$ime_dep" "$sync_dep")
PROVIDES=$(union_field "$ime_prov" "$sync_prov")
CONFLICTS=$(union_field "$ime_conf" "$sync_conf" "$old_conf")
REPLACES=$(union_field "$ime_repl" "$sync_repl" "$old_repl")

[ -n "$sync_ver" ] || sync_ver="$ime_ver"
[ -n "$sync_maint" ] || sync_maint=$(field "$ime_deb" Maintainer)
[ -n "$sync_sect" ] || sync_sect=utils
[ -n "$sync_arch" ] || sync_arch=amd64
# Multi-Arch / Homepage：IME 包有就沿用（IME 包一直是 Multi-Arch: same）。
merged_multi=${ime_multi:-$sync_multi}
merged_home=${ime_home:-$sync_home}

# Installed-Size 按合并后的文件树重算（KB）。
size_kb=$(du -sk "$merged_root" | awk '{print $1}')
[ -n "$size_kb" ] || size_kb=0

mkdir -p "$build_root/DEBIAN"
{
  echo "Package: $MERGED_PKG"
  echo "Version: $sync_ver"
  echo "Section: $sync_sect"
  echo "Priority: optional"
  echo "Architecture: $sync_arch"
  echo "Maintainer: $sync_maint"
  echo "Installed-Size: $size_kb"
  [ -n "$merged_home" ] && echo "Homepage: $merged_home"
  [ -n "$merged_multi" ] && echo "Multi-Arch: $merged_multi"
  [ -n "$DEPENDS" ] && echo "Depends: $DEPENDS"
  [ -n "$PROVIDES" ] && echo "Provides: $PROVIDES"
  [ -n "$CONFLICTS" ] && echo "Conflicts: $CONFLICTS"
  [ -n "$REPLACES" ] && echo "Replaces: $REPLACES"
  echo "Description: rime-sync 同步程序与 Rime 输入法（合并包）"
  echo " 装一次即同时得到 Rime 同步程序与自编译的 Rime 输入法（fcitx5-rime）。"
  echo " 本包由 rime-inputmethod 与 rime-sync 合并而来；卸载也是一个整体："
  echo " 移除 rime-sync 会同时移除输入法组件。"
} >"$build_root/DEBIAN/control"

say "控制字段：Depends=[$DEPENDS]"
say "控制字段：Provides=[$PROVIDES]"
say "控制字段：Conflicts=[$CONFLICTS]"
say "控制字段：Replaces=[$REPLACES]"

# ---------------------------------------------------------------- 维护脚本合并
# 两个包的维护脚本都要跑。做法：把原始脚本正文原样内嵌进合并脚本里，
# 运行时各自落到临时文件再 bash 执行（不能用函数包起来 —— 原脚本结尾是 exit 0，
# 会把整个合并脚本一起带走，后一个就跑不到了）。
# 失败处理：一个失败不挡住另一个；最后退出码取最后一个失败的返回值。
gen_merged_ms() { # <输出路径> <说明> <是否注入输入法探测兼容层 0|1> <label:脚本路径>...
  local outp=$1 desc=$2 shim=$3; shift 3
  local label path delim
  {
    echo '#!/bin/bash'
    echo "# 合并后的维护脚本：$desc"
    echo "# 由 packaging/linux/merge-debs.sh 自动生成，内嵌原始脚本正文，请勿手改。"
    echo 'set -u'
    if [ "$shim" = "1" ]; then
      echo '# ---- 合并包专用：输入法「是否已安装」的探测兼容层 ----'
      # 从安装好的 shim 文件取内容，保证「内联版本」与「落盘版本」逐字一致。
      if [ -r "$merged_root/usr/lib/rime-sync/ime-detect-shim.sh" ]; then
        cat "$merged_root/usr/lib/rime-sync/ime-detect-shim.sh"
      else
        write_ime_shim /dev/stdout
      fi
      echo '# ---- 兼容层结束 -------------------------------------------'
    fi
    echo 'MERGED_TMP=$(mktemp -d "${TMPDIR:-/tmp}/rime-sync-ms.XXXXXX") || exit 1'
    echo "trap 'rm -rf \"\$MERGED_TMP\"' EXIT"
    echo 'merged_rc=0'
    for item in "$@"; do
      label=${item%%:*}
      path=${item#*:}
      delim="__RIME_SYNC_EMBED_${label}__"
      if grep -qF "$delim" "$path" 2>/dev/null; then
        echo "!! 原始脚本里出现了嵌入分隔符 $delim，无法自动合并" >&2
        exit 1
      fi
      echo "cat > \"\$MERGED_TMP/$label.sh\" <<'$delim'"
      cat "$path"
      echo
      echo "$delim"
      echo "echo '---- 合并维护脚本：$label 开始 ----'"
      echo "bash \"\$MERGED_TMP/$label.sh\" \"\$@\"; rc=\$?"
      echo '[ "$rc" -eq 0 ] || merged_rc=$rc'
      echo "echo '---- 合并维护脚本：$label 结束（退出码 '\"\$rc\"'）----'"
    done
    if [ "$shim" = "1" ]; then
      # 兜底：诊断页「重试」走的是 pkexec bash <wrapper>，pkexec 会清空环境，
      # 内联的 export -f 传不过去。给生成的 wrapper 补一行 source，
      # 让 wrapper 里的 im_ready() 也能用上兼容层。
      cat <<'RIME_SYNC_WRAPPER_HARDEN_EOF'
# ---- 合并包专用：给 vendor wrapper 补上兼容层（pkexec 路径收不到 export -f）----
RIME_SYNC_WRAPPER=/var/lib/rime-sync/install-vendor-deps-wrapper.sh
if [ -f "$RIME_SYNC_WRAPPER" ] && ! grep -q 'ime-detect-shim' "$RIME_SYNC_WRAPPER" 2>/dev/null; then
    RIME_SYNC_WRAPPER_TMP=$(mktemp "${TMPDIR:-/tmp}/rime-sync-wrapper.XXXXXX") || exit 0
    {
        echo '#!/bin/bash'
        echo '. /usr/lib/rime-sync/ime-detect-shim.sh'
        tail -n +2 "$RIME_SYNC_WRAPPER"
    } >"$RIME_SYNC_WRAPPER_TMP"
    # 用 mv 原子替换：正在运行的 wrapper 仍读旧 inode，不会被改到一半。
    mv -f "$RIME_SYNC_WRAPPER_TMP" "$RIME_SYNC_WRAPPER" 2>/dev/null \
        || { rm -f "$RIME_SYNC_WRAPPER_TMP"; true; }
    chmod 0755 "$RIME_SYNC_WRAPPER" 2>/dev/null || true
fi
# ---- 兜底结束 --------------------------------------------------------------
RIME_SYNC_WRAPPER_HARDEN_EOF
    fi
    echo 'exit "$merged_rc"'
  } >"$outp"
  chmod 0755 "$outp"
}

# postinst：先 IME 的 localepurge 提示，再同步包的 postinst（固定顺序）。
postinst_parts=""
[ -f "$tmp/ime/DEBIAN/postinst" ] && postinst_parts="ime:$tmp/ime/DEBIAN/postinst"
[ -f "$tmp/sync/DEBIAN/postinst" ] && postinst_parts="${postinst_parts:+$postinst_parts }sync:$tmp/sync/DEBIAN/postinst"
if [ -n "$postinst_parts" ]; then
  # shellcheck disable=SC2086
  gen_merged_ms "$build_root/DEBIAN/postinst" "postinst（IME 提示在前，同步包在后）" 1 $postinst_parts
fi

# prerm / postrm：同样两个都跑；顺序按卸载惯例「先同步包、再 IME 包」。
for ms in prerm postrm; do
  parts=""
  [ -f "$tmp/sync/DEBIAN/$ms" ] && parts="sync:$tmp/sync/DEBIAN/$ms"
  [ -f "$tmp/ime/DEBIAN/$ms" ] && parts="${parts:+$parts }ime:$tmp/ime/DEBIAN/$ms"
  if [ -n "$parts" ]; then
    # shellcheck disable=SC2086
    gen_merged_ms "$build_root/DEBIAN/$ms" "$ms（同步包在前，IME 包在后）" 0 $parts
  fi
done

# 同步包的 deb 本来就带 /usr/lib/rime-sync/install-extra-ime.sh（prepare-sync-deb.sh 放的），
# 合并时原样带过去即可；缺失时从仓库补一份。
if [ ! -f "$merged_root/usr/lib/rime-sync/install-extra-ime.sh" ] && [ -f "$HERE/install-extra-ime.sh" ]; then
  mkdir -p "$merged_root/usr/lib/rime-sync"
  install -m 0755 "$HERE/install-extra-ime.sh" "$merged_root/usr/lib/rime-sync/install-extra-ime.sh"
fi

# ---------------------------------------------------------------- 打包
# 数据树定稿后再进暂存目录（维护脚本与 install-extra-ime.sh 都是上面才补的）。
cp -a "$merged_root/." "$build_root/"
find "$merged_root" -type d -exec chmod 0755 {} +
find "$merged_root" -type f -exec chmod 0644 {} +
for d in usr/bin usr/lib/rime-sync; do
  [ -d "$merged_root/$d" ] && find "$merged_root/$d" -type f -exec chmod 0755 {} +
done
find "$build_root/DEBIAN" -type f -exec chmod 0644 {} +
find "$build_root/DEBIAN" -type f \( -name 'post*' -o -name 'pre*' \) -exec chmod 0755 {} +

built=0
if dpkg-deb --root-owner-group -Zxz --build "$build_root" "$out" >/dev/null 2>&1; then
  built=1
else
  echo "!! dpkg-deb --build 失败，改用 ar 兜底打包" >&2
fi
if [ "$built" -ne 1 ]; then
  work="$tmp/ar"; rm -rf "$work"; mkdir -p "$work"
  printf '2.0\n' >"$work/debian-binary"
  tar --owner=root --group=root -czf "$work/control.tar.gz" -C "$build_root/DEBIAN" .
  tar --owner=root --group=root --exclude=./DEBIAN -czf "$work/data.tar.gz" -C "$build_root" .
  rm -f "$out"
  ( cd "$work" && ar r "$out" debian-binary control.tar.gz data.tar.gz ) || fail "ar 兜底打包也失败"
fi

# ---------------------------------------------------------------- 自检
pkg_name=$(dpkg-deb -f "$out" Package 2>/dev/null || true)
[ "$pkg_name" = "$MERGED_PKG" ] || fail "产物包名是 $pkg_name，不是 $MERGED_PKG"
# /usr/bin/rime-sync-cli 只在同步包里存在；同步包带了它，合并后就必须恰好 1 份
# （两份说明两个来源写同一个路径，是打包事故；0 份说明同步包本身就没有，不归合并管）。
sync_cli_count=$(dpkg-deb -c "$sync_deb" | awk '{ p = $6; sub(/^\.\//, "", p); if (p == "usr/bin/rime-sync-cli") c++ } END { print c + 0 }')
if [ "$sync_cli_count" -gt 0 ]; then
  cli_count=$(dpkg-deb -c "$out" | awk '{ p = $6; sub(/^\.\//, "", p); if (p == "usr/bin/rime-sync-cli") c++ } END { print c + 0 }')
  [ "$cli_count" -eq 1 ] || fail "/usr/bin/rime-sync-cli 应恰好 1 份，实际 $cli_count 份"
fi
say "产物：$out（$(stat -c %s "$out") bytes，包名 $pkg_name）"
