#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""gen-schema-custom.py 的自检。

默认跑「不依赖 librime」的静态检查：
  * 保留清单里的每个方案都生成了 custom 文件，且能被 yaml.safe_load 解析
  * translators 首位是 rimesync_vnum
  * engine/processors 顺序满足 rime-sync 的三条硬规则（与 Rust 侧
    crates/core/src/processor_spec.rs 的 validate() 同一口径）：
    rimesync_vselect 紧跟 recognizer、numpad 紧跟 rimesync_vselect、
    rimesync_punct 排在 punctuator 之前（否则它拿不到按键，中文标点直出等于没开）
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

import rime_common  # 与生成器共用同一份规则（scripts/ime 在 sys.path 里）

REPO = Path(__file__).resolve().parents[2]
GEN = REPO / "scripts" / "ime" / "gen-schema-custom.py"
RIME_DATA = REPO / "third_party" / "rime-data"
OUT = REPO / "packaging" / "rime" / "schemas"
USER_DEFAULT = REPO / "packaging" / "rime" / "default.custom.yaml"
PLUGIN_TABLE = (
    REPO / "third_party" / "librime" / "plugins" / "rimesync-extra" / "punct_table.h"
)

FAILS = []


# 生成器的兜底表（PUNCT_PRIMARY）与插件表（punct_table.h）之间的**已知差异**。
# 每一处都必须写清楚原因；改了插件表而不同步这里（或反过来），CI 就要变红。
PUNCT_DIVERGENCE = {
    "\\": "插件直出全角反斜杠 '＼'（2026-11-11 起），生成器仍是顿号 '、'；"
          "改它会牵动 packaging/rime 的产物（custom.yaml + 编译好的 "
          "build/*.schema.yaml），必须连同编译产物一起重新生成",
    "|": "插件故意**不含** '|' 这一项：它是 '\\' 的 Shift 孪生，菜单挂在 "
         "'\\' 上（见 punct_table.h 的 FindShiftAlternatives）",
}

# 插件有、生成器没有覆盖的键：这些键在 Rime 自带的 punctuator 里本来就直出
# 全角，生成器不需要再写一遍 half_shape 覆盖。插件表增删键时这里也要跟着改。
PLUGIN_ONLY_KEYS = {"!", ",", ".", "/", ":", ";", "?"}


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
    # 保留清单是单一真相源（packaging/rime/retained-schemas.txt）：模板数必须与它一致。
    # 早先这里写死 16（0.6.21 之前的方案数），保留清单收敛到 3 个后一直误报。
    retained = rime_common.retained_schemas()
    check(ids == retained, "schema_list 与 retained-schemas.txt 一致",
          "%s vs %s" % (ids, retained))
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

        # 位置口径：vselect 紧跟 recognizer、numpad 紧跟 vselect、
        # punct 排在 punctuator 之前（与 crates/core/src/processor_spec.rs 一致）。
        violations = rime_common.processor_order_violations(processors)
        check(not violations, "%s processors 顺序满足三条硬规则" % sid, "；".join(violations))
        check(
            [x for x in processors if x not in (gen.PUNCT, gen.VSEL, gen.NUMPAD)] == src_p,
            "%s processors 其余顺序与源方案一致" % sid,
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


def parse_plugin_table():
    """从 punct_table.h 里读出插件的标点表：{键: (主字符, [备选…])}。

    插件表是运行时的权威（rimesync_punct 排在 punctuator 之前，命中即上屏），
    生成器的 PUNCT_PRIMARY 只是「没装插件时 punctuator 的兜底」，两者必须对得上。
    """
    text = PLUGIN_TABLE.read_text(encoding="utf-8")
    entry = re.compile(
        r"table\['((?:\\.|[^'\\])*)'\]\s*=\s*PunctEntry\{u8\"([^\"]*)\",\s*\{([^}]*)\}\};"
    )
    table = {}
    for key, main, alternatives in entry.findall(text):
        table[unescape_cpp(key)] = (main, re.findall(r'u8"([^"]*)"', alternatives))
    return table


def parse_base_key_of():
    """从 punct_table.h 里读出 BaseKeyOf()（Shift 字符 → 基础键）。"""
    text = PLUGIN_TABLE.read_text(encoding="utf-8")
    cases = re.findall(
        r"case '((?:\\.|[^'\\])*)'\s*:\s*return '((?:\\.|[^'\\])*)';", text
    )
    return {unescape_cpp(src): unescape_cpp(dst) for src, dst in cases}


def unescape_cpp(text):
    """C++ 字符字面量里的转义（只处理本表用得到的那几种）。"""
    return (
        text.replace("\\\\", "\\")
        .replace("\\'", "'")
        .replace('\\"', '"')
    )


# 菜单首项规则的例外：'\\' 的 Shift 孪生是 '|'，而 '|' 故意不在插件表里
# （菜单挂在 '\\' 上），所以它的菜单首项是全角竖线 '｜'，不是 '\\' 自己的
# 主字符 '＼'（按约定，菜单不提供这个键自己的主字符）。
MENU_FIRST_ITEM_EXCEPTIONS = {"\\": "Shift 孪生 '|' 不在插件表里"}


def punct_table_consistency(gen):
    """锁住「生成器的兜底表 ↔ 插件的标点表」的一致性。"""
    if not PLUGIN_TABLE.exists():
        check(False, "插件表存在", str(PLUGIN_TABLE))
        return
    table = parse_plugin_table()
    check(bool(table), "插件表解析出条目", "%d 条" % len(table))

    # 插件表里不得有 '|'：它是 '\\' 的 Shift 孪生，菜单挂在 '\\' 上。
    check("|" not in table, "插件表不含 '|'")

    base_of = parse_base_key_of()
    check(len(base_of) >= 20, "BaseKeyOf 解析出条目", "%d 条" % len(base_of))

    # 带备选的项：菜单首项必须是「Shift 孪生的主字符」（例外见上）。
    bad_first = []
    for key, (main, alternatives) in table.items():
        if not alternatives or key in MENU_FIRST_ITEM_EXCEPTIONS:
            continue
        if alternatives[0] != main:
            bad_first.append(key)
    check(not bad_first, "插件表带备选项的备选首项 = 主字符", repr(bad_first))

    # 带备选的项：菜单里不能出现这个键**不按 Shift**时的主字符
    # （即该键的基础键的主字符；基础键不在表里就没有这条约束）。
    bad_include = []
    for key, (main, alternatives) in table.items():
        if not alternatives:
            continue
        base = base_of.get(key)
        if base is None or base not in table:
            continue
        if table[base][0] in alternatives:
            bad_include.append(key)
    check(not bad_include, "插件菜单不含该键自己的主字符", repr(bad_include))

    # 生成器覆盖的键：与插件同名时主字符必须一致（已知差异除外）。
    mismatched = []
    for key, char in gen.PUNCT_PRIMARY.items():
        if key in PUNCT_DIVERGENCE:
            continue
        if key not in table:
            mismatched.append("%s: 生成器有、插件没有" % key)
            continue
        if table[key][0] != char:
            mismatched.append("%s: 生成器 %r vs 插件 %r" % (key, char, table[key][0]))
    check(not mismatched, "生成器与插件的主字符一致（已知差异除外）", repr(mismatched))

    # 生成器有、插件没有的键：必须都写进了已知差异清单。
    unknown = [
        key for key in gen.PUNCT_PRIMARY if key not in table and key not in PUNCT_DIVERGENCE
    ]
    check(not unknown, "生成器独有的键都登记在已知差异里", repr(unknown))

    # 已知差异清单里的键不能悄悄失效（差异消失了也要改清单）。
    stale = [
        key
        for key in PUNCT_DIVERGENCE
        if key in table and key in gen.PUNCT_PRIMARY and table[key][0] == gen.PUNCT_PRIMARY[key]
    ]
    check(not stale, "已知差异清单没有过期项", repr(stale))

    # 插件有、生成器没覆盖的键：集合必须与登记的一致。
    plugin_only = set(table) - set(gen.PUNCT_PRIMARY)
    check(
        plugin_only == PLUGIN_ONLY_KEYS,
        "插件独有键集合与登记的一致",
        "%s vs %s" % (sorted(plugin_only), sorted(PLUGIN_ONLY_KEYS)),
    )

    with_alts = sorted(k for k, (_m, alts) in table.items() if alts)
    print(
        "  info 插件表 %d 项，其中带备选 %d 项：%s"
        % (len(table), len(with_alts), " ".join(with_alts))
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
        violations = rime_common.processor_order_violations(processors)
        check(not violations, "[生效] %s processors 顺序满足三条硬规则" % sid,
              "；".join(violations))
        check(
            [x for x in processors if x not in (gen.PUNCT, gen.VSEL, gen.NUMPAD)] == src_proc,
            "[生效] %s 引擎其余顺序不变" % sid,
        )
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
    print("[1/4] 静态检查")
    static_checks(gen)
    print("[2/4] 标点表一致性（生成器 ↔ 插件）")
    punct_table_consistency(gen)
    print("[3/4] 可复现性 / --check")
    check_reproducible(gen)
    if args.deploy:
        print("[4/4] 真实 librime 部署验证")
        deploy_checks(gen, args.keep)
    else:
        print("[4/4] 跳过真实部署验证（加 --deploy 开启）")

    if FAILS:
        print("\n!! %d 项失败：" % len(FAILS))
        for item in FAILS:
            print("   - %s" % item)
        return 1
    print("\n全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
