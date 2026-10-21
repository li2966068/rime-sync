#!/usr/bin/env bash
# 容器内验证：
#   1) rimesync.so / rimesync.conf 装到了 fcitx5 的搜索路径，ldd 无 not found；
#   2) Xvfb + dbus 下启动 fcitx5，日志里 addon 被加载且无 error；
#   3) 通过 fcitx5-configtool 用的同一个 DBus 接口（GetConfig/SetConfig）证明配置项可读可写，
#      写回 ~/.config/rime-sync/config.json（0600、未知键保留、URL 补尾斜杠）。
set -uo pipefail

OUT=${OUT:-/out}
PREFIX=${PREFIX:-/usr}
LIBDIR=${LIBDIR:-lib/$(dpkg-architecture -qDEB_HOST_MULTIARCH)}
SO="$PREFIX/$LIBDIR/fcitx5/rimesync.so"
CONF="$PREFIX/share/fcitx5/addon/rimesync.conf"
HOME_DIR=${SMOKE_HOME:-/root}
CFG="$HOME_DIR/.config/rime-sync/config.json"
DISPLAY_NUM=${SMOKE_DISPLAY:-:97}
FAILED=0

mkdir -p "$OUT"
ok() { printf '  ok   %s\n' "$*"; }
bad() { printf '  FAIL %s\n' "$*"; FAILED=1; }

echo "== 安装路径"
[ -f "$SO" ] && ok "$SO" || bad "missing $SO"
[ -f "$CONF" ] && ok "$CONF" || bad "missing $CONF"
[ -x "$PREFIX/bin/rime-sync-open-setup" ] && ok "$PREFIX/bin/rime-sync-open-setup (action target)" \
    || bad "missing $PREFIX/bin/rime-sync-open-setup"

echo "== addon 元数据"
grep -E '^(Library|Type|Configurable|OnDemand)=' "$CONF" 2>/dev/null | sed 's/^/  /'
grep -qx 'Configurable=True' "$CONF" 2>/dev/null && ok "Configurable=True" || bad "Configurable is not True"

echo "== ldd"
ldd "$SO" > "$OUT/ldd-rimesync.so.txt" 2>&1
if grep -q 'not found' "$OUT/ldd-rimesync.so.txt"; then
  bad "unresolved shared libraries"
  sed 's/^/  /' "$OUT/ldd-rimesync.so.txt"
else
  ok "no 'not found' in ldd"
fi

echo "== 预置主程序格式的 config.json"
mkdir -p "$(dirname "$CFG")"
cat > "$CFG" <<'JSON'
{
  "schema_version": 2,
  "webdav": {
    "url": "https://dav.example.com/dav/",
    "username": "seed-user",
    "remote_dir": "seed-dir"
  },
  "device_id": "seed-device",
  "sync_kinds": { "userdict": true, "custom_yaml": true, "dict_sources": true },
  "stop_fcitx5_during_apply": true
}
JSON
chmod 600 "$CFG"
ok "$CFG"

export HOME="$HOME_DIR"
export LANG=C.UTF-8
export LC_ALL=C.UTF-8
export XDG_CONFIG_HOME="$HOME_DIR/.config"
export XDG_RUNTIME_DIR=/tmp/rimesync-xdg-runtime
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
export DISPLAY="$DISPLAY_NUM"
rm -f "/tmp/.X${DISPLAY_NUM#:}-lock"

Xvfb "$DISPLAY_NUM" -screen 0 1280x800x24 >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!
sleep 1

cat > /tmp/rimesync-inner.sh <<'INNER'
set -u
fcitx5 --verbose default=4 >/tmp/fcitx5.log 2>&1 &
FCITX_PID=$!
trap 'kill $FCITX_PID 2>/dev/null' EXIT

for _ in $(seq 1 80); do
  gdbus introspect --session --dest org.fcitx.Fcitx5 --object-path /controller >/dev/null 2>&1 && break
  sleep 0.25
done

gdbus call --session --dest org.fcitx.Fcitx5 --object-path /controller \
  --method org.fcitx.Fcitx.Controller1.GetConfig 'fcitx://config/addon/rimesync' \
  > /tmp/getconfig.txt 2>&1
echo "GetConfig rc=$?"

gdbus call --session --dest org.fcitx.Fcitx5 --object-path /controller \
  --method org.fcitx.Fcitx.Controller1.SetConfig 'fcitx://config/addon/rimesync' \
  "<{'WebDAVURL': <'https://dav.example.org/dav'>, 'WebDAVUsername': <'page-user'>, 'WebDAVPassword': <'page-secret'>, 'RemoteDir': <'page-dir'>, 'DeviceId': <'page-device'>, 'StopFcitx5DuringApply': <'False'>}>" \
  > /tmp/setconfig.txt 2>&1
echo "SetConfig rc=$?"
sleep 1
# 显式退出，保证日志落盘后再做断言
kill $FCITX_PID 2>/dev/null
wait $FCITX_PID 2>/dev/null
INNER

dbus-run-session -- bash /tmp/rimesync-inner.sh
kill "$XVFB_PID" 2>/dev/null
wait "$XVFB_PID" 2>/dev/null

echo "== fcitx5 日志（addon 加载）"
grep -iE 'rimesync' /tmp/fcitx5.log | sed 's/^/  /' | head -20
grep -q 'Loaded addon rimesync' /tmp/fcitx5.log && ok "addon loaded" || bad "no 'Loaded addon rimesync' in log"
if grep -qE 'Could not load addon rimesync|Failed to load addon' /tmp/fcitx5.log; then
  bad "addon load error in log"
fi

echo "== GetConfig（fcitx5-configtool 打开配置页用的同一接口）"
sed 's/^/  /' /tmp/getconfig.txt | head -5
cp /tmp/getconfig.txt "$OUT/getconfig.txt" 2>/dev/null
grep -q 'WebDAVURL' /tmp/getconfig.txt && ok "option WebDAVURL exported" || bad "WebDAVURL missing"
grep -q 'seed-user' /tmp/getconfig.txt && ok "value read from config.json (seed-user)" || bad "seed value not read"
grep -q 'External' /tmp/getconfig.txt && ok "External (=action button) option exported" || bad "External option missing"
grep -q 'rime-sync-open-setup' /tmp/getconfig.txt && ok "action targets rime-sync-open-setup" || bad "action URI missing"

echo "== SetConfig → config.json"
sed 's/^/  /' /tmp/setconfig.txt | head -5
cp /tmp/setconfig.txt "$OUT/setconfig.txt" 2>/dev/null
mode=$(stat -c %a "$CFG" 2>/dev/null)
[ "$mode" = "600" ] && ok "config.json mode 0600" || bad "config.json mode=${mode:-missing}"
for needle in page-user page-dir page-device; do
  grep -q "\"$needle\"" "$CFG" && ok "written: $needle" || bad "not written: $needle"
done
grep -q '"stop_fcitx5_during_apply": false' "$CFG" && ok "boolean written: stop_fcitx5_during_apply=false" \
  || bad "boolean not written"
grep -q '"schema_version": 2' "$CFG" && ok "unknown key schema_version preserved" || bad "schema_version lost"
grep -q 'sync_kinds' "$CFG" && ok "unknown key sync_kinds preserved" || bad "sync_kinds lost"
grep -q 'https://dav.example.org/dav/' "$CFG" && ok "url normalized with trailing slash" || bad "url not normalized"

echo "== 密码不落 fcitx 配置（skipSave）"
FCITX_CONF="$HOME_DIR/.config/fcitx5/conf/rimesync.conf"
if [ -f "$FCITX_CONF" ]; then
  if grep -q 'page-secret' "$FCITX_CONF"; then bad "password leaked into $FCITX_CONF"; else ok "no password in $FCITX_CONF"; fi
else
  ok "fcitx never wrote conf/rimesync.conf (password option is skipSave)"
fi

echo
if [ "$FAILED" = 0 ]; then echo "SMOKE PASS"; else echo "SMOKE FAIL"; fi
exit "$FAILED"
