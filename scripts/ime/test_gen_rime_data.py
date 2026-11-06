#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""gen-rime-data.py --check 的自检（0.6.22）。

回归用例：0.6.21 的 CI ime 作业把 fcitx5.yaml 判成「多余方案」后失败。
「多余」断言只允许针对方案文件（*.schema.yaml / *.custom.yaml）；其余配置
（default/weasel/fcitx5/symbols/key_bindings/punctuation/installation/user、
*.txt、*.json）一律放行。

用法：
  python3 scripts/ime/test_gen_rime_data.py
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True  # 不要在 scripts/ime 里留 __pycache__
sys.path.insert(0, str(Path(__file__).resolve().parent))

import rime_common

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "gen-rime-data.py"


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def fake_rime_data(root: Path) -> Path:
    """最小 vendored rime-data：保留的 3 个方案 + luna_pinyin 基类 + essay。"""
    data = root / "rime-data"
    write(
        data / "rime-prelude" / "default.yaml",
        "schema_list:\n"
        "  - schema: luna_pinyin_simp\n"
        "  - schema: wubi_pinyin\n"
        "  - schema: wubi86\n"
        "\n"
        "switcher:\n"
        "  hotkeys:\n"
        "    - Control+grave\n",
    )
    write(data / "rime-essay" / "essay.txt", "的\t100\n")
    write(
        data / "rime-test" / "luna_pinyin.schema.yaml",
        "schema:\n  schema_id: luna_pinyin\n",
    )
    for sid in ("luna_pinyin_simp", "wubi_pinyin", "wubi86"):
        write(
            data / "rime-test" / ("%s.schema.yaml" % sid),
            "schema:\n  schema_id: %s\n" % sid,
        )
    return data


class CheckDataTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.rime_data = fake_rime_data(self.root)
        self.out = self.root / "out"
        self.out.mkdir(parents=True, exist_ok=True)
        for name, text in rime_common.expected_data(self.rime_data).items():
            write(self.out / name, text)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_non_scheme_configs_are_allowed(self) -> None:
        for name in (
            "fcitx5.yaml",
            "weasel.yaml",
            "symbols.yaml",
            "key_bindings.yaml",
            "punctuation.yaml",
            "installation.yaml",
            "user.yaml",
            "notes.txt",
            "meta.json",
        ):
            write(self.out / name, "generated at runtime\n")
        self.assertEqual(rime_common.check_data(self.out, self.rime_data), [])

    def test_extra_schema_is_reported(self) -> None:
        write(self.out / "terra_pinyin.schema.yaml", "schema:\n  schema_id: terra_pinyin\n")
        problems = rime_common.check_data(self.out, self.rime_data)
        self.assertTrue(
            any("多余 terra_pinyin.schema.yaml" in item for item in problems), problems
        )

    def test_extra_custom_is_reported(self) -> None:
        write(self.out / "terra_pinyin.custom.yaml", "patch: {}\n")
        problems = rime_common.check_data(self.out, self.rime_data)
        self.assertTrue(
            any("多余 terra_pinyin.custom.yaml" in item for item in problems), problems
        )


class CommandLineTest(unittest.TestCase):
    """直接跑 --check，确认退出码与提示（与 CI 用法一致）。"""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.rime_data = fake_rime_data(self.root)
        self.out = self.root / "out"
        self.out.mkdir(parents=True, exist_ok=True)
        for name, text in rime_common.expected_data(self.rime_data).items():
            write(self.out / name, text)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def run_check(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--rime-data",
                str(self.rime_data),
                "--out",
                str(self.out),
                "--check",
            ],
            capture_output=True,
            text=True,
        )

    def test_exit_zero_with_fcitx5_yaml(self) -> None:
        write(self.out / "fcitx5.yaml", "input_method:\n  - name: rime\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("OK:", result.stdout)

    def test_exit_nonzero_with_extra_schema(self) -> None:
        write(self.out / "terra_pinyin.schema.yaml", "schema:\n  schema_id: terra_pinyin\n")
        result = self.run_check()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("多余 terra_pinyin.schema.yaml", result.stderr)


if __name__ == "__main__":
    unittest.main()
