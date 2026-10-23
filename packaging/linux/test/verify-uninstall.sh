#!/usr/bin/env bash
# verify-uninstall.sh -- 在容器里安装真实的 rime-inputmethod / rime-sync deb，
# 再跑统一卸载入口，断言：两个包彻底未安装、用户目录保留、桌面项被清理。
#
#   packaging/linux/test/verify-uninstall.sh              # debian:13
#   packaging/linux/test/verify-uninstall.sh debian:12
#
# 说明：测试用的 rime-sync deb 是在已有产物上追加 uninstall.sh 与桌面项重打出来的，
# 等价于 tauri.conf.json 的 deb.files 会做的事（本测试不编译、不打包发布产物）。
# 全程 docker --network none；用 dpkg -i --force-depends 只装包不拉 GUI 运行库，
# 因为这里验证的是卸载行为，不启动应用。
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$HERE/../../.." && pwd)
IMAGE=${1:-debian:13}
case "$IMAGE" in
  debian:12)    DISTRO=debian-12 ;;
  debian:13)    DISTRO=debian-13 ;;
  ubuntu:22.04) DISTRO=ubuntu-22.04 ;;
  ubuntu:24.04) DISTRO=ubuntu-24.04 ;;
  *) echo "!! unsupported image: $IMAGE" >&2; exit 2 ;;
esac

SYNC_SRC=$(ls -1t "$REPO_ROOT/packaging/linux/out/offline/$DISTRO"/rime-sync_*.deb 2>/dev/null | head -n 1 || true)
IME_SRC=$(ls -1t "$REPO_ROOT/build/deb/$DISTRO"/rime-inputmethod_*.deb 2>/dev/null | head -n 1 || true)
[ -n "$SYNC_SRC" ] || { echo "!! missing $DISTRO rime-sync deb" >&2; exit 1; }
[ -n "$IME_SRC" ] || { echo "!! missing $DISTRO rime-inputmethod deb" >&2; exit 1; }
SYNC_SRC=$(cd "$(dirname "$SYNC_SRC")" && pwd)/$(basename "$SYNC_SRC")
IME_SRC=$(cd "$(dirname "$IME_SRC")" && pwd)/$(basename "$IME_SRC")

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rime-uninstall-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

echo "==> 用已有产物重打测试 deb（等价 deb.files 追加文件）"
"$REPO_ROOT/packaging/linux/prepare-sync-deb.sh" "$SYNC_SRC" "$WORK/sync-base.deb" >/dev/null
dpkg-deb -R "$WORK/sync-base.deb" "$WORK/root"
install -Dm0755 "$REPO_ROOT/packaging/linux/uninstall.sh" "$WORK/root/usr/lib/rime-sync/uninstall.sh"
install -Dm0644 "$REPO_ROOT/packaging/linux/rime-sync-uninstall.desktop" \
  "$WORK/root/usr/share/applications/rime-sync-uninstall.desktop"
dpkg-deb --root-owner-group -Zxz --build "$WORK/root" "$WORK/rime-sync-test.deb" >/dev/null
cp -f "$IME_SRC" "$WORK/$(basename "$IME_SRC")"
# 离线包根目录：install.sh + uninstall.sh 平铺（build-offline-installer.sh 的做法）
mkdir -p "$WORK/bundle"
cp -f "$REPO_ROOT/packaging/linux/install.sh" "$REPO_ROOT/packaging/linux/uninstall.sh" "$WORK/bundle/"
echo "    sync=$(basename "$WORK/rime-sync-test.deb")  from $(basename "$SYNC_SRC")"
echo "    ime =$(basename "$IME_SRC")"
echo "    image=$IMAGE"
echo

cat > "$WORK/incontainer.sh" <<'INNER'
set -u
export DEBIAN_FRONTEND=noninteractive
STATE() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true; }
fail() { echo "FAIL: $*"; exit 1; }
ok() { echo "  OK: $*"; }

assert_gone() {
  s=$(STATE "$1")
  case "$s" in
    ""|*not-installed*|*config-files*) ok "$1 不在 dpkg 数据库（state=${s:-无记录}）" ;;
    *) fail "$1 仍处于 $s" ;;
  esac
}
assert_installed() {
  [ "$(STATE "$1")" = "install ok installed" ] || fail "$1 未正确安装（state=$(STATE "$1")）"
  ok "$1 已安装"
}

install_both() {
  echo "--- 安装 rime-inputmethod 与 rime-sync（--force-depends，跳过 GUI 运行库）---"
  dpkg -i --force-depends /debs/rime-inputmethod_*.deb >/tmp/i.log 2>&1 || true
  dpkg -i --force-depends /debs/rime-sync-test.deb >/tmp/s.log 2>&1 || true
  assert_installed rime-inputmethod
  assert_installed rime-sync
  [ -x /usr/lib/rime-sync/uninstall.sh ] || fail "uninstall.sh 未以 0755 安装"
  [ -f /usr/share/applications/rime-sync-uninstall.desktop ] || fail "桌面项未安装"
  ok "uninstall.sh 0755 + 桌面项就位"
}

seed_user_data() {
  mkdir -p /root/.local/share/fcitx5/rime /root/.config/rime-sync /root/.config/autostart
  printf 'user_dict_v1\n' > /root/.local/share/fcitx5/rime/user.yaml
  printf 'webdav: v1\n' > /root/.config/rime-sync/config.json
  printf '[Desktop Entry]\nType=Application\nName=rime-sync\n' > /root/.config/autostart/rime-sync.desktop
}

echo "===== A. 默认卸载：两个包都卸，用户数据保留，桌面项/autostart 清理 ====="
install_both
seed_user_data
SUDO_UID=0 SUDO_USER=root bash /usr/lib/rime-sync/uninstall.sh --yes </dev/null ||
  fail "uninstall.sh --yes 退出码非 0"
assert_gone rime-sync
assert_gone rime-inputmethod
[ -f /root/.local/share/fcitx5/rime/user.yaml ] || fail "Rime 用户词库被删除"
[ "$(cat /root/.local/share/fcitx5/rime/user.yaml)" = "user_dict_v1" ] || fail "用户词库内容被改"
[ -f /root/.config/rime-sync/config.json ] || fail "同步配置被删除"
[ ! -e /root/.config/autostart/rime-sync.desktop ] || fail "用户 autostart 未清理"
if compgen -G '/usr/share/applications/rime-sync*.desktop' >/dev/null; then fail "系统桌面项残留"; fi
if compgen -G '/usr/bin/rime-sync*' >/dev/null; then fail "可执行文件残留"; fi
if compgen -G '/tmp/rime-sync-uninstall.*' >/dev/null; then fail "自我重定位的临时副本未清理"; fi
ok "用户数据保留、桌面项/autostart/临时副本已清理"

echo
echo "===== B. packaging/linux/install.sh --uninstall 复用同一脚本 ====="
install_both
bash /pk-install.sh --uninstall --yes </dev/null >/tmp/install-uninstall.log 2>&1 ||
  fail "install.sh --uninstall 退出码非 0"
grep -q '调用卸载脚本：/usr/lib/rime-sync/uninstall.sh' /tmp/install-uninstall.log || {
  sed -n '1,40p' /tmp/install-uninstall.log
  fail "install.sh 没有委派给统一卸载脚本"
}
grep -q '保留的用户数据' /tmp/install-uninstall.log || fail "未打印保留路径"
echo "  install.sh 输出摘录："
grep -E '调用卸载脚本|保留的用户数据' /tmp/install-uninstall.log | sed 's/^/    /'
assert_gone rime-sync
assert_gone rime-inputmethod
ok "install.sh --uninstall 已委派且输出保留路径"

echo
echo "===== C. 桌面入口 Exec 路径（pkexec 包装，用假 pkexec 验证命令行）====="
install_both
printf '#!/bin/sh\necho "[fake-pkexec] $*" >&2\nexec "$@"\n' > /usr/local/bin/pkexec
chmod 0755 /usr/local/bin/pkexec
EXEC=$(sed -n 's/^Exec=//p' /usr/share/applications/rime-sync-uninstall.desktop)
echo "  Exec=$EXEC"
sh -c "$EXEC" </dev/null >/tmp/exec.log 2>&1 || fail "桌面入口命令行执行失败"
grep -q '\[fake-pkexec\] /usr/lib/rime-sync/uninstall.sh' /tmp/exec.log || {
  cat /tmp/exec.log
  fail "Exec 没有经 pkexec 调用卸载脚本"
}
assert_gone rime-sync
assert_gone rime-inputmethod
ok "桌面入口经 pkexec 调用 /usr/lib/rime-sync/uninstall.sh"

echo
echo "===== D. 非 root 拒绝 + --keep-ime 只卸同步 ====="
install_both
if su -s /bin/sh nobody -c 'bash /usr/lib/rime-sync/uninstall.sh' </dev/null >/tmp/nonroot.log 2>&1; then
  fail "非 root 竟然卸载成功"
fi
grep -q '需要 root 权限' /tmp/nonroot.log || { cat /tmp/nonroot.log; fail "非 root 未提示 pkexec/sudo"; }
ok "非 root 被拒绝并提示 pkexec/sudo"
assert_installed rime-sync
SUDO_UID=0 SUDO_USER=root bash /usr/lib/rime-sync/uninstall.sh --keep-ime --yes </dev/null ||
  fail "--keep-ime 退出码非 0"
assert_gone rime-sync
assert_installed rime-inputmethod
ok "--keep-ime 只卸 rime-sync、保留 rime-inputmethod"
install_both
bash /pk-install.sh --uninstall --keep-ime --yes </dev/null >/tmp/keep-ime.log 2>&1 ||
  fail "install.sh --uninstall --keep-ime 失败"
assert_gone rime-sync
assert_installed rime-inputmethod
ok "install.sh --keep-ime 已转发到统一脚本"

echo
echo "===== E. 只剩输入法时：离线包同目录脚本仍能卸载（回退路径）====="
assert_installed rime-inputmethod
if [ -e /usr/lib/rime-sync/uninstall.sh ]; then fail "rime-sync 已卸，安装路径的脚本不该还在"; fi
bash /b/install.sh --uninstall --yes </dev/null >/tmp/only-ime.log 2>&1 ||
  fail "只装输入法时 install.sh --uninstall 失败"
grep -q '调用卸载脚本：/b/uninstall.sh' /tmp/only-ime.log || {
  sed -n '1,40p' /tmp/only-ime.log
  fail "没有回退到离线包同目录的 uninstall.sh"
}
grep -q 'rime-sync：未安装，跳过' /tmp/only-ime.log || fail "未报告 rime-sync 未安装"
echo "  install.sh 输出摘录："
grep -E '调用卸载脚本|未安装，跳过' /tmp/only-ime.log | sed 's/^/    /'
assert_gone rime-inputmethod
ok "没有 rime-sync 时仍能卸掉 rime-inputmethod"

echo
echo "===== F. dpkg 卸载失败：打印原因、继续卸下一个、退出码 1 ====="
install_both
mkdir -p /tmp/dep/DEBIAN
printf '%s\n' 'Package: rime-sync-dep' 'Version: 1.0' 'Architecture: all' \
  'Maintainer: test <test@example.com>' 'Depends: rime-sync' \
  'Description: fake reverse dependency for uninstall test' > /tmp/dep/DEBIAN/control
dpkg-deb --root-owner-group --build /tmp/dep /tmp/rime-sync-dep.deb >/dev/null
dpkg -i --force-depends /tmp/rime-sync-dep.deb >/dev/null 2>&1 || fail "无法安装测试用的反向依赖包"
if SUDO_UID=0 SUDO_USER=root bash /usr/lib/rime-sync/uninstall.sh --yes </dev/null >/tmp/fail.log 2>&1; then
  fail "rime-sync 被反向依赖挡住，卸载却返回 0"
fi
grep -q 'dependency problems' /tmp/fail.log || { cat /tmp/fail.log; fail "没有打印 dpkg 给出的失败原因"; }
grep -q 'rime-sync 卸载失败' /tmp/fail.log || fail "没有报告 rime-sync 卸载失败"
# 反向依赖挡住时 dpkg 把 want 状态置为 deinstall，但文件仍在（dependency problems - not removing）
[ -x /usr/bin/rime-sync-app ] || fail "卸载失败却把 rime-sync 的文件删了"
case "$(STATE rime-sync)" in
  *installed*) ok "rime-sync 仍在（state=$(STATE rime-sync)）" ;;
  *) fail "rime-sync 状态异常：$(STATE rime-sync)" ;;
esac
assert_gone rime-inputmethod
echo "  失败输出摘录："
grep -E 'dependency problems|卸载失败' /tmp/fail.log | sed 's/^/    /'
ok "失败原因已打印，且继续卸掉了 rime-inputmethod"
dpkg -P rime-sync-dep >/dev/null 2>&1 || true
dpkg -P rime-sync >/dev/null 2>&1 || true

echo
echo "===== 最终 dpkg 状态 ====="
for p in rime-sync rime-inputmethod; do
  dpkg-query -W -f='${Package} ${Version} [${Status}]\n' "$p" 2>/dev/null ||
    echo "$p [无 dpkg 记录]"
done
echo
echo "===== PASS ====="
INNER
chmod 0755 "$WORK/incontainer.sh"

docker run --rm --network none \
  -v "$WORK:/debs:ro" \
  -v "$REPO_ROOT/packaging/linux/install.sh:/pk-install.sh:ro" \
  -v "$WORK/bundle:/b:ro" \
  "$IMAGE" bash /debs/incontainer.sh
