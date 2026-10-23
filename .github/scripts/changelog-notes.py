#!/usr/bin/env python3
"""从 CHANGELOG.md 提取某个版本的段落，用作更新提示（中文）。

用法:
    python3 .github/scripts/changelog-notes.py --version 0.5.3 --out dist/notes.md
    python3 .github/scripts/changelog-notes.py --version v0.5.3          # 输出到 stdout
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys


def extract(text: str, version: str) -> str:
    ver = version.strip().lstrip("v")
    header = re.compile(rf"^##\s+v?{re.escape(ver)}\b.*$", re.MULTILINE)
    match = header.search(text)
    if not match:
        return ""
    start = match.end()
    nxt = re.compile(r"^##\s+", re.MULTILINE).search(text, start)
    end = nxt.start() if nxt else len(text)
    body = text[start:end].strip()
    title = match.group(0).strip()
    return f"{title}\n\n{body}\n" if body else f"{title}\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="提取 CHANGELOG 中的版本段落")
    parser.add_argument("--version", required=True, help="版本号，可带 v 前缀")
    parser.add_argument("--changelog", default="CHANGELOG.md")
    parser.add_argument("--out", help="写入文件；省略则打印到 stdout")
    args = parser.parse_args()

    text = pathlib.Path(args.changelog).read_text(encoding="utf-8")
    notes = extract(text, args.version)
    if not notes:
        print(f"error: CHANGELOG 中找不到版本 {args.version}", file=sys.stderr)
        return 1
    if args.out:
        pathlib.Path(args.out).write_text(notes, encoding="utf-8")
        print(f"notes: {args.out} ({len(notes)} chars)")
    else:
        sys.stdout.write(notes)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
