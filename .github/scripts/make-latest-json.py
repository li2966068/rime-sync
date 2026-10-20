#!/usr/bin/env python3
"""把 tauri 生成的 *.sig 汇总成更新检查用的 latest.json。

用法：
    python3 .github/scripts/make-latest-json.py \
        --dir dist --version 0.2.0 --repo owner/repo --tag v0.2.0 \
        --notes "变更见 CHANGELOG.md" --out dist/latest.json

约定（Tauri v2 updater）：
    linux  → rime-sync_<ver>_amd64.AppImage.tar.gz + 同名 .sig → 平台键 linux-x86_64
    windows→ rime-sync_<ver>_x64-setup.exe + 同名 .sig        → 平台键 windows-x86_64
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import urllib.parse
from datetime import datetime, timezone

PLATFORM_SUFFIXES = [
    (".AppImage.tar.gz", "linux-x86_64"),
    ("-setup.exe", "windows-x86_64"),
    (".AppImage", "linux-x86_64"),
]
DEFAULT_REQUIRED = ("linux-x86_64", "windows-x86_64")


def classify(name: str) -> str | None:
    for suffix, platform in PLATFORM_SUFFIXES:
        if name.endswith(suffix):
            return platform
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 Tauri updater 的 latest.json")
    parser.add_argument("--dir", required=True, help="存放构建产物的目录（递归查找 *.sig）")
    parser.add_argument("--version", required=True, help="版本号，不带 v 前缀")
    parser.add_argument("--repo", required=True, help="GitHub 仓库，owner/name")
    parser.add_argument("--tag", required=True, help="发布 tag，如 v0.2.0")
    parser.add_argument("--notes", default="", help="release notes 摘要")
    parser.add_argument("--out", required=True, help="输出 latest.json 路径")
    parser.add_argument("--allow-partial", action="store_true", help="缺少某个平台时不报错")
    args = parser.parse_args()

    root = pathlib.Path(args.dir)
    if not root.is_dir():
        print(f"error: 目录不存在：{root}", file=sys.stderr)
        return 2

    platforms: dict[str, dict[str, str]] = {}
    for sig in sorted(root.rglob("*.sig")):
        artifact = sig.with_suffix("")
        if not artifact.is_file():
            print(f"warn: 跳过没有对应产物的签名 {sig}", file=sys.stderr)
            continue
        platform = classify(artifact.name)
        if platform is None:
            print(f"warn: 无法从文件名判断平台，跳过 {artifact.name}", file=sys.stderr)
            continue
        signature = sig.read_text(encoding="utf-8").strip()
        if not signature:
            print(f"warn: 签名文件为空，跳过 {sig}", file=sys.stderr)
            continue
        url = "https://github.com/{repo}/releases/download/{tag}/{name}".format(
            repo=args.repo,
            tag=urllib.parse.quote(args.tag),
            name=urllib.parse.quote(artifact.name),
        )
        platforms[platform] = {"signature": signature, "url": url}

    missing = [p for p in DEFAULT_REQUIRED if p not in platforms]
    if missing:
        message = f"缺少平台更新包：{', '.join(missing)}"
        if args.allow_partial:
            print(f"warn: {message}（--allow-partial，继续）", file=sys.stderr)
        else:
            print(f"error: {message}", file=sys.stderr)
            return 1

    payload = {
        "version": args.version.lstrip("v"),
        "notes": args.notes,
        "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }

    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"latest.json 已生成：{out}（平台：{', '.join(sorted(platforms))}）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
