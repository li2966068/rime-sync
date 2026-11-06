#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 vendored rime-data 裁剪成「保留方案 + 依赖闭包」的 data 目录（deb 用）。

单一真相源是 packaging/rime/retained-schemas.txt（与 Rust prune、其它生成器共用）。
保留方案 = luna_pinyin_simp / wubi_pinyin / wubi86；另外自动带上 schema 里
__include 的基类（luna_pinyin）、dependencies（pinyin_simp / stroke）与
__patch 引用的资源（pinyin.yaml）——闭包算法在 scripts/ime/rime_common.py。

同时把 default.yaml 的 schema_list 改写成保留清单：librime 部署器会把缺失的
schema 计为失败，随包 data 里不能再列 16 个方案。

0.6.22：--check 的「多余」断言只校验方案文件（*.schema.yaml / *.custom.yaml）。
default.yaml / weasel.yaml / fcitx5.yaml / symbols.yaml / key_bindings.yaml /
punctuation.yaml / installation.yaml / user.yaml / *.txt / *.json 由 librime、
fcitx5 或用户后续生成，一律放行——0.6.21 的 CI 就是因为 fcitx5.yaml 被判成
「多余方案」而失败。

用法：
  scripts/ime/gen-rime-data.py --out build/deb/data
  scripts/ime/gen-rime-data.py --out build/deb/data --check   # 校验已有目录
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import rime_common


def check(out_dir: Path, rime_data: Path) -> int:
    problems = rime_common.check_data(out_dir, rime_data)
    if problems:
        for item in problems:
            print("!! %s" % item, file=sys.stderr)
        return 1
    want = rime_common.expected_data(rime_data)
    print("OK: %d 个 data 文件与保留清单一致（%s）" % (len(want), out_dir))
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="裁剪随包 rime-data（保留方案 + 依赖闭包）")
    parser.add_argument("--out", required=True, help="输出目录（deb 的 /usr/share/rime-data）")
    parser.add_argument("--rime-data", default=str(rime_common.RIME_DATA),
                        help="vendored rime-data 目录")
    parser.add_argument("--check", action="store_true", help="只校验已有目录，不写盘")
    args = parser.parse_args(argv)

    rime_data = Path(args.rime_data)
    out_dir = Path(args.out)
    if args.check:
        return check(out_dir, rime_data)
    try:
        written = rime_common.install_data(
            out_dir, rime_common.retained_schemas(), rime_data
        )
    except SystemExit as exc:
        print("!! %s" % exc, file=sys.stderr)
        return 1
    ids = rime_common.retained_schemas()
    print(
        "data: %d 个文件 -> %s（保留 %s）"
        % (len(written), out_dir, ", ".join(ids))
    )
    for name in ids:
        if not (out_dir / ("%s.schema.yaml" % name)).is_file():
            print("!! 保留方案 %s 没有 data 文件" % name, file=sys.stderr)
            return 1
    return check(out_dir, rime_data)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
