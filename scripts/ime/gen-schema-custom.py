#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成「随包方案」的 <schema>.custom.yaml。

每个文件做三件事：
  1. engine/translators 首位加 rimesync_vnum（v123 / v12.5 -> 中文数字候选）
  2. engine/processors 里 punctuator 之前插入 rimesync_punct（中文标点直出 + Shift 备选）
  3. 内联 punctuator：half_shape 的主字符改成单值 commit（没装插件时的兜底）

为什么 punctuator 要整段内联，而不是只写 punctuator/half_shape/<键>：
  方案里写的是 "punctuator: import_preset: symbols"，librime 在 link 阶段把 preset
  整个 include 进来，再把方案自己的 punctuator 当 overrides 逐键合并（见
  legacy_preset_config_plugin.cc + IncludeReference::Resolve）。合并时节点类型必须一致：
  symbols 里 '<' 是 list（候选菜单），我们想写成 {commit: 《}（map），
  实测 librime 直接报
      copy on write failed; incompatible node type: commit
      failed to merge tree: symbols:punctuator
      error building config: luna_pinyin.schema
  整个方案配置构建失败。所以这里把生效的 punctuator 解析成完整映射后内联，
  并去掉 import_preset，避免二次 include。代价是文件大一些，好处是自足：
  不依赖 data 目录里的额外 preset 文件，跨设备同步 custom.yaml 也不会缺依赖。

列表一律从 vendored 方案（third_party/rime-data/**/<schema>.schema.yaml）解析，包括
"__include: <base>.schema:/" 继承来的方案（luna_pinyin_simp 等），不写死。

用法：
  scripts/ime/gen-schema-custom.py              # 写 packaging/rime/schemas/
  scripts/ime/gen-schema-custom.py --check      # 构建/CI 用：缺失或内容过期则退出 1
  scripts/ime/gen-schema-custom.py --out DIR    # 构建时直接写进 IME deb 的 data 目录
"""
from __future__ import annotations

import argparse
import copy
import sys
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]
RIME_DATA = REPO / "third_party" / "rime-data"
DEFAULT_OUT = REPO / "packaging" / "rime" / "schemas"
# 安装器真正写进用户目录的那份模板；schema_list 必须与随包 default.yaml 一致
USER_DEFAULT = REPO / "packaging" / "rime" / "default.custom.yaml"

VNUM = "rimesync_vnum"
PUNCT = "rimesync_punct"
VSEL = "rimesync_vselect"
# 小键盘归一化（KP_0..KP_9 / KP_Decimal -> 0..9 / .），对所有输入都生效。
NUMPAD = "numpad"
# rimesync_punct 要插在 key_binder / punctuator / selector 里最先出现的那个之前：
# 只插在 punctuator 前的话，数字选词会先被 selector（或 key_binder 的页码键）吃掉。
PUNCT_ANCHORS = ("key_binder", "punctuator", "selector")
# vnum translator 依赖 recognizer 把 v1234 当一个片段；朙月拼音的拼写代数会把 v 当
# 无效音节切开，所以每个方案都要给一条 vnum 模式（允许中间态 v、v12.）。
VNUM_PATTERN = r"^[vV][0-9.]*$"

# half_shape 主字符：无 Shift 直接上屏，Shift 弹备选（rimesync_punct 负责）。
# 这里同时是没装插件时的 punctuator 兜底。顺序即输出顺序。
PUNCT_PRIMARY = {
    "\\": "、",
    "<": "《",
    ">": "》",
    "[": "「",
    "]": "」",
    "{": "『",
    "}": "』",
    "|": "·",
    "%": "％",
    "$": "￥",
    "*": "×",
    "~": "～",
}

HEADER = [
    "# 由 rime-sync 生成：启用 v 数值转换与中文标点直出（缺失才写入）",
]

PRESET_FILES = {
    "symbols": ("rime-prelude", "symbols.yaml"),
    "default": ("rime-prelude", "default.yaml"),
}
# default preset 的 punctuator 只有 __include，真正的内容在 punctuation.yaml
PRESET_RESOURCES = {"punctuation": ("rime-prelude", "punctuation.yaml")}


def load_yaml(path):
    with open(path, encoding="utf-8") as fh:
        data = yaml.safe_load(fh)
    if data is None:
        return {}
    if not isinstance(data, dict):
        raise SystemExit("%s: 顶层不是映射" % path)
    return data


def schema_index(rime_data=RIME_DATA):
    index = {}
    for path in sorted(rime_data.glob("**/*.schema.yaml")):
        sid = path.name[: -len(".schema.yaml")]
        if sid in index:
            raise SystemExit("方案 id 重复：%s（%s / %s）" % (sid, index[sid], path))
        index[sid] = path
    if not index:
        raise SystemExit("%s 下没有 *.schema.yaml" % rime_data)
    return index


def schema_ids(rime_data=RIME_DATA, user_default=USER_DEFAULT):
    """随包方案的权威列表 = 随包 default.yaml 的 schema_list。

    安装器写进用户目录的 default.custom.yaml 是同一份列表的手工副本，两边不一致
    会造成「装了模板但没数据」或反之，直接在生成阶段报错。
    """
    prelude = load_yaml(rime_data / "rime-prelude" / "default.yaml")
    ids = [item["schema"] for item in prelude.get("schema_list") or []]
    if not ids:
        raise SystemExit("随包 default.yaml 没有 schema_list")
    if user_default.exists():
        template = load_yaml(user_default)
        tids = [
            item["schema"]
            for item in (template.get("patch") or {}).get("schema_list") or []
        ]
        if tids != ids:
            raise SystemExit(
                "schema_list 不一致：default.yaml=%s / default.custom.yaml=%s" % (ids, tids)
            )
    return ids


def _deep_merge(base, over):
    if not isinstance(base, dict) or not isinstance(over, dict):
        return over
    merged = dict(base)
    for key, value in over.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            merged[key] = _deep_merge(base[key], value)
        else:
            merged[key] = value
    return merged


def _resolve_include_path(value, preset_data, where):
    """__include: punctuation:/half_shape -> 该 preset 文件里的节点。"""
    if not isinstance(value, str) or ":" not in value:
        raise SystemExit("%s: 不支持的 __include：%r" % (where, value))
    resource, _, node_path = value.partition(":")
    node_path = node_path.lstrip("/")
    if resource not in PRESET_RESOURCES:
        raise SystemExit("%s: 未知 preset 资源 %s" % (where, resource))
    recipe, name = PRESET_RESOURCES[resource]
    doc = load_yaml(preset_data / recipe / name)
    node = doc
    for key in [k for k in node_path.split("/") if k]:
        node = (node or {}).get(key)
        if node is None:
            raise SystemExit("%s: %s 里没有 %s" % (where, resource, node_path))
    return node


def _materialize(node, preset_data, where="preset"):
    """展开 preset 里的 __include（目前只遇到 __include）。"""
    if isinstance(node, dict):
        if "__include" in node and len(node) == 1:
            return _materialize(
                _resolve_include_path(node["__include"], preset_data, where),
                preset_data,
                where,
            )
        if "__patch" in node:
            raise SystemExit("%s: preset 里的 __patch 需要人工确认（%r）" % (where, node["__patch"]))
        return {k: _materialize(v, preset_data, where) for k, v in node.items()}
    if isinstance(node, list):
        return [_materialize(item, preset_data, where) for item in node]
    return node


def preset_punctuator(name, rime_data=RIME_DATA):
    if name not in PRESET_FILES:
        raise SystemExit("未知 punctuator preset：%s（已知：%s）" % (name, sorted(PRESET_FILES)))
    recipe, filename = PRESET_FILES[name]
    doc = load_yaml(rime_data / recipe / filename)
    node = doc.get("punctuator")
    if not isinstance(node, dict):
        raise SystemExit("%s/%s 没有 punctuator 段" % (recipe, filename))
    return _materialize(copy.deepcopy(node), rime_data)


def resolve_schema(path, index, stack=()):
    """把 "__include: <base>.schema:/" 链展开成一份有效配置（只关心 engine/punctuator）。"""
    if path in stack:
        raise SystemExit("__include 成环：%s" % " -> ".join(str(p) for p in stack + (path,)))
    doc = load_yaml(path)
    merged = {}
    include = doc.get("__include")
    if isinstance(include, str) and include.endswith(":/"):
        target = include[:-2]
        if target.endswith(".schema"):
            base_id = target[: -len(".schema")]
            base_path = index.get(base_id)
            if base_path is None:
                raise SystemExit("%s: __include 指向不存在的 %s.schema.yaml" % (path, base_id))
            merged = resolve_schema(base_path, index, stack + (path,))
    for key, value in doc.items():
        if key in ("__include", "__patch"):
            # 根级 __patch 只改 switches 之类，与 engine/punctuator 无关
            continue
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _is_fixed_commit(current, wanted):
    """方案自己把该键写成「固定上屏某个字符」且与我们要的主字符不同 -> 视为冲突。

    只有注音（bopomofo）这类不用 preset 的方案会命中（'<'=， '>'=。 '~'='~'），
    改了会破坏它自己的标点布局。
    """
    if isinstance(current, str):
        return current != wanted
    if isinstance(current, dict) and "commit" in current:
        return current["commit"] != wanted
    return False


def effective_punctuator(cfg, rime_data=RIME_DATA):
    """方案生效的 punctuator（方案自己的值优先，preset 打底），去掉 include 指令。"""
    own = cfg.get("punctuator") or {}
    if not isinstance(own, dict):
        raise SystemExit("punctuator 不是映射")
    base = {}
    preset_name = own.get("import_preset")
    if preset_name:
        base = preset_punctuator(preset_name, rime_data)
    return _deep_merge(base, {k: v for k, v in own.items() if k != "import_preset"})


def build_schema(sid, index, rime_data=RIME_DATA):
    path = index[sid]
    cfg = resolve_schema(path, index)
    engine = cfg.get("engine") or {}
    processors = list(engine.get("processors") or [])
    translators = list(engine.get("translators") or [])
    if not processors:
        raise SystemExit("%s: 解析后没有 engine/processors" % sid)
    if not translators:
        raise SystemExit("%s: 解析后没有 engine/translators" % sid)
    if PUNCT not in processors:
        anchors = [processors.index(name) for name in PUNCT_ANCHORS if name in processors]
        if not anchors:
            raise SystemExit(
                "%s: processors 里没有 %s 中的任何一个，无处插入 %s"
                % (sid, "/".join(PUNCT_ANCHORS), PUNCT)
            )
        processors.insert(min(anchors), PUNCT)
    if VSEL not in processors:
        # 紧随 rimesync_punct：两者都必须在 speller 之前（vselect 要能拦字母、punct 要能拦标点）。
        processors.insert(processors.index(PUNCT) + 1, VSEL)
    if NUMPAD not in processors:
        # 紧随 vselect：小键盘要先归一化成 ASCII，再交给 recognizer/speller。
        processors.insert(processors.index(VSEL) + 1, NUMPAD)
    if VNUM not in translators:
        translators.insert(0, VNUM)

    own = cfg.get("punctuator") or {}
    punct = effective_punctuator(cfg, rime_data)
    half = dict(punct.get("half_shape") or {})
    applied, skipped = [], []
    for key, char in PUNCT_PRIMARY.items():
        current = half.get(key)
        if _is_fixed_commit(current, char):
            skipped.append((key, char, current))
        else:
            applied.append((key, char))
            half[key] = {"commit": char}
    punct["half_shape"] = half
    return {
        "path": path,
        "processors": processors,
        "translators": translators,
        "punctuator": punct,
        "applied": applied,
        "skipped": skipped,
        "preset": own.get("import_preset"),
    }


def render(sid, built):
    path = built["path"]
    rel = path.relative_to(REPO).as_posix() if path.is_relative_to(REPO) else str(path)
    lines = list(HEADER)
    lines.append("# schema: %s" % sid)
    lines.append(
        "# 引擎顺序抄自 %s（translators 首位加 %s；%s 插在 key_binder/punctuator/selector 之前）"
        % (rel, VNUM, PUNCT)
    )
    lines.append("# 关闭：删掉本文件后重新部署；或删掉下面 patch 里的 %s / %s。" % (VNUM, PUNCT))
    lines.append("patch:")
    lines.append("  engine/processors:")
    for name in built["processors"]:
        lines.append("    - %s" % name)
    lines.append("  engine/translators:")
    for name in built["translators"]:
        lines.append("    - %s" % name)
    preset = built["preset"]
    lines.append("  # vnum 需要 recognizer 把 v1234 当成一个片段（v 在拼音方案里不是合法音节）")
    lines.append("  recognizer/patterns/vnum: '%s'" % VNUM_PATTERN)
    lines.append(
        "  # punctuator 整段内联（原 %s 已展开并去掉 import_preset）："
        % ("import_preset: %s" % preset if preset else "无 preset")
    )
    lines.append("  # librime 的 include 合并不能把 preset 里的候选 list 覆盖成 {commit: X}")
    lines.append("  # （实测报 incompatible node type / failed to merge tree），只能整体内联。")
    lines.append("  # 下面只改 half_shape 的主字符，其余键与方案/preset 完全一致。")
    dumped = yaml.safe_dump(
        {"punctuator": built["punctuator"]},
        allow_unicode=True,
        default_flow_style=None,
        sort_keys=False,
        width=4096,
    )
    for line in dumped.rstrip("\n").split("\n"):
        lines.append("  " + line if line else "")
    for key, char, current in built["skipped"]:
        lines.append("  # 保留 %r 的方案自定映射 %s（不改）" % (key, current))
    return "\n".join(lines) + "\n"


def expected_files(rime_data=RIME_DATA, user_default=USER_DEFAULT):
    index = schema_index(rime_data)
    ids = schema_ids(rime_data, user_default)
    out = {}
    for sid in ids:
        if sid not in index:
            raise SystemExit("schema_list 里的 %s 没有随包 schema 文件" % sid)
        out[sid] = render(sid, build_schema(sid, index, rime_data))
    return out


def main(argv=None):
    parser = argparse.ArgumentParser(description="生成随包方案的 <schema>.custom.yaml")
    parser.add_argument("--out", default=str(DEFAULT_OUT), help="输出目录（默认 packaging/rime/schemas）")
    parser.add_argument("--rime-data", default=str(RIME_DATA), help="vendored rime-data 目录")
    parser.add_argument("--check", action="store_true", help="只校验：缺失/过期/多余则退出 1")
    args = parser.parse_args(argv)

    rime_data = Path(args.rime_data)
    out_dir = Path(args.out)
    files = expected_files(rime_data)

    if args.check:
        problems = []
        for sid, text in sorted(files.items()):
            path = out_dir / ("%s.custom.yaml" % sid)
            if not path.exists():
                problems.append("缺失 %s" % path)
            elif path.read_text(encoding="utf-8") != text:
                problems.append("过期 %s" % path)
        if out_dir.is_dir():
            for path in sorted(out_dir.glob("*.custom.yaml")):
                if path.name[: -len(".custom.yaml")] not in files:
                    problems.append("多余 %s（schema_list 里没有这个方案）" % path)
        if problems:
            for item in problems:
                print("!! %s" % item, file=sys.stderr)
            print("!! 重新生成：scripts/ime/gen-schema-custom.py --out %s" % out_dir, file=sys.stderr)
            return 1
        print("OK: %d 个 custom 文件与方案一致（%s）" % (len(files), out_dir))
        return 0

    out_dir.mkdir(parents=True, exist_ok=True)
    for sid, text in sorted(files.items()):
        (out_dir / ("%s.custom.yaml" % sid)).write_text(text, encoding="utf-8")
    print("wrote %d custom files -> %s" % (len(files), out_dir))
    return 0


if __name__ == "__main__":
    sys.exit(main())
