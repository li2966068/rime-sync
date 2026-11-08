#!/usr/bin/env python3
"""gen-prebuilt-build.py —— 生成/校验随安装包分发的「预编译 build 产物」。

为什么需要它
------------
有些 Windows 机器上 WeaselDeployer.exe /deploy 返回 0 但什么都不写
（%APPDATA%\\Rime\\{user.yaml,installation.yaml,build} 一直停在旧日期），
手动起 WeaselServer 也一样。这种机器上「本机部署」永远不生效，安装器只能把
别处已经部署好的 build/ 产物直接铺进用户目录。

产物内容（3 个保留方案 + 自动解析的依赖）
-----------------------------------------
  build/    保留方案（packaging/rime/retained-schemas.txt：luna_pinyin_simp /
            wubi_pinyin / wubi86）的 *.schema.yaml + *.prism.bin + *.table.bin
            + *.reverse.bin，以及 schema 里 dependencies 声明所必需的依赖方案
            产物（自动递归解析，当前为 luna_pinyin / pinyin_simp / stroke）；
            另加部署器写出的 build/default.yaml。
  config/   生成这套产物时用的 default.custom.yaml（schema_list 已裁剪到保留
            方案）+ 各保留方案的 <schema>.custom.yaml。

依赖闭包只有一份实现（scripts/ime/rime_common.py），deb 打包
（scripts/ime/gen-rime-data.py）用的是同一个函数——预编译生成成功就证明安装包
里那份 data 集合是自足的。
  MANIFEST.json   文件清单（大小 / sha256）+ 方案表 + 体积 + 生成器版本。

用法
----
  scripts/ime/gen-prebuilt-build.py                  # 真实部署并写出 packaging/rime/prebuilt
  scripts/ime/gen-prebuilt-build.py --check          # CI：校验产物与 schema 一致性（不需要 docker）
  scripts/ime/gen-prebuilt-build.py --schemas luna_pinyin --out /tmp/p   # 只要一套

部署在容器里做（镜像里没有 rime_deployer，但仓库 build/ime-staging/<发行版>/root
有 scripts/ime/build-linux.sh 编出来的 deployer 与 librime），也可以用
--no-docker 在宿主上跑（宿主 glibc 与该 staging 一致时）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import rime_common

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = REPO_ROOT / "packaging" / "rime" / "prebuilt"
# 单一真相源：packaging/rime/retained-schemas.txt（Rust 侧 include_str! 同一份）。
DEFAULT_SCHEMAS = tuple(rime_common.retained_schemas())
DEFAULT_BUDGET_MB = 60.0
DEFAULT_IMAGE = "rime-ime-builder:debian_13"

TEMPLATE_DIR = REPO_ROOT / "packaging" / "rime"
SCHEMA_CUSTOM_DIR = TEMPLATE_DIR / "schemas"
RIME_DATA_DIR = REPO_ROOT / "third_party" / "rime-data"
STAGING_ROOT = REPO_ROOT / "build" / "ime-staging"

# 2021-01-01T00:00:00Z：把输入文件的 mtime 钉死，重复生成得到逐字节相同的产物
# （__build_info.timestamps 记的就是源文件 mtime，不钉死每次都不一样）。
FIXED_MTIME = 1609459200

# 这层必须自带 .gitignore：仓库根的 `build/` 会把预编译产物整个吞掉，而产物必须入库。
# 以前它是手工维护的文件，生成器 rmtree(out_dir) 时会连它一起删掉（--check 也发现不了），
# 0.6.21 起由生成器自己写、--check 自己断言。
GITIGNORE = """# 仓库根的 .gitignore 有 `build/`（匹配任意层级的 build 目录，用于忽略构建产物），
# 它会把这里的预编译产物目录一起吞掉。这份产物**必须入库**（随安装包分发，
# 见 scripts/ime/gen-prebuilt-build.py），所以在这一层显式反选。
#
# 反选的是目录本身；只要目录被重新纳入，里面的文件就不再受根的 build/ 规则影响。
!build/
"""

# 装到用户目录后必须能在 build/*.schema.yaml 里找到的插件标记（0.6.15 的验收点）。
MARKERS = ("rimesync_vnum", "rimesync_punct")

# 部署器在 build/ 里产出的文件后缀；其余文件一律不随包（避免把临时文件带出去）。
PRODUCT_SUFFIXES = (".schema.yaml", ".prism.bin", ".table.bin", ".reverse.bin")


def log(msg: str) -> None:
    print(msg, flush=True)


def die(msg: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"FAIL {msg}", file=sys.stderr, flush=True)
    raise SystemExit(1)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def human(n: int) -> str:
    return f"{n / (1024 * 1024):.1f} MB"


# --------------------------------------------------------------------------- 输入
def find_deployer(explicit: str | None) -> tuple[Path, Path | None]:
    """返回 (rime_deployer 路径, librime 所在 libdir 或 None)。"""
    if explicit:
        p = Path(explicit).expanduser().resolve()
        if not p.is_file():
            die(f"--deployer {p} 不存在")
        return p, guess_libdir(p)
    env = os.environ.get("RIMESYNC_PREBUILT_DEPLOYER")
    if env:
        p = Path(env).expanduser().resolve()
        if not p.is_file():
            die(f"RIMESYNC_PREBUILT_DEPLOYER={env} 不存在")
        return p, guess_libdir(p)
    # 仓库里 build-linux.sh 编出来的 staging 优先（debian-13 与镜像一致，排第一）
    candidates = list(STAGING_ROOT.glob("*/root/usr/bin/rime_deployer"))
    candidates.sort(key=lambda p: (p.relative_to(STAGING_ROOT).parts[0] != "debian-13", str(p)))
    for cand in candidates:
        return cand, guess_libdir(cand)
    which = shutil.which("rime_deployer")
    if which:
        return Path(which), None
    die(
        "找不到 rime_deployer：先跑 scripts/ime/build-linux.sh（产物在 "
        "build/ime-staging/<distro>/root/usr/bin/），或用 --deployer/--no-docker 指定"
    )


def guess_libdir(deployer: Path) -> Path | None:
    """staging 布局：<root>/usr/bin/rime_deployer，库在同级 ../lib/<triplet>。"""
    # <root>/usr/bin/rime_deployer -> <root>/usr/lib/<triplet>/librime.so*
    root = deployer.parent.parent.parent
    for lib_root in (root / "usr" / "lib", root / "lib", root / "usr" / "lib64"):
        if not lib_root.is_dir():
            continue
        for libdir in sorted(lib_root.iterdir()):
            if (libdir / "librime.so").exists() or list(libdir.glob("librime.so.*")):
                return libdir
    return None


def prepare_shared(dest: Path) -> int:
    """把保留方案（含依赖闭包）需要的 *.yaml / *.txt 平铺进共享数据目录。

    与 deb 打包（scripts/ime/gen-rime-data.py）共用 rime_common 的闭包实现；
    default.yaml 的 schema_list 同步裁剪到保留清单——部署器把缺失 schema 当失败，
    上游那份 16 方案的清单不能原样带进来。
    """
    if not RIME_DATA_DIR.is_dir():
        die(f"缺少 {RIME_DATA_DIR}（随包的 rime 数据）")
    try:
        files = rime_common.install_data(dest, list(DEFAULT_SCHEMAS), RIME_DATA_DIR)
    except SystemExit as exc:
        die(str(exc))
    if not files:
        die(f"{RIME_DATA_DIR} 里没有保留方案需要的数据")
    return len(files)


def reduced_default_custom(schemas: tuple[str, ...]) -> str:
    """packaging/rime/default.custom.yaml 的裁剪版：schema_list 只留给定方案。

    其余 patch（menu/page_size、rimesync/punctuator_primary…）原样保留，否则
    预编译产物与随包默认配置就不是同一个配置生成的了。
    """
    src = (TEMPLATE_DIR / "default.custom.yaml").read_text(encoding="utf-8")
    lines = src.splitlines()
    try:
        start = lines.index("  schema_list:")
    except ValueError:
        die("packaging/rime/default.custom.yaml 里找不到 '  schema_list:'")
    end = start + 1
    while end < len(lines) and lines[end].startswith("    - "):
        end += 1
    wanted = set(schemas)
    kept, dropped = [], []
    for line in lines[start + 1 : end]:
        sid = line.split("{schema:")[-1].rstrip("} ").strip()
        (kept if sid in wanted else dropped).append(line)
    missing = wanted - {
        line.split("{schema:")[-1].rstrip("} ").strip() for line in lines[start + 1 : end]
    }
    if missing:
        die(f"default.custom.yaml 的 schema_list 里没有这些方案：{sorted(missing)}")
    header = [
        "# 由 scripts/ime/gen-prebuilt-build.py 生成的预编译包配置（不要手改）。",
        f"# schema_list 已裁剪到预编译产物里的方案：{', '.join(schemas)}；",
        "# 其余 patch 与 packaging/rime/default.custom.yaml 完全一致再部署。",
    ]
    out = lines[:start] + header + ["  schema_list:"] + kept + lines[end:]
    return "\n".join(out) + "\n"


def prepare_user(dest: Path, schemas: tuple[str, ...]) -> None:
    """生成 payload 的 config/（A-4：config 模板的**唯一一份源**）。

    目录是扁平的：default.custom.yaml / weasel.custom.yaml / <schema>.custom.yaml。
    程序的 seed_templates 与 install_prebuilt 都从这一份读（Windows 资源目录里
    不再另拷一份 resources/schemas/）。

    weasel.custom.yaml 只影响外观（候选横向排列），但它也得在这儿：种子路径
    （seed_templates）读的就是这个目录，少一份就播不出去。
    """
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "default.custom.yaml").write_text(reduced_default_custom(schemas), encoding="utf-8")
    weasel = TEMPLATE_DIR / "weasel.custom.yaml"
    if not weasel.is_file():
        die(f"缺少 {weasel}（外观模板必须随包提供）")
    shutil.copyfile(weasel, dest / weasel.name)
    for schema in schemas:
        src = SCHEMA_CUSTOM_DIR / f"{schema}.custom.yaml"
        if not src.is_file():
            die(f"缺少 {src}（打包的方案必须自带 custom 模板）")
        shutil.copyfile(src, dest / src.name)


def pin_mtimes(root: Path) -> None:
    for path in sorted(root.rglob("*")):
        if path.is_file():
            os.utime(path, (FIXED_MTIME, FIXED_MTIME))


# --------------------------------------------------------------------------- 部署
def to_container_path(host_path: Path) -> str:
    try:
        return "/src/" + str(host_path.resolve().relative_to(REPO_ROOT))
    except ValueError:
        return str(host_path)


def run_deployer(
    deployer: Path,
    libdir: Path | None,
    user_dir: Path,
    shared_dir: Path,
    *,
    use_docker: bool,
    image: str,
) -> tuple[int, str]:
    if use_docker:
        args = [
            "docker", "run", "--rm",
            "--user", f"{os.getuid()}:{os.getgid()}",
            "-e", "HOME=/tmp",
            "-v", f"{REPO_ROOT}:/src:ro",
            "-v", f"{user_dir.parent}:/scratch",
            "-w", "/scratch",
            image, "bash", "-lc",
        ]
        inner = []
        lib_inner = to_container_path(libdir) if libdir else None
        if lib_inner:
            inner.append(f"export LD_LIBRARY_PATH={lib_inner}")
        inner.append(
            f'exec "{to_container_path(deployer)}" --build '
            f'"/scratch/{user_dir.name}" "/scratch/{shared_dir.name}"'
        )
        args.append(" && ".join(inner))
        cmd = args
    else:
        env = dict(os.environ)
        if libdir:
            env["LD_LIBRARY_PATH"] = f"{libdir}:{env.get('LD_LIBRARY_PATH', '')}".rstrip(":")
        cmd = [str(deployer), "--build", str(user_dir), str(shared_dir)]
        env["HOME"] = env.get("HOME") or "/tmp"
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=3600,
                              env=env if not use_docker else None)
    except FileNotFoundError:
        die("docker 不可用；改用 --no-docker 或先安装 docker")
    out = (proc.stdout + proc.stderr).strip().splitlines()
    tail = "\n".join(out[-20:])
    return proc.returncode, tail


def collect_products(build_dir: Path) -> list[Path]:
    files = []
    for f in sorted(build_dir.iterdir()):
        if not f.is_file():
            continue
        # build/default.yaml 是部署器编译出来的默认配置（含 schema_list），
        # 少了它引擎启动时找不到方案表，必须一起铺。
        if f.name == "default.yaml" or any(f.name.endswith(s) for s in PRODUCT_SUFFIXES):
            files.append(f)
    return files


def probe_markers(build_dir: Path) -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    for name in sorted(os.listdir(build_dir)):
        if not name.endswith(".schema.yaml"):
            continue
        text = (build_dir / name).read_text(encoding="utf-8", errors="replace")
        found[name] = [m for m in MARKERS if m in text]
    return found


# --------------------------------------------------------------------------- 生成
def generate(args: argparse.Namespace) -> int:
    out_dir = Path(args.out).resolve()
    budget = int(args.budget_mb * 1024 * 1024)
    schemas = tuple(s.strip() for s in args.schemas.split(",") if s.strip())
    if not schemas:
        die("--schemas 为空")
    use_docker = not args.no_docker
    deployer, libdir = find_deployer(args.deployer)
    log(f"deployer: {deployer}")
    if libdir:
        log(f"librime:  {libdir}")
    log(f"docker:   {DEFAULT_IMAGE if use_docker else 'off（直接在宿主跑）'}")

    scratch = Path(tempfile.mkdtemp(prefix="rime-prebuilt."))
    try:
        shared = scratch / "shared"
        user = scratch / "user"
        log(f"共享数据：{prepare_shared(shared)} 个 *.yaml/*.txt（third_party/rime-data）")

        attempt = list(schemas)
        while True:
            shutil.rmtree(user, ignore_errors=True)
            shutil.rmtree(shared / "build", ignore_errors=True)
            prepare_user(user, tuple(attempt))
            pin_mtimes(shared)
            pin_mtimes(user)
            log(f"部署：{', '.join(attempt)} -> rime_deployer --build <user> <shared>")
            rc, tail = run_deployer(deployer, libdir, user, shared,
                                    use_docker=use_docker, image=args.image)
            if rc != 0:
                log(tail)
                die(f"rime_deployer 失败（exit {rc}）")
            build_dir = user / "build"
            if not build_dir.is_dir():
                die("rime_deployer 没有产出 build/")
            products = collect_products(build_dir)
            total = sum(p.stat().st_size for p in products)
            log(f"产物：{len(products)} 个文件，{human(total)}")
            if total <= budget:
                break
            # 保留清单是发布契约：不许为了过预算偷偷砍掉清单里的方案。
            die(f"{human(total)} 超过预算 {human(budget)}：提高 --budget-mb，"
                "或先评估是否真的需要这么多方案（清单在 packaging/rime/retained-schemas.txt）")

        # 产物 = 保留方案 + dependencies 闭包（部署器会为依赖方案也产出 build 文件）；
        # 多一个都算错——预编译包必须只剩保留清单与它的依赖。
        closure = rime_common.dependencies_closure(list(attempt))
        # 部署器只为保留方案 + 它们**直接**声明的依赖产出 build 文件（深层依赖
        # 只保证源码在 shared 里，供 __include/资源加载），所以这里断言到直接依赖层。
        direct = rime_common.direct_dependencies(list(attempt))
        missing = [s for s in list(attempt) + direct
                   if not (build_dir / f"{s}.schema.yaml").is_file()]
        if missing:
            die(f"缺少方案产物：{missing}（保留 {list(attempt)}，直接依赖 {direct}）")
        extra = sorted(
            name[: -len(".schema.yaml")]
            for name in os.listdir(build_dir)
            if name.endswith(".schema.yaml")
            and name[: -len(".schema.yaml")] not in closure
        )
        if extra:
            die(f"产物里有多余方案：{extra}（只允许 {closure}）")
        markers = probe_markers(build_dir)
        for schema in attempt:
            name = f"{schema}.schema.yaml"
            want = set(MARKERS)
            got = set(markers.get(name, []))
            if want - got:
                die(f"{name} 缺少插件标记 {sorted(want - got)}"
                    "（packaging/rime/schemas/*.custom.yaml 没生效？）")

        # 铺制自检：把 prebuilt 当成安装后用户目录里的东西，再让部署器/引擎跑一次。
        if not args.skip_verify:
            verify_prebuilt(deployer, libdir, scratch / "verify", build_dir,
                            user, attempt, use_docker=use_docker, image=args.image,
                            smoke=args.smoke, no_smoke=args.no_smoke)

        write_payload(out_dir, build_dir, user, attempt, deployer, use_docker,
                      budget, total, markers)
        log(f"写出：{out_dir}")
        return 0
    finally:
        if not args.keep_scratch:
            shutil.rmtree(scratch, ignore_errors=True)
        else:
            log(f"保留临时目录：{scratch}")


def verify_prebuilt(deployer: Path, libdir: Path | None, root: Path, build_dir: Path,
                    user_src: Path, schemas: list[str], *, use_docker: bool,
                    image: str, smoke: str | None, no_smoke: bool) -> None:
    """把产物当成「已铺好的用户目录」：不部署也要能打字，部署器也得认这套产物。"""
    root.mkdir(parents=True, exist_ok=True)
    user = root / "user"
    shutil.copytree(build_dir, user / "build")
    for name in os.listdir(user_src):
        if name.endswith(".custom.yaml") or name == "default.custom.yaml":
            shutil.copyfile(user_src / name, user / name)
    shared = root / "shared"
    prepare_shared(shared)
    pin_mtimes(root)

    # 1) 最关键的一条：完全不做部署，只用 prebuilt/build 里的产物建会话并打出「你好」。
    #    这正是「本机部署永远不生效」的机器上的运行方式。
    if not no_smoke:
        run_smoke(root, shared, user, deployer=deployer, libdir=libdir,
                  use_docker=use_docker, image=image, explicit=smoke)

    # 2) 部署器也得认这套产物（用户以后在本机能部署时，不会把它当成坏目录）。
    rc, tail = run_deployer(deployer, libdir, user, shared,
                            use_docker=use_docker, image=image)
    if rc != 0:
        log(tail)
        die(f"铺制自检：rime_deployer 在预编译产物上失败（exit {rc}）")
    for schema in schemas:
        if not (user / "build" / f"{schema}.schema.yaml").is_file():
            die(f"铺制自检：{schema}.schema.yaml 不见了")
    markers = probe_markers(user / "build")
    for schema in schemas:
        got = set(markers.get(f"{schema}.schema.yaml", []))
        if set(MARKERS) - got:
            die(f"铺制自检：{schema}.schema.yaml 丢了插件标记")
    log("铺制自检：rime_deployer 在预编译产物上 exit 0，标记完整")


def docker_run(scratch_root: Path, image: str, inner: list[str], *,
               what: str, timeout: int = 900) -> subprocess.CompletedProcess:
    cmd = [
        "docker", "run", "--rm",
        "--user", f"{os.getuid()}:{os.getgid()}",
        "-e", "HOME=/tmp",
        "-v", f"{REPO_ROOT}:/src:ro",
        "-v", f"{scratch_root}:/scratch",
        "-w", "/scratch",
        image, "bash", "-lc", " && ".join(inner),
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except FileNotFoundError:
        die("docker 不可用；去掉 --no-docker 之外的选项后重试，或先安装 docker")
    if proc.returncode != 0:
        log("\n".join((proc.stdout + proc.stderr).strip().splitlines()[-10:]))
        die(f"{what} 失败（exit {proc.returncode}）")
    return proc


def compile_smoke(root: Path, deployer: Path, libdir: Path | None, image: str) -> Path:
    """容器里现编 rime-smoke（复用 scripts/ime/rime-smoke.cc），不依赖旧二进制。"""
    staging_root = deployer.parent.parent.parent
    incdir = staging_root / "usr" / "include"
    if not (incdir / "rime_api.h").is_file():
        die(f"找不到 rime_api.h（{incdir}）；用 --smoke 指向现成的 rime-smoke")
    inner = [f"export LD_LIBRARY_PATH={to_container_path(libdir)}" if libdir else "true"]
    libdir_arg = to_container_path(libdir) if libdir else "/usr/lib"
    inner.append(
        'g++ -O2 -std=c++11 -o /scratch/rime-smoke "/src/scripts/ime/rime-smoke.cc" '
        f"-I{to_container_path(incdir)} -L{libdir_arg} -lrime"
    )
    docker_run(root, image, inner, what="容器里编译 rime-smoke")
    return root / "rime-smoke"


def run_smoke(root: Path, shared: Path, user: Path, *, deployer: Path,
              libdir: Path | None, use_docker: bool, image: str,
              explicit: str | None) -> None:
    if explicit:
        smoke_bin = Path(explicit).expanduser().resolve()
        if not smoke_bin.is_file():
            die(f"--smoke {smoke_bin} 不存在")
    elif use_docker:
        smoke_bin = compile_smoke(root, deployer, libdir, image)
    else:
        found = shutil.which("rime-smoke")
        if not found:
            die("宿主上没有 rime-smoke；用 --smoke 指定，或去掉 --no-docker")
        smoke_bin = Path(found)

    shared_in = "/scratch/" + str(shared.relative_to(root))
    user_in = "/scratch/" + str(user.relative_to(root))
    if use_docker:
        try:
            smoke_in = "/scratch/" + str(smoke_bin.relative_to(root))
        except ValueError:
            if not smoke_bin.is_relative_to(REPO_ROOT):
                die(f"--smoke {smoke_bin} 既不在仓库里也不在本次临时目录里，容器看不到它")
            smoke_in = to_container_path(smoke_bin)
        inner = [f"export LD_LIBRARY_PATH={to_container_path(libdir)}" if libdir else "true"]
        inner.append(f'exec "{smoke_in}" "{shared_in}" "{user_in}" --no-deploy')
        proc = docker_run(root, image, inner, what="铺制自检：rime-smoke")
        out = (proc.stdout + proc.stderr).strip()
    else:
        env = dict(os.environ)
        if libdir:
            env["LD_LIBRARY_PATH"] = f"{libdir}:{env.get('LD_LIBRARY_PATH', '')}".rstrip(":")
        proc = subprocess.run([str(smoke_bin), str(shared), str(user), "--no-deploy"],
                              capture_output=True, text=True, timeout=1800, env=env)
        out = (proc.stdout + proc.stderr).strip()
    if proc.returncode != 0 or "PASS: candidate" not in out:
        log("\n".join(out.splitlines()[-8:]))
        die(f"铺制自检：rime-smoke 没通过（exit {proc.returncode}）")
    log("铺制自检：rime-smoke 不部署、只用预编译产物 PASS（luna_pinyin 打出 你好）")

def write_payload(out_dir: Path, build_dir: Path, user_src: Path, schemas: list[str],
                  deployer: Path, use_docker: bool, budget: int, total: int,
                  markers: dict[str, list[str]]) -> None:
    if out_dir.exists():
        shutil.rmtree(out_dir)
    (out_dir / "build").mkdir(parents=True)
    (out_dir / "config").mkdir(parents=True)
    (out_dir / ".gitignore").write_text(GITIGNORE, encoding="utf-8")
    for src in collect_products(build_dir):
        dst = out_dir / "build" / src.name
        shutil.copyfile(src, dst)
    for name in sorted(os.listdir(user_src)):
        if name.endswith(".custom.yaml") or name == "default.custom.yaml":
            dst = out_dir / "config" / name
            shutil.copyfile(user_src / name, dst)
    pin_mtimes(out_dir)
    # 清单必须在**最终落盘的内容**上算：产物文件全部写完之后重新读一遍磁盘再算
    # 大小/sha256。复制过程中的中间值不算数 —— 之后任何一次改写（rimesync 补丁、
    # 部署器重写 build/<id>.schema.yaml、固定 mtime 之后的任何编辑）都会让清单
    # 与产物对不上，而对不上的清单装到机器上就是「自相矛盾」的兜底产物（首启直接
    # 降级成 partial/degraded）。
    files = manifest_entries(out_dir)
    # 清单里的 total_bytes 是「整包」体积（build + config），与 --check 的算法一致。
    total = sum(entry["bytes"] for entry in files)
    rime_version = ""
    head = (out_dir / "build" / f"{schemas[0]}.schema.yaml").read_text(
        encoding="utf-8", errors="replace").splitlines()[:6]
    for line in head:
        if "rime_version:" in line:
            rime_version = line.split("rime_version:", 1)[1].strip()
            break
    manifest = {
        "schema": 1,
        "generator": "scripts/ime/gen-prebuilt-build.py",
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "rime_version": rime_version,
        "deployer": str(deployer.relative_to(REPO_ROOT)) if deployer.is_relative_to(REPO_ROOT) else str(deployer),
        "docker_image": DEFAULT_IMAGE if use_docker else None,
        "schemas": list(schemas),
        "retained_schemas": list(schemas),
        "dependencies": sorted(
            set(rime_common.dependencies_closure(list(schemas))) - set(schemas)
        ),
        "markers": markers,
        "budget_bytes": budget,
        "total_bytes": total,
        "files": files,
    }
    (out_dir / "MANIFEST.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    # 自检：清单刚落盘就与产物逐字节对一次，不许带着矛盾出门（见 verify_manifest）。
    fails = verify_manifest(out_dir)
    if fails:
        die("MANIFEST.json 与产物不一致：\n  " + "\n  ".join(fails))
    log(f"体积：{human(total)}（预算 {human(budget)}），"
        f"方案：{', '.join(schemas)}，rime {rime_version or '?'}")


def manifest_entries(out_dir: Path) -> list[dict]:
    """按**最终落盘的内容**算清单条目（大小 + sha256）。

    清单必须描述最终产物：任何在复制之后发生的改写（rimesync 补丁、部署器重写
    build/<id>.schema.yaml、固定 mtime 之后的编辑）都会让复制时的中间值失效。
    """
    entries: list[dict] = []
    for sub in ("build", "config"):
        base = out_dir / sub
        if not base.is_dir():
            continue
        for path in sorted(base.iterdir()):
            if not path.is_file():
                continue
            entries.append({"path": f"{sub}/{path.name}", "bytes": path.stat().st_size,
                            "sha256": sha256_file(path)})
    return entries


def verify_manifest(out_dir: Path) -> list[str]:
    """MANIFEST.json 与产物逐字节对照（大小 + sha256）。

    返回不一致的条目，每条都写清**文件 / 期望 / 实际**：
    清单与产物对不上 = 产物自相矛盾 = 装到机器上首启就会降级（partial/degraded）。
    """
    fail: list[str] = []
    manifest_path = out_dir / "MANIFEST.json"
    if not manifest_path.is_file():
        return [f"{manifest_path} 不存在（产物没有清单就无法自检）"]
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return [f"MANIFEST.json 不是合法 JSON：{exc}"]
    listed = {entry["path"]: entry for entry in manifest.get("files", [])}
    if not listed:
        return ["MANIFEST.json 的 files 为空"]
    for rel, entry in sorted(listed.items()):
        path = out_dir / rel
        if not path.is_file():
            fail.append(f"{rel} 缺失（清单 expected={entry.get('bytes')} 字节 "
                        f"sha256={entry.get('sha256')}）")
            continue
        size = path.stat().st_size
        if size != entry.get("bytes"):
            fail.append(f"{rel} 大小 expected={entry.get('bytes')} actual={size}")
            continue
        digest = sha256_file(path)
        if digest != entry.get("sha256"):
            fail.append(f"{rel} sha256 expected={entry.get('sha256')} actual={digest}")
    # 反向：产物里多出来的文件清单没登记 = 清单没描述全这份产物。
    skip_names = ("MANIFEST.json", ".gitignore")
    for path in sorted(out_dir.rglob("*")):
        if path.is_file() and path.name not in skip_names:
            rel = str(path.relative_to(out_dir))
            if rel not in listed:
                fail.append(f"{rel} 未登记在清单里（清单没描述这份产物）")
    return fail


# --------------------------------------------------------------------------- 校验
def check_payload(out_dir: Path, budget: int) -> list[str]:
    fail: list[str] = []
    manifest_path = out_dir / "MANIFEST.json"
    if not manifest_path.is_file():
        return [f"{manifest_path} 不存在（先跑不带 --check 的生成）"]
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return [f"MANIFEST.json 不是合法 JSON：{exc}"]
    # 清单 ↔ 产物逐字节对照（与 --verify-dir 同一份实现）。
    fail.extend(verify_manifest(out_dir))
    # .gitignore 是本目录的入库胶水（反选 prebuilt/build），不算产物
    skip_names = ("MANIFEST.json", ".gitignore")
    if not (out_dir / ".gitignore").is_file():
        fail.append("缺少 .gitignore（仓库根的 build/ 规则会把预编译产物忽略掉，文件进不了仓库）")
    schemas = manifest.get("schemas") or []
    if not schemas:
        fail.append("清单里没有 schemas")
    retained = rime_common.retained_schemas()
    if schemas != retained:
        fail.append(f"清单方案 {schemas} 与 retained-schemas.txt {retained} 不一致"
                    "（改清单后要重新生成 prebuilt）")
    try:
        closure = rime_common.dependencies_closure(list(schemas))
    except SystemExit as exc:
        closure = []
        fail.append(str(exc))
    try:
        direct = rime_common.direct_dependencies(list(schemas))
    except SystemExit as exc:
        direct = []
        fail.append(str(exc))
    for schema in list(schemas) + direct:
        if not (out_dir / "build" / f"{schema}.schema.yaml").is_file():
            fail.append(f"缺少方案产物 build/{schema}.schema.yaml")
    build_dir = out_dir / "build"
    if build_dir.is_dir():
        for path in sorted(build_dir.glob("*.schema.yaml")):
            stem = path.name[: -len(".schema.yaml")]
            if closure and stem not in closure:
                fail.append(f"多余方案产物 build/{path.name}（只允许保留方案与依赖 {closure}）")
    config_dir = out_dir / "config"
    if config_dir.is_dir():
        for path in sorted(config_dir.glob("*.custom.yaml")):
            stem = path.name[: -len(".custom.yaml")]
            # A-4：config/ 是**唯一一份** config 源（扁平布局）：
            #   default.custom.yaml（写 schema_list）/ weasel.custom.yaml（外观）
            #   / <保留方案>.custom.yaml
            if stem not in ("default", "weasel") and stem not in schemas:
                fail.append(f"多余配置模板 config/{path.name}（只允许 default/weasel 与保留方案 {schemas}）")
        if not (config_dir / "weasel.custom.yaml").is_file():
            fail.append("缺少 config/weasel.custom.yaml（A-4：种子路径要靠它播外观模板）")
    for schema in schemas:
        name = f"build/{schema}.schema.yaml"
        path = out_dir / name
        if not path.is_file():
            fail.append(f"缺少 {name}")
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for marker in MARKERS:
            if marker not in text:
                fail.append(f"{name} 里没有 {marker}")
        for bad in ("/usr/share/rime-data", "/src/", "/scratch/", "/tmp/", str(REPO_ROOT)):
            if bad in text:
                fail.append(f"{name} 泄漏了绝对路径 {bad}（产物不可移植）")
    default_custom = out_dir / "config" / "default.custom.yaml"
    if not default_custom.is_file():
        fail.append("缺少 config/default.custom.yaml")
    else:
        text = default_custom.read_text(encoding="utf-8")
        listed_schemas = [
            line.split("{schema:")[-1].rstrip("} ").strip()
            for line in text.splitlines() if line.startswith("    - ") and "{schema:" in line
        ]
        if listed_schemas != list(schemas):
            fail.append(f"config/default.custom.yaml 的 schema_list {listed_schemas} "
                        f"与清单方案 {schemas} 不一致")
    total = manifest.get("total_bytes")
    if isinstance(total, int):
        if total > budget:
            fail.append(f"产物 {human(total)} 超过预算 {human(budget)}")
        actual = sum(p.stat().st_size for p in out_dir.rglob("*")
                     if p.is_file() and p.name not in skip_names)
        if actual != total:
            fail.append(f"实际体积 {actual} 与清单 total_bytes {total} 不一致")
    return fail


def check_git_ignore(out_dir: Path) -> list[str]:
    """产物必须能入库：仓库根的 .gitignore 有 `build/`，会吞掉 prebuilt/build。

    （packaging/rime/prebuilt/.gitignore 里的 `!build/` 就是为此存在的；
    改坏了要在这里立刻发现，否则 CI 构建出来的包会静默缺少兜底产物。）
    """
    # 直接测目录本身：只测文件测不出「目录被 build/ 规则忽略」，那正是最容易回归的情形。
    sample = next(iter(sorted(out_dir.glob("build"))), None)
    if sample is None:
        return []
    try:
        proc = subprocess.run(
            ["git", "-C", str(REPO_ROOT), "check-ignore", "-q", str(sample)],
            capture_output=True, text=True)
    except FileNotFoundError:
        return []
    if proc.returncode == 0:
        return [f"{sample.relative_to(REPO_ROOT)} 被 .gitignore 忽略"
                "（预编译产物必须入库才能随安装包分发）"]
    return []


def check_wiring() -> list[str]:
    """断言打包侧真的接上了这套产物（改坏了会在 CI 里立刻红）。"""
    fail: list[str] = []

    def has(path: Path, needles: list[str]) -> None:
        if not path.is_file():
            fail.append(f"缺少打包文件 {path.relative_to(REPO_ROOT)}")
            return
        text = path.read_text(encoding="utf-8", errors="replace")
        for needle in needles:
            if needle not in text:
                fail.append(f"{path.relative_to(REPO_ROOT)} 缺少 {needle!r}")

    has(REPO_ROOT / "src-tauri" / "tauri.conf.json",
        ['"../packaging/rime/prebuilt/"', '"resources/prebuilt/"'])
    # 0.6.24（B 方案）：NSIS 钩子**不再**碰输入法 / 产物 / 裁剪 ——
    # 一切都由程序侧的 ensure(Install/Bootstrap) 负责。断言方向因此**反转**：
    # 钩子里出现这些名字就说明「输入法逻辑又被搬回安装器」。
    nsis_hooks = REPO_ROOT / "packaging" / "nsis-hooks.nsh"
    if nsis_hooks.is_file():
        text = nsis_hooks.read_text(encoding="utf-8", errors="replace")
        for forbidden in ("RIMESYNC_PREBUILT", "RimeSyncPruneSchemas", "prune-schemas",
                          "phase=PRUNE", "PREBUILT ok", "PREBUILT failed"):
            if forbidden in text:
                fail.append(f"packaging/nsis-hooks.nsh 不该再出现 {forbidden!r}"
                            "（输入法逻辑必须留在程序侧）")
    else:
        fail.append("缺少打包文件 packaging/nsis-hooks.nsh")
    # 程序侧必须真的接住了这份责任。
    has(REPO_ROOT / "crates" / "core" / "src" / "ime_state.rs",
        ["pub fn ensure(", "pub fn install() -> Self", "pub fn bootstrap() -> Self",
         "pub fn seed_templates(", "ime::ensure("])
    for script in ("postinst", "install.sh"):
        has(REPO_ROOT / "packaging" / "linux" / script,
            ["/usr/share/rime-sync/prebuilt", "RIMESYNC_PREBUILT", "seed_prebuilt_build"])
    has(REPO_ROOT / "scripts" / "ime" / "package-ime-deb.sh",
        ["/usr/share/rime-sync/prebuilt", "retained-schemas.txt", "gen-rime-data.py"])
    # 0.6.21：保留清单的清理必须真的接进两端安装器（并且只调同一份 Rust 实现）。
    for script in ("postinst", "install.sh"):
        has(REPO_ROOT / "packaging" / "linux" / script,
            ["prune_extra_schemas", "rime-sync-cli", "retained-schemas.txt"])
    # A-4：config 模板只有一份手写源（packaging/rime/schemas/），
    # 分发副本（prebuilt/config/）由生成器产出，脚本断言两者不漂移。
    if not (REPO_ROOT / "packaging" / "windows" / "check-config-sources.sh").is_file():
        fail.append("缺少 packaging/windows/check-config-sources.sh（A-4 的单一源校验）")
    return fail


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="生成/校验随包分发的预编译 Rime build 产物")
    parser.add_argument("--out", default=str(DEFAULT_OUT), help="输出目录（默认 packaging/rime/prebuilt）")
    parser.add_argument("--schemas", default=",".join(DEFAULT_SCHEMAS),
                        help="随包方案，逗号分隔（默认 luna_pinyin,wubi86）")
    parser.add_argument("--budget-mb", type=float, default=DEFAULT_BUDGET_MB,
                        help="体积上限 MB，超出就只带第一个方案（默认 60）")
    parser.add_argument("--check", action="store_true",
                        help="只校验已有产物与打包接线（不需要 docker）")
    parser.add_argument("--verify-dir", default=None,
                        help="只校验某个已生成的 prebuilt 目录：MANIFEST.json 与产物"
                             "是否逐字节一致（CI 用来校验发布产物里的 prebuilt）")
    parser.add_argument("--image", default=os.environ.get("RIMESYNC_PREBUILT_IMAGE", DEFAULT_IMAGE),
                        help="部署用的 docker 镜像")
    parser.add_argument("--deployer", default=None, help="rime_deployer 路径（默认自动找 staging）")
    parser.add_argument("--no-docker", action="store_true", help="不套 docker，直接在宿主跑 deployer")
    parser.add_argument("--skip-verify", action="store_true", help="跳过铺制自检（不推荐）")
    parser.add_argument("--smoke", default=None,
                        help="rime-smoke 二进制路径（默认在容器里用 scripts/ime/rime-smoke.cc 现编）")
    parser.add_argument("--no-smoke", action="store_true",
                        help="跳过 rime_api 引擎自检（只跑部署器；不推荐）")
    parser.add_argument("--keep-scratch", action="store_true", help="保留临时目录（排错用）")
    args = parser.parse_args(argv)

    out_dir = Path(args.out).resolve()
    budget = int(args.budget_mb * 1024 * 1024)
    if args.verify_dir:
        # 只做一件事：清单与产物逐字节对照（发布产物里的 prebuilt 用这个查）。
        target = Path(args.verify_dir).resolve()
        fails = verify_manifest(target)
        if fails:
            for item in fails:
                print(f"FAIL {item}", file=sys.stderr)
            print(f"verify-prebuilt: {len(fails)} 项不一致（{target}）", file=sys.stderr)
            return 1
        print(f"ok   prebuilt 清单与产物逐字节一致（{target}）")
        return 0
    if args.check:
        fails = check_payload(out_dir, budget) + check_git_ignore(out_dir) + check_wiring()
        if fails:
            for item in fails:
                print(f"FAIL {item}", file=sys.stderr)
            print(f"check-prebuilt: {len(fails)} 项不通过", file=sys.stderr)
            return 1
        files = len(json.loads((out_dir / "MANIFEST.json").read_text(encoding="utf-8"))["files"])
        total = json.loads((out_dir / "MANIFEST.json").read_text(encoding="utf-8"))["total_bytes"]
        print(f"ok   prebuilt {files} 个文件 {human(total)}（预算 {human(budget)}），"
              "方案标记/清单/打包接线全部一致")
        return 0
    return generate(args)


if __name__ == "__main__":
    raise SystemExit(main())
