#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""随包输入方案的清单与依赖闭包（只用标准库）。

单一真相源是 `packaging/rime/retained-schemas.txt`：

* Rust（crates/core/src/prune.rs）用 `include_str!` 把同一份文件编译进去；
* Python 生成器（gen-schema-custom.py / gen-prebuilt-build.py / gen-rime-data.py）
  直接读它；
* shell（postinst / install.sh / NSIS 钩子）不抄列表，只调 `rime-sync-cli
  prune-schemas`，由上面的 Rust 实现统一判定。

这里刻意不 import yaml：deb 构建镜像（rime-ime-builder）里没有 PyYAML，而 schema
文件只需要读三种文本结构——根级 `__include: <x>.schema:/`、`dependencies:` 列表、
`dictionary:` 名称。用缩进扫描足够，误判会在 prebuilt 生成阶段由真实部署暴露。
"""

from __future__ import annotations

import re
import shutil
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RETAINED_FILE = REPO / "packaging" / "rime" / "retained-schemas.txt"
RIME_DATA = REPO / "third_party" / "rime-data"

SCHEMA_SUFFIX = ".schema.yaml"
DICT_SUFFIX = ".dict.yaml"

_INCLUDE_RE = re.compile(r"^\s*__include:\s*([A-Za-z0-9_]+)\.schema:/\s*$")
_DEPENDENCIES_RE = re.compile(r"^(\s*)dependencies:\s*$")
_ITEM_RE = re.compile(r"^\s*-\s*([A-Za-z0-9_]+)\s*$")
_DICTIONARY_RE = re.compile(r"^\s*dictionary:\s*[\"']?([A-Za-z0-9_.]+)[\"']?\s*$")
# 资源引用：__patch: pinyin:/abbreviation、librime 会把 pinyin.yaml 当资源加载。
# 前面排除 "." 是为了不把 luna_pinyin.schema:/ 拆成 schema。
_RESOURCE_RE = re.compile(r"(?<![A-Za-z0-9_.])([A-Za-z0-9_]+):/")


def retained_schemas(path: Path = RETAINED_FILE) -> list[str]:
    """读保留清单（`#` 注释与空行忽略）。顺序就是 schema_list 的顺序。"""
    ids: list[str] = []
    for raw in Path(path).read_text(encoding="utf-8").splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            ids.append(line)
    if not ids:
        raise SystemExit("%s 里没有任何方案" % path)
    return ids


def schema_index(rime_data: Path = RIME_DATA) -> dict[str, Path]:
    index: dict[str, Path] = {}
    for path in sorted(Path(rime_data).glob("**/*.schema.yaml")):
        sid = path.name[: -len(SCHEMA_SUFFIX)]
        if sid in index:
            raise SystemExit("方案 id 重复：%s（%s / %s）" % (sid, index[sid], path))
        index[sid] = path
    return index


def schema_refs(text: str) -> tuple[list[str], list[str], list[str], list[str]]:
    """返回 (schema 级 __include 目标, dependencies, dictionary 名称, 资源名)。

    资源名来自 `__patch: pinyin:/abbreviation` 这类引用：librime 部署时会把
    `pinyin.yaml` 当独立资源加载，不发出去就会报
    `resource could not be loaded: pinyin`（实测）。
    """
    includes: list[str] = []
    dependencies: list[str] = []
    dictionaries: list[str] = []
    resources: list[str] = []
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        match = _INCLUDE_RE.match(line)
        if match:
            includes.append(match.group(1))
            i += 1
            continue
        match = _DEPENDENCIES_RE.match(line)
        if match:
            base_indent = len(match.group(1))
            i += 1
            while i < len(lines):
                item = lines[i]
                stripped = item.strip()
                if not stripped or stripped.startswith("#"):
                    i += 1
                    continue
                indent = len(item) - len(item.lstrip(" "))
                if indent <= base_indent:
                    break
                dep = _ITEM_RE.match(item)
                if dep:
                    dependencies.append(dep.group(1))
                i += 1
            continue
        match = _DICTIONARY_RE.match(line)
        if match and not line.lstrip().startswith("#"):
            name = match.group(1)
            if name and name not in ("''", '""'):
                dictionaries.append(name)
        i += 1
    for raw in lines:
        if raw.lstrip().startswith("#"):
            continue
        for match in _RESOURCE_RE.finditer(raw):
            resources.append(match.group(1))
    return includes, dependencies, dictionaries, resources


def dependency_closure(
    ids: list[str], index: dict[str, Path] | None = None
) -> tuple[list[str], list[str]]:
    """把保留方案展开成 (schema 闭包, 数据名称集合)。

    schema 闭包 = 保留方案 + `dependencies` + 根级 `__include: <x>.schema:/`
    （如 luna_pinyin_simp 继承 luna_pinyin）。数据名称集合额外带上 `dictionary`
    引用的码表（如 wubi_pinyin 的 dictionary: wubi86）。
    """
    index = schema_index() if index is None else index
    schemas: list[str] = []
    queue = list(ids)
    dictionaries: set[str] = set()
    while queue:
        sid = queue.pop(0)
        if sid in schemas:
            continue
        path = index.get(sid)
        if path is None:
            raise SystemExit("方案 %s 没有随包 schema 文件" % sid)
        schemas.append(sid)
        includes, deps, dicts, resources = schema_refs(path.read_text(encoding="utf-8"))
        dictionaries.update(dicts)
        dictionaries.update(resources)
        for dep in deps + includes:
            if dep not in schemas:
                queue.append(dep)
    names = list(schemas) + sorted(dictionaries - set(schemas))
    return schemas, names


def direct_dependencies(
    ids: list[str], index: dict[str, Path] | None = None
) -> list[str]:
    """保留方案直接声明的依赖（部署器保证会产出这些 build 文件）。"""
    index = schema_index() if index is None else index
    out: list[str] = []
    for sid in ids:
        path = index.get(sid)
        if path is None:
            raise SystemExit("方案 %s 没有随包 schema 文件" % sid)
        _, deps, _, _ = schema_refs(path.read_text(encoding="utf-8"))
        for dep in deps:
            if dep not in ids and dep not in out:
                out.append(dep)
    return out


def dependencies_closure(
    ids: list[str], index: dict[str, Path] | None = None
) -> list[str]:
    """只沿 `dependencies` 展开（部署器会为这些方案产出 build/<id>.schema.yaml）。

    与 dependency_closure 的区别：`__include` 只是配置继承（luna_pinyin_simp 继承
    luna_pinyin.schema），被继承的方案本身不一定会被部署；`dependencies` 才会。
    """
    index = schema_index() if index is None else index
    seen: list[str] = []
    queue = list(ids)
    while queue:
        sid = queue.pop(0)
        if sid in seen:
            continue
        path = index.get(sid)
        if path is None:
            raise SystemExit("方案 %s 没有随包 schema 文件" % sid)
        seen.append(sid)
        _, deps, _, _ = schema_refs(path.read_text(encoding="utf-8"))
        for dep in deps:
            if dep not in seen:
                queue.append(dep)
    return seen


def data_files(
    ids: list[str], rime_data: Path = RIME_DATA, index: dict[str, Path] | None = None
) -> list[Path]:
    """保留方案（含依赖）需要随包分发的 data 文件（平铺后的源文件）。"""
    schemas, names = dependency_closure(ids, index)
    keep = set(names)
    out: list[Path] = []
    for recipe in sorted(p for p in Path(rime_data).iterdir() if p.is_dir()):
        prelude = recipe.name == "rime-prelude"
        for path in sorted(p for p in recipe.iterdir() if p.is_file()):
            if path.suffix not in (".yaml", ".txt"):
                continue
            if prelude or path.suffix == ".txt":
                # 预设（default/key_bindings/punctuation/symbols）与词频文本无条件带上
                out.append(path)
                continue
            name = path.name
            if name.endswith(SCHEMA_SUFFIX):
                if name[: -len(SCHEMA_SUFFIX)] in set(schemas):
                    out.append(path)
                continue
            stem = name[: -len(DICT_SUFFIX)] if name.endswith(DICT_SUFFIX) else path.stem
            first = stem.split(".", 1)[0]
            if first in keep:
                out.append(path)
    return out


def trim_default_yaml(text: str, ids: list[str]) -> str:
    """把 rime-prelude/default.yaml 的 schema_list 改写成保留清单。"""
    lines = text.splitlines()
    try:
        start = lines.index("schema_list:")
    except ValueError:
        raise SystemExit("default.yaml 里没有根级 schema_list:")
    end = start + 1
    while end < len(lines) and lines[end].startswith("  - "):
        end += 1
    block = ["schema_list:"] + ["  - schema: %s" % sid for sid in ids]
    return "\n".join(lines[:start] + block + lines[end:]) + "\n"


def install_data(
    out_dir: Path,
    ids: list[str] | None = None,
    rime_data: Path = RIME_DATA,
    linux_only: bool = True,
) -> list[Path]:
    """把 data_files 平铺复制到 out_dir，并写入裁剪过的 default.yaml。"""
    ids = retained_schemas() if ids is None else ids
    index = schema_index(rime_data)
    files = data_files(ids, rime_data, index)
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    written: list[Path] = []
    for src in files:
        dst = out_dir / src.name
        if src.name == "default.yaml":
            dst.write_text(
                trim_default_yaml(src.read_text(encoding="utf-8"), ids),
                encoding="utf-8",
            )
        else:
            shutil.copyfile(src, dst)
        written.append(dst)
    if linux_only and not (out_dir / "essay.txt").is_file():
        raise SystemExit("%s 里没有 essay.txt（rime-essay 没有随包）" % rime_data)
    return written


if __name__ == "__main__":
    # 调试入口：打印清单与闭包
    ids = retained_schemas()
    schemas, names = dependency_closure(ids)
    print("retained:", " ".join(ids))
    print("closure: ", " ".join(schemas))
    print("data:    ", " ".join(p.name for p in data_files(ids)))
