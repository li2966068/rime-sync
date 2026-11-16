#!/usr/bin/env bash
# 断言：桌面程序的 default feature 包含 custom-protocol。
#
# 为什么要有这道关：
# Tauri v2 不带 custom-protocol feature 时是 **dev 模式** —— 前端产物不打包进程序，
# 程序运行时去 devUrl（http://localhost:1420）现场取；那台机器上没开 vite 的话，
# 界面就是一片白（2026-11-16 踩过，还一度误判成自己代码的问题）。
#
# 发布链用 `pnpm tauri build`（tauri-cli 自己会加 feature），所以发布产物一直是对的；
# 但本地任何人用 `cargo build --release` 构桌面程序就会踩坑。src-tauri/Cargo.toml 的
# `default = ["custom-protocol"]` 就是让「正确的做法」成为默认，这条断言钉住它。
#
# 用法：
#   assert-desktop-default-feature.sh [Cargo.toml 路径]
#   assert-desktop-default-feature.sh --selftest     用合成文件证明双向有效
set -euo pipefail

check() {
  local f=${1:?}

  if [ ! -f "$f" ]; then
    echo "::error::找不到 $f"
    return 1
  fi

  # 取 [features] 段里的 default = [...] 行
  local default_line
  default_line=$(awk '/^\[features\]/{in_f=1; next} /^\[/{in_f=0} in_f && /^default[[:space:]]*=/{print; exit}' "$f")
  if [ -z "$default_line" ]; then
    echo "::error::$f 的 [features] 段没有 default 行"
    echo "::error::桌面程序会用 dev 模式构建（界面不打包，运行时白屏）"
    return 1
  fi
  echo "found: $default_line"

  case "$default_line" in
    *custom-protocol*) : ;;
    *)
      echo "::error::default 不含 custom-protocol：cargo build --release 出来的桌面程序会是 dev 模式（白屏）"
      return 1
      ;;
  esac

  # custom-protocol 必须真的转发给 tauri，否则写了也没用
  if ! grep -qE '^custom-protocol[[:space:]]*=[[:space:]]*\["tauri/custom-protocol"\]' "$f"; then
    echo "::error::custom-protocol 没有转发到 tauri/custom-protocol"
    return 1
  fi

  echo "ok：default 含 custom-protocol，且转发到 tauri/custom-protocol"
}

if [ "${1:-}" = "--selftest" ]; then
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT

  cat > "$work/good.toml" <<'EOF'
[features]
default = ["custom-protocol"]
custom-protocol = ["tauri/custom-protocol"]
EOF

  cat > "$work/no-default.toml" <<'EOF'
[features]
custom-protocol = ["tauri/custom-protocol"]
EOF

  cat > "$work/other-default.toml" <<'EOF'
[features]
default = ["tray-icon"]
custom-protocol = ["tauri/custom-protocol"]
EOF

  cat > "$work/not-forwarded.toml" <<'EOF'
[features]
default = ["custom-protocol"]
custom-protocol = []
EOF

  if ! check "$work/good.toml" >/dev/null; then
    echo "!! 自检失败：正确的 Cargo.toml 应当通过" >&2
    exit 1
  fi
  for bad in no-default other-default not-forwarded; do
    if check "$work/$bad.toml" >/dev/null 2>&1; then
      echo "!! 自检失败：$bad.toml 应当被拦住" >&2
      exit 1
    fi
  done
  if check "$work/nope.toml" >/dev/null 2>&1; then
    echo "!! 自检失败：不存在的文件应当被拦住" >&2
    exit 1
  fi
  echo "自检通过：正确配置放行，缺 default / default 不含 / 没转发 三种坏配置都被拦住。"
  exit 0
fi

check "${1:-src-tauri/Cargo.toml}"
