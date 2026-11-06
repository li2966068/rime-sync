#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""gen-schema-custom.py 的自检。

默认跑「不依赖 librime」的静态检查：
  * 16 个方案都生成了 custom 文件，且能被 yaml.safe_load 解析
  * translators 首位是 rimesync_vnum；rimesync_punct 在 key_binder/punctuator/selector
    中最先出现的那个之前（selector 会先吃掉数字选词）
  * 其余引擎条目顺序与 vendored 方案完全一致（含 __include 继承来的方案）
  * 有 recognizer/patterns/vnum（vnum 依赖 segmentor 把 v1234 当一个片段）
  * 不再有 import_preset；half_shape 目标键是 {commit: X}，非目标键与 preset 原值一致
  * --check 通过（仓库里提交的产物与当前生成结果一致），且生成结果可复现

加 --deploy 时再用真实 librime 验证一遍（需要系统里有 rime_deployer）：
把 vendored 数据拷到临时目录，用生成的 custom 文件跑 rime_deployer --build，
断言部署日志没有 config 构建错误，并解析 deployer 写出的 <schema>.schema.yaml
（编译后的生效配置）核对引擎顺序、recognizer 模式与 punctuator 取值。

用法：
  scripts/ime/test_gen_schema_custom.py [--deploy] [--keep]
"""
from __future__ import annotations

import argparse
import importlib.util
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

sys.dont_write_bytecode = True  # 不要在 scripts/ime 里留 __pycache__

REPO = Path(__file__).resolve().parents[2]
GEN = REPO / "scripts" / "ime" / "gen-schema-custom.py"
RIME_DATA = REPO / "third_party" / "rime-data"
OUT = REPO / "packaging" / "rime" / "schemas"
USER_DEFAULT = REPO / "packaging" / "rime" / "default.custom.yaml"

FAILS = []


def check(ok, label, detail=""):
    if ok:
        print("  ok   %s" % label)
    else:
        print("  FAIL %s %s" % (label, detail))
        FAILS.append(label)


def load_gen():
    spec = importlib.util.spec_from_file_location("gen_schema_custom", GEN)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def source_cfg(sid, gen):
    """独立读一遍 vendored 方案（含 __include 链）的有效配置，用来对照产物。"""
    index = gen.schema_index(RIME_DATA)
    return gen.resolve_schema(index[sid], index)


def static_checks(gen):
    ids = gen.schema_ids(RIME_DATA, USER_DEFAULT)
    check(len(ids) == 16, "schema_list 是 16 个方案", "got %d" % len(ids))
    files = sorted(OUT.glob("*.custom.yaml"))
    check(len(files) == len(ids), "custom 文件数 = 方案数", "%d vs %d" % (len(files), len(ids)))

    for sid in ids:
        path = OUT / ("%s.custom.yaml" % sid)
        if not path.exists():
            check(False, "%s 存在" % sid)
            continue
        try:
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            check(False, "%s 可解析" % sid, str(exc))
            continue
        check(isinstance(doc, dict) and isinstance(doc.get("patch"), dict), "%s 有 patch" % sid)
        patch = doc["patch"]
        processors = patch.get("engine/processors") or []
        translators = patch.get("engine/translators") or []
        cfg = source_cfg(sid, gen)
        src_p = cfg["engine"]["processors"]
        src_t = cfg["engine"]["translators"]

        check(translators[:1] == [gen.VNUM], "%s translators[0]=%s" % (sid, gen.VNUM))
        check(
            [x for x in translators if x != gen.VNUM] == src_t,
            "%s translators 其余顺序与源方案一致" % sid,
        )

        anchors = [i for i, name in enumerate(src_p) if name in gen.PUNCT_ANCHORS]
        want_idx = min(anchors) if anchors else None
        if gen.PUNCT in processors:
            idx = processors.index(gen.PUNCT)
            check(
                idx == want_idx
                and (processors[idx + 1] == src_p[want_idx]
                     or (processors[idx + 1] == gen.VSEL
                         and processors[idx + 2] == gen.NUMPAD
                         and processors[idx + 3] == src_p[want_idx])),
                "%s rimesync_punct 在最先出现的 key_binder/punctuator/selector 之前" % sid,
                "idx=%s want=%s" % (idx, want_idx),
            )
        else:
            check(False, "%s 有 rimesync_punct" % sid)
        check(
            [x for x in processors if x not in (gen.PUNCT, gen.VSEL, gen.NUMPAD)] == src_p,
            "%s processors 其余顺序与源方案一致" % sid,
        )
        check(
            gen.VSEL in processors
            and processors.index(gen.VSEL) == processors.index(gen.PUNCT) + 1,
            "%s rimesync_vselect 紧跟 rimesync_punct" % sid,
        )
        check(
            gen.NUMPAD in processors
            and processors.index(gen.NUMPAD) == processors.index(gen.VSEL) + 1,
            "%s numpad 紧跟 rimesync_vselect" % sid,
        )
        check(
            patch.get("recognizer/patterns/vnum") == gen.VNUM_PATTERN,
            "%s 有 recognizer/patterns/vnum" % sid,
            repr(patch.get("recognizer/patterns/vnum")),
        )

        punct = patch.get("punctuator")
        check(isinstance(punct, dict), "%s 内联了 punctuator" % sid)
        check("import_preset" not in punct, "%s 去掉了 import_preset" % sid)
        half = punct.get("half_shape") or {}
        source = gen.effective_punctuator(cfg, RIME_DATA)
        src_half = source.get("half_shape") or {}
        skipped = {k for k, _c, _cur in gen.build_schema(sid, gen.schema_index(RIME_DATA))["skipped"]}
        extra = set(half) - set(src_half)
        check(
            set(src_half) <= set(half) and extra <= set(gen.PUNCT_PRIMARY),
            "%s half_shape 保留源方案全部键" % sid,
            "missing=%s extra=%s" % (set(src_half) - set(half), extra),
        )
        wrong = [
            key
            for key, value in src_half.items()
            if key not in gen.PUNCT_PRIMARY and half.get(key) != value
        ]
        check(not wrong, "%s half_shape 非目标键取值未改动（%d 键）" % (sid, len(src_half)), repr(wrong))
        for key, char in gen.PUNCT_PRIMARY.items():
            if key in skipped:
                check(
                    half.get(key) == src_half.get(key),
                    "%s 保留方案自定 %r" % (sid, key),
                    repr(half.get(key)),
                )
            else:
                check(half.get(key) == {"commit": char}, "%s half_shape[%r]=%s" % (sid, key, char))
        check(punct.get("full_shape") == source.get("full_shape"), "%s full_shape 未改动" % sid)
        check(
            ("symbols" in punct) == ("symbols" in source),
            "%s punctuator/symbols 与源方案一致" % sid,
        )


def check_reproducible(gen):
    with tempfile.TemporaryDirectory() as tmp:
        first = Path(tmp) / "a"
        second = Path(tmp) / "b"
        for target in (first, second):
            rc = gen.main(["--out", str(target), "--rime-data", str(RIME_DATA)])
            check(rc == 0, "生成到 %s" % target.name)
        same = all(
            (first / p.name).read_text(encoding="utf-8")
            == (second / p.name).read_text(encoding="utf-8")
            for p in first.glob("*.custom.yaml")
        )
        check(same, "两次生成结果一致（可复现）")
        check(
            gen.main(["--check", "--out", str(first), "--rime-data", str(RIME_DATA)]) == 0,
            "--check 对刚生成的结果通过",
        )
        check(
            gen.main(["--check", "--out", str(OUT), "--rime-data", str(RIME_DATA)]) == 0,
            "--check 对仓库产物通过",
        )


def deploy_checks(gen, keep=False):
    if not shutil.which("rime_deployer"):
        print("  skip 系统里没有 rime_deployer，跳过真实部署验证")
        return
    ids = gen.schema_ids(RIME_DATA, USER_DEFAULT)
    tmp = Path(tempfile.mkdtemp(prefix="rime-sync-custom-"))
    user, shared, staging = tmp / "user", tmp / "shared", tmp / "staging"
    for d in (user, shared, staging):
        d.mkdir(parents=True)
    for path in list(RIME_DATA.glob("*/*.yaml")) + list(RIME_DATA.glob("*/*.txt")):
        shutil.copy(path, shared / path.name)
    default = yaml.safe_load((shared / "default.yaml").read_text(encoding="utf-8"))
    default["schema_list"] = [{"schema": sid} for sid in ids]
    (shared / "default.yaml").write_text(
        yaml.safe_dump(default, allow_unicode=True, sort_keys=False), encoding="utf-8"
    )
    for path in OUT.glob("*.custom.yaml"):
        shutil.copy(path, user / path.name)

    log = tmp / "deploy.log"
    with open(log, "w", encoding="utf-8") as fh:
        rc = subprocess.call(
            ["rime_deployer", "--build", str(user), str(shared), str(staging)],
            stdout=fh,
            stderr=subprocess.STDOUT,
        )
    text = log.read_text(encoding="utf-8", errors="replace")
    bad = [
        line
        for line in text.splitlines()
        if re.search(
            r"failed to merge|error building config|incompatible node type|failed to include",
            line,
        )
    ]
    check(not bad, "rime_deployer 部署无配置错误", "\n".join(bad[:6]))
    check(rc == 0, "rime_deployer 退出码 0", "rc=%d" % rc)

    for sid in ids:
        compiled = staging / ("%s.schema.yaml" % sid)
        if not compiled.exists():
            check(False, "deployer 写出 %s.schema.yaml（生效配置）" % sid)
            continue
        cfg = yaml.safe_load(compiled.read_text(encoding="utf-8")) or {}
        engine = cfg.get("engine") or {}
        processors = engine.get("processors") or []
        translators = engine.get("translators") or []
        check(translators[:1] == [gen.VNUM], "[生效] %s translators[0]=%s" % (sid, gen.VNUM))

        src = source_cfg(sid, gen)
        src_proc = src["engine"]["processors"]
        src_t = src["engine"]["translators"]
        want_idx = min(i for i, n in enumerate(src_proc) if n in gen.PUNCT_ANCHORS)
        if gen.PUNCT in processors:
            idx = processors.index(gen.PUNCT)
            check(
                idx == want_idx
                and [x for x in processors if x not in (gen.PUNCT, gen.VSEL, gen.NUMPAD)] == src_proc
                and gen.VSEL in processors
                and processors.index(gen.VSEL) == idx + 1
                and gen.NUMPAD in processors
                and processors.index(gen.NUMPAD) == idx + 2,
                "[生效] %s rimesync_punct 位置正确且引擎其余顺序不变" % sid,
            )
        else:
            check(False, "[生效] %s 有 rimesync_punct" % sid)
        check(
            [x for x in translators if x != gen.VNUM] == src_t,
            "[生效] %s translators 其余顺序不变" % sid,
        )

        recog = (cfg.get("recognizer") or {}).get("patterns") or {}
        check(
            recog.get("vnum") == gen.VNUM_PATTERN,
            "[生效] %s recognizer/patterns/vnum" % sid,
            repr(recog.get("vnum")),
        )
        src_recog = (src.get("recognizer") or {}).get("patterns") or {}
        check(
            set(src_recog) <= set(recog),
            "[生效] %s recognizer/patterns 保留原有键" % sid,
            "%s" % (set(src_recog) - set(recog)),
        )

        punct = cfg.get("punctuator") or {}
        check("import_preset" not in punct, "[生效] %s 没有 import_preset" % sid)
        half = punct.get("half_shape") or {}
        source = gen.effective_punctuator(src, RIME_DATA)
        src_half = source.get("half_shape") or {}
        skipped = {k for k, _c, _cur in gen.build_schema(sid, gen.schema_index(RIME_DATA))["skipped"]}
        extra = set(half) - set(src_half)
        check(
            set(src_half) <= set(half) and extra <= set(gen.PUNCT_PRIMARY),
            "[生效] %s half_shape 保留源方案全部键" % sid,
            "missing=%s extra=%s" % (set(src_half) - set(half), extra),
        )
        wrong = []
        for key, value in src_half.items():
            if key in gen.PUNCT_PRIMARY and key not in skipped:
                want = {"commit": gen.PUNCT_PRIMARY[key]}
            else:
                want = value
            if half.get(key) != want:
                wrong.append(key)
        check(
            not wrong,
            "[生效] %s half_shape 取值与预期一致（%d 键）" % (sid, len(src_half)),
            repr(wrong),
        )
        check(punct.get("full_shape") == source.get("full_shape"), "[生效] %s full_shape 未改动" % sid)
        check(
            ("symbols" in punct) == ("symbols" in source),
            "[生效] %s punctuator/symbols 与源方案一致" % sid,
        )

    if keep:
        print("  info 临时目录保留在 %s" % tmp)
    else:
        shutil.rmtree(tmp, ignore_errors=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description="gen-schema-custom.py 自检")
    parser.add_argument("--deploy", action="store_true", help="额外用真实 librime 部署验证")
    parser.add_argument("--keep", action="store_true", help="保留部署临时目录")
    args = parser.parse_args(argv)

    gen = load_gen()
    print("[1/3] 静态检查")
    static_checks(gen)
    print("[2/3] 可复现性 / --check")
    check_reproducible(gen)
    if args.deploy:
        print("[3/3] 真实 librime 部署验证")
        deploy_checks(gen, args.keep)
    else:
        print("[3/3] 跳过真实部署验证（加 --deploy 开启）")

    if FAILS:
        print("\n!! %d 项失败：" % len(FAILS))
        for item in FAILS:
            print("   - %s" % item)
        return 1
    print("\n全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
