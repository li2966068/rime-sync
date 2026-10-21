#!/bin/bash
# Runs INSIDE the target container AFTER /bundle/install.sh succeeded.
# Collects the acceptance evidence: package status, schema data, librime probe,
# and fcitx5 loading the rime addon under Xvfb.
set -u

fail() { echo "FAIL: $*"; exit 1; }

echo "===== os ====="
. /etc/os-release
echo "${PRETTY_NAME}"

echo "===== packages ====="
for p in rime-inputmethod rime-sync fcitx5; do
  dpkg-query -W -f='${Package} ${Version} [${Status}]\n' "$p" 2>/dev/null ||
    echo "$p [not installed]"
done
dpkg -s rime-inputmethod | grep -E '^(Package|Version|Depends|Conflicts|Provides):'
dpkg -s rime-sync | grep -E '^(Package|Version|Depends):'

echo "===== rime data ====="
echo "files=$(find /usr/share/rime-data -maxdepth 1 -type f | wc -l)"
ls /usr/share/rime-data | sort | head -30
test -f /usr/share/rime-data/default.yaml || fail "default.yaml missing"
test -f /usr/share/rime-data/essay.txt || fail "essay.txt missing"
test -f /usr/share/rime-data/luna_pinyin.schema.yaml || fail "luna_pinyin schema missing"

echo "===== plugin ====="
ls -l /usr/lib/x86_64-linux-gnu/fcitx5/rime.so
ls -l /usr/lib/x86_64-linux-gnu/librime.so.1.* 2>/dev/null | head -3
ldd /usr/lib/x86_64-linux-gnu/fcitx5/rime.so | grep -c 'not found' | sed 's/^/ldd_not_found=/'

echo "===== rime_deployer ====="
command -v rime_deployer
rime_deployer 2>&1 | head -5   # no arguments prints usage and exits 0
rm -rf /tmp/rime-deployer-user
if rime_deployer --build /tmp/rime-deployer-user /usr/share/rime-data \
     >/tmp/rime-deployer.log 2>&1; then
  echo "deployer_build=OK"
else
  echo "deployer_build=FAILED"; tail -n 20 /tmp/rime-deployer.log
  fail "rime_deployer --build failed"
fi
echo "deployer_tables=$(find /tmp/rime-deployer-user/build -name '*.bin' 2>/dev/null | wc -l)"

echo "===== librime decompose/candidate probe ====="
rm -rf /tmp/rime-probe-user
/probe/rime-smoke /usr/share/rime-data /tmp/rime-probe-user || fail "rime-smoke probe"

echo "===== fcitx5 addon load under Xvfb ====="
export DISPLAY=:99
Xvfb :99 -screen 0 640x480x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XPID=$!
sleep 2
mkdir -p /root/.config/fcitx5
cat >/root/.config/fcitx5/profile <<'EOF'
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=rime

[Groups/0/Items/0]
Name=rime
Layout=

[GroupOrder]
0=Default
EOF
timeout 90 dbus-run-session -- fcitx5 --replace --verbose "*=5" >/tmp/fcitx5.log 2>&1 || true
kill "$XPID" 2>/dev/null || true
grep -nE 'Loaded addon rime|deploy (success|failure)|failed to load rime|undefined symbol' /tmp/fcitx5.log || true
grep -q 'Loaded addon rime' /tmp/fcitx5.log || fail "fcitx5 did not load addon rime"
if grep -q 'deploy failure' /tmp/fcitx5.log; then fail "rime deploy failure in fcitx5"; fi
echo "deploy_tables=$(find /root/.local/share/fcitx5/rime/build -name '*.bin' 2>/dev/null | wc -l)"
find /root/.local/share/fcitx5/rime/build -name '*.bin' 2>/dev/null | head -5

echo "===== install order (installer log) ====="
cat /var/log/rime-sync/install-steps.log 2>/dev/null || echo "no install-steps.log"
echo "===== install order (dpkg log) ====="
grep -E ' install rime-(inputmethod|sync)' /var/log/dpkg.log 2>/dev/null | tail -10 || true

echo "===== PASS ====="
