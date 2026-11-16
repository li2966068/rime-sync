#!/bin/bash
# merge-debs.sh（把 rime-inputmethod 与 rime-sync 合成一个 deb）的回归测试。
#
# 全部用「手工造的最小假 deb」跑：不需要 root、不需要 docker、不需要联网，
# 也不需要真的编译 librime（假包里只放几个占位文件）。
#
# 断言九件事：
#   1. Depends 取并集且不重复；
#   2. Provides 保留 fcitx5-rime；
#   3. Conflicts 同时含发行版包名（fcitx5-rime）与旧包名（rime-inputmethod / rime-sync）；
#   4. 两个包的维护脚本都会执行（假 postinst 各写一个标记文件）；
#   5. 两个包装到同一个路径时合并报错退出，不静默覆盖；
#   6. 合并产物里 /usr/bin/rime-sync-cli 恰好 1 份；
#   7. 合并产物带「输入法探测兼容层」（/usr/lib/rime-sync/ime-detect-shim.sh），
#      且它在输入法在位时把 rime-inputmethod / fcitx5-rime 回答成「已安装」，
#      其余包名照旧转交真正的 dpkg-query；
#   8. 合并后的 postinst 内联了同一份兼容层；
#   9. 端到端：拿真实 postinst 的 rime_im_installed() 在「模拟没装旧包的机器」上跑，
#      有兼容层 ⇒ 判「已安装」（不会去 apt 装发行版 fcitx5-rime 把合并包自己卸掉），
#      不加载兼容层 ⇒ 判「未安装」（对照组，证明兼容层确实是关键补丁）。
#
# 用法：bash packaging/linux/test/test-merge-debs.sh
set -u

here=$(cd "$(dirname "$0")" && pwd)
merge_sh=$here/../merge-debs.sh
tmp=$(mktemp -d "${TMPDIR:-/tmp}/test-merge-debs.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT

if [ ! -f "$merge_sh" ]; then
    echo "找不到 $merge_sh" >&2
    exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "需要 dpkg-deb" >&2
    exit 1
fi

pass=0
fail=0
ok() { pass=$((pass + 1)); echo "  OK   $1"; }
ng() { fail=$((fail + 1)); echo "  FAIL $1"; }

marker=$tmp/marker.txt
: >"$marker"

# 造一个最小 deb：$1=包名 $2=版本 $3=额外 control 字段（每行一个） $4=要放的文件（空格分隔的相对路径）
make_deb() { # <包名> <版本> <额外字段> <文件...>
    local pkg=$1 ver=$2 extra=$3; shift 3
    local root="$tmp/build-$pkg-$ver"
    rm -rf "$root"
    mkdir -p "$root/DEBIAN"
    {
        echo "Package: $pkg"
        echo "Version: $ver"
        echo "Section: utils"
        echo "Priority: optional"
        echo "Architecture: amd64"
        echo "Maintainer: rime-sync <dev@rimesync.invalid>"
        echo "Installed-Size: 1"
        [ -n "$extra" ] && printf '%s\n' "$extra"
        echo "Description: $pkg 测试包"
        echo " 手工造的最小假包，只用于 merge-debs.sh 的回归测试。"
    } >"$root/DEBIAN/control"
    # 维护脚本：写标记文件，用来验证「两个包的脚本都跑了」
    {
        echo "#!/bin/bash"
        echo "echo \"$pkg-postinst\" >> \"$marker\""
        echo "exit 0"
    } >"$root/DEBIAN/postinst"
    {
        echo "#!/bin/bash"
        echo "echo \"$pkg-prerm\" >> \"$marker\""
        echo "exit 0"
    } >"$root/DEBIAN/prerm"
    {
        echo "#!/bin/bash"
        echo "echo \"$pkg-postrm\" >> \"$marker\""
        echo "exit 0"
    } >"$root/DEBIAN/postrm"
    chmod 0755 "$root/DEBIAN/postinst" "$root/DEBIAN/prerm" "$root/DEBIAN/postrm"
    local f
    for f in "$@"; do
        mkdir -p "$root/$(dirname "$f")"
        echo "content-of-$f" >"$root/$f"
        chmod 0755 "$root/$f"
    done
    dpkg-deb --root-owner-group --build "$root" "$tmp/$pkg.deb" >/dev/null
}

count_field() { # <deb> <字段> <值>：该值在字段里出现几次
    dpkg-deb -f "$1" "$2" 2>/dev/null | tr ',' '\n' | sed 's/^ *//; s/ *$//' \
        | grep -cx -- "$3" || true
}

has_field() { # <deb> <字段> <值>
    [ "$(count_field "$1" "$2" "$3")" -gt 0 ]
}

echo "== 造包 =="
make_deb rime-inputmethod 0.6.30~trixie \
    "Depends: libc6, libstdc++6
Provides: fcitx5-rime
Conflicts: fcitx5-rime, librime-data
Replaces: fcitx5-rime, librime-data" \
    usr/bin/rime-sync-open-setup usr/share/rime-data/luna_pinyin.schema.yaml
make_deb rime-sync 0.6.32 \
    "Depends: libc6, libssl3" \
    usr/bin/rime-sync-cli usr/bin/rime-sync-app usr/share/rime-sync/defaults/default.custom.yaml

echo "== 合并 =="
out=$tmp/merged.deb
if ! bash "$merge_sh" "$tmp/rime-inputmethod.deb" "$tmp/rime-sync.deb" "$out" >"$tmp/merge.log" 2>&1; then
    ng "merge-debs.sh 执行失败"
    sed 's/^/      /' "$tmp/merge.log"
    echo "通过 $pass 项，失败 $fail 项"
    exit 1
fi
ok "merge-debs.sh 合并成功"

if [ -f "$out" ]; then ok "产物已生成"; else ng "产物没有生成"; fi

pkg=$(dpkg-deb -f "$out" Package 2>/dev/null)
if [ "$pkg" = "rime-sync" ]; then ok "包名是 rime-sync"; else ng "包名是 $pkg，不是 rime-sync"; fi

# 1. Depends 并集且不重复
if has_field "$out" Depends libc6 && has_field "$out" Depends libstdc++6 && has_field "$out" Depends libssl3; then
    ok "Depends 含两个包的依赖"
else
    ng "Depends 缺少某个包的依赖：$(dpkg-deb -f "$out" Depends 2>/dev/null)"
fi
n=$(count_field "$out" Depends libc6)
if [ "$n" -eq 1 ]; then ok "Depends 里 libc6 只出现 1 次"; else ng "Depends 里 libc6 出现了 $n 次"; fi
n=$(count_field "$out" Depends libstdc++6)
if [ "$n" -eq 1 ]; then ok "Depends 里 libstdc++6 只出现 1 次"; else ng "Depends 里 libstdc++6 出现了 $n 次"; fi

# 2. Provides
if has_field "$out" Provides fcitx5-rime; then
    ok "Provides 保留 fcitx5-rime"
else
    ng "Provides 里没有 fcitx5-rime：$(dpkg-deb -f "$out" Provides 2>/dev/null)"
fi

# 3. Conflicts：发行版包名 + 两个旧包名
for c in fcitx5-rime librime-data rime-inputmethod rime-sync; do
    if has_field "$out" Conflicts "$c"; then
        ok "Conflicts 含 $c"
    else
        ng "Conflicts 里没有 $c：$(dpkg-deb -f "$out" Conflicts 2>/dev/null)"
    fi
done
for c in rime-inputmethod rime-sync; do
    if has_field "$out" Replaces "$c"; then
        ok "Replaces 含 $c"
    else
        ng "Replaces 里没有 $c：$(dpkg-deb -f "$out" Replaces 2>/dev/null)"
    fi
done

# 4. 两个包的维护脚本都执行
: >"$marker"
dpkg-deb -e "$out" "$tmp/ms" >/dev/null 2>&1
if bash "$tmp/ms/postinst" configure >/dev/null 2>&1; then
    ok "合并后的 postinst 执行成功"
else
    ng "合并后的 postinst 执行失败"
fi
if grep -q '^rime-inputmethod-postinst$' "$marker"; then
    ok "IME 包的 postinst 执行了"
else
    ng "IME 包的 postinst 没有执行（标记：$(tr '\n' ' ' <"$marker")）"
fi
if grep -q '^rime-sync-postinst$' "$marker"; then
    ok "同步包的 postinst 执行了"
else
    ng "同步包的 postinst 没有执行（标记：$(tr '\n' ' ' <"$marker")）"
fi
: >"$marker"
bash "$tmp/ms/prerm" remove >/dev/null 2>&1
if grep -q '^rime-inputmethod-prerm$' "$marker" || grep -q '^rime-sync-prerm$' "$marker"; then
    ok "prerm 至少跑了一个包的脚本"
else
    ng "prerm 没有执行任何包的脚本"
fi

# 5. 路径冲突必须报错，不静默覆盖
make_deb rime-conflict-a 1.0 "" usr/share/rime-sync/same.txt
make_deb rime-conflict-b 1.0 "" usr/share/rime-sync/same.txt
conflict_out=$tmp/conflict.deb
rm -f "$conflict_out"
if bash "$merge_sh" "$tmp/rime-conflict-a.deb" "$tmp/rime-conflict-b.deb" "$conflict_out" >"$tmp/conflict.log" 2>&1; then
    ng "两个包装同一路径时合并居然成功了（应当报错退出）"
else
    ok "两个包装同一路径时合并报错退出"
fi
if grep -q '冲突' "$tmp/conflict.log"; then
    ok "冲突错误信息里点明了「冲突」"
else
    ng "冲突错误信息里没提「冲突」：$(tr '\n' ' ' <"$tmp/conflict.log")"
fi
if [ -f "$conflict_out" ]; then
    ng "冲突时仍然产出了 deb（不该产出）"
else
    ok "冲突时没有产出 deb"
fi

# 6. /usr/bin/rime-sync-cli 恰好 1 份
cli=$(dpkg-deb -c "$out" | awk '{ p = $6; sub(/^\.\//, "", p); if (p == "usr/bin/rime-sync-cli") c++ } END { print c + 0 }')
if [ "$cli" -eq 1 ]; then
    ok "合并产物里 /usr/bin/rime-sync-cli 恰好 1 份"
else
    ng "合并产物里 /usr/bin/rime-sync-cli 有 $cli 份"
fi

# 7. 输入法探测兼容层（合并后必须还能回答「输入法已安装」）
#    合并之后包名变成 rime-sync，fcitx5-rime 又是虚包，dpkg-query 查不到；
#    packaging/linux/postinst 里的 rime_im_installed() 会误判为「没装」，
#    从而去 apt 装发行版 fcitx5-rime —— 而本包 Conflicts: fcitx5-rime，
#    apt 会顺手把本包自己卸掉。兼容层就是来解决这个的。
shim_rel=usr/lib/rime-sync/ime-detect-shim.sh
shim_count=$(dpkg-deb -c "$out" | awk -v p="$shim_rel" '{ x = $6; sub(/^\.\//, "", x); if (x == p) c++ } END { print c + 0 }')
if [ "$shim_count" -eq 1 ]; then
    ok "合并产物里带了输入法探测兼容层 $shim_rel"
else
    ng "合并产物里 $shim_rel 有 $shim_count 份（应为 1）"
fi

dpkg-deb -x "$out" "$tmp/x" >/dev/null 2>&1
shim=$tmp/x/$shim_rel
if [ -r "$shim" ]; then
    ok "兼容层文件可读"
    # 假 payload 根：模拟「输入法组件确实在位」
    fake_root=$tmp/fakeroot
    mkdir -p "$fake_root/usr/share/rime-data"
    echo "x" >"$fake_root/usr/share/rime-data/luna_pinyin.schema.yaml"
    # 空根：模拟「输入法组件不在位」
    empty_root=$tmp/emptyroot
    mkdir -p "$empty_root"

    st=$(RIME_SYNC_IME_ROOT="$fake_root" bash -c ". '$shim'; dpkg-query -W -f='\${Status}' rime-inputmethod 2>/dev/null" 2>/dev/null || true)
    if [ "$st" = "install ok installed" ]; then
        ok "兼容层：输入法在位时 rime-inputmethod 报「已安装」"
    else
        ng "兼容层：rime-inputmethod 报的是「$st」，不是 install ok installed"
    fi

    st=$(RIME_SYNC_IME_ROOT="$fake_root" bash -c ". '$shim'; dpkg-query -W -f='\${Status}' fcitx5-rime 2>/dev/null" 2>/dev/null || true)
    if [ "$st" = "install ok installed" ]; then
        ok "兼容层：输入法在位时 fcitx5-rime 报「已安装」"
    else
        ng "兼容层：fcitx5-rime 报的是「$st」，不是 install ok installed"
    fi

    # 无关包名必须照常委托给真的 dpkg-query（不能被兼容层误答）
    st=$(RIME_SYNC_IME_ROOT="$fake_root" bash -c ". '$shim'; dpkg-query -W -f='\${Status}' rime-sync-nosuchpkg-xyz 2>/dev/null" 2>/dev/null || true)
    if [ "$st" != "install ok installed" ]; then
        ok "兼容层：无关包名仍交给真的 dpkg-query（未被误答为已安装）"
    else
        ng "兼容层把无关包名也误答成已安装了"
    fi

    # 输入法不在位时，兼容层必须原样退回真的 dpkg-query 的回答（不能自己编一个）
    st_shim=$(RIME_SYNC_IME_ROOT="$empty_root" bash -c ". '$shim'; dpkg-query -W -f='\${Status}' rime-inputmethod 2>/dev/null" 2>/dev/null || true)
    st_real=$(dpkg-query -W -f='${Status}' rime-inputmethod 2>/dev/null || true)
    if [ "$st_shim" = "$st_real" ]; then
        ok "兼容层：输入法不在位时退回真的 dpkg-query 回答（不自己编造）"
    else
        ng "输入法不在位时兼容层回答「$st_shim」，真的 dpkg-query 回答「$st_real」"
    fi
else
    ng "兼容层文件不存在或不可读：$shim"
fi

# 8. 合并后的 postinst 必须内联兼容层（覆盖安装时的 postinst 自己）
if grep -q 'ime-detect-shim' "$tmp/ms/postinst" 2>/dev/null && grep -q '输入法「是否已安装」的探测兼容层' "$tmp/ms/postinst" 2>/dev/null; then
    ok "合并后的 postinst 内联了输入法探测兼容层"
else
    ng "合并后的 postinst 里没有兼容层"
fi

# 9. 端到端：拿「真实 postinst 的探测函数」在「模拟一台没装旧包的机器」上跑
#    假 dpkg-query 一律回答 not-installed（模拟旧包已被卸、fcitx5-rime 是查不到的虚包），
#    有兼容层时必须判成「已安装」（否则 postinst 会去 apt 装发行版 fcitx5-rime，
#    而合并包 Conflicts: fcitx5-rime ⇒ apt 会把合并包自己卸掉）。
fakebin=$tmp/fakebin
mkdir -p "$fakebin"
cat >"$fakebin/dpkg-query" <<'FAKE_EOF'
#!/bin/bash
# 模拟「机器上没有 rime-inputmethod，fcitx5-rime 是 dpkg 查不到的虚包」
printf 'unknown ok not-installed\n'
exit 0
FAKE_EOF
chmod 0755 "$fakebin/dpkg-query"

postinst_sh=$here/../postinst
if [ -r "$postinst_sh" ] && [ -r "$shim" ]; then
    # 抽出真实 postinst 里的 rime_im_installed() 函数体
    awk '/^rime_im_installed\(\) \{/,/^\}/' "$postinst_sh" >"$tmp/fn.sh"
    if [ -s "$tmp/fn.sh" ]; then
        PATH="$fakebin:$PATH" RIME_SYNC_IME_ROOT="$fake_root" bash -c "
            . '$shim'
            . '$tmp/fn.sh'
            rime_im_installed
        " >/dev/null 2>&1
        if [ $? -eq 0 ]; then
            ok "有兼容层时，真实 postinst 的 rime_im_installed() 判成「已安装」"
        else
            ng "有兼容层时，真实 postinst 的 rime_im_installed() 仍判成「未安装」"
        fi
        # 对照：不加载兼容层时应当判成「未安装」（这正是合并引入的回归）
        PATH="$fakebin:$PATH" bash -c "
            . '$tmp/fn.sh'
            rime_im_installed
        " >/dev/null 2>&1
        if [ $? -ne 0 ]; then
            ok "对照：不加载兼容层时判成「未安装」（说明兼容层确实是关键补丁）"
        else
            ng "对照组异常：不加载兼容层也判成已安装，测试环境不可信"
        fi
    else
        ng "没能从 $postinst_sh 里抽出 rime_im_installed()"
    fi
else
    ng "找不到 $postinst_sh 或兼容层文件"
fi

echo "通过 $pass 项，失败 $fail 项"
[ "$fail" -eq 0 ]
