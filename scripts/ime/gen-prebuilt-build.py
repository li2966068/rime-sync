#!/usr/bin/env python3
"""gen-prebuilt-build.py —— 生成/校验随安装包分发的「预编译 build 产物」。

为什么需要它
------------
有些 Windows 机器上 WeaselDeployer.exe /deploy 返回 0 但什么都不写
（%APPDATA%\\Rime\\{user.yaml,installation.yaml,build} 一直停在旧日期），
手动起 WeaselServer 也一样。这种机器上「本机部署」永远不生效，安装器只能把
别处已经部署好的 build/ 产物直接铺进用户目录。

产物内容（默认两套方案）
------------------------
  build/    luna_pinyin / wubi86 的 *.schema.yaml + *.prism.bin + *.table.bin
            + *.reverse.bin，以及它们 schema 里 dependencies 声明所必需的
            依赖方案产物（luna_pinyin -> stroke，wubi86 -> pinyin_simp）；
            另加部署器写出的 build/default.yaml。
  config/   生成这套产物时用的 default.custom.yaml（schema_list 已裁剪到
            产物里的方案）+ 各方案的 <schema>.custom.yaml。
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

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUT = REPO_ROOT / "packaging" / "rime" / "prebuilt"
DEFAULT_SCHEMAS = ("luna_pinyin", "wubi86")
DEFAULT_BUDGET_MB = 60.0
DEFAULT_IMAGE = "rime-ime-builder:debian_13"

TEMPLATE_DIR = REPO_ROOT / "packaging" / "rime"
SCHEMA_CUSTOM_DIR = TEMPLATE_DIR / "schemas"
RIME_DATA_DIR = REPO_ROOT / "third_party" / "rime-data"
STAGING_ROOT = REPO_ROOT / "build" / "ime-staging"

# 2021-01-01T00:00:00Z：把输入文件的 mtime 钉死，重复生成得到逐字节相同的产物
# （__build_info.timestamps 记的就是源文件 mtime，不钉死每次都不一样）。
FIXED_MTIME = 1609459200

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
    """把 third_party/rime-data/*/ 的 *.yaml / *.txt 平铺进共享数据目录。"""
    if not RIME_DATA_DIR.is_dir():
        die(f"缺少 {RIME_DATA_DIR}（随包的 rime 数据）")
    dest.mkdir(parents=True, exist_ok=True)
    count = 0
    for recipe in sorted(p for p in RIME_DATA_DIR.iterdir() if p.is_dir()):
        for f in sorted(recipe.iterdir()):
            if f.is_file() and f.suffix in (".yaml", ".txt"):
                shutil.copyfile(f, dest / f.name)
                count += 1
    if count == 0:
        die(f"{RIME_DATA_DIR} 里没有 *.yaml/*.txt")
    return count


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
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "default.custom.yaml").write_text(reduced_default_custom(schemas), encoding="utf-8")
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
            if len(attempt) > 1:
                log(f"!! {human(total)} 超过预算 {human(budget)}：只带 {attempt[0]}")
                attempt = attempt[:1]
                continue
            die(f"{human(total)} 超过预算 {human(budget)}，单套方案也放不下")

        missing = [s for s in attempt if not (build_dir / f"{s}.schema.yaml").is_file()]
        if missing:
            die(f"缺少方案产物：{missing}")
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
    files = []
    for src in collect_products(build_dir):
        dst = out_dir / "build" / src.name
        shutil.copyfile(src, dst)
        files.append({"path": f"build/{src.name}", "bytes": dst.stat().st_size,
                      "sha256": sha256_file(dst)})
    for name in sorted(os.listdir(user_src)):
        if name.endswith(".custom.yaml") or name == "default.custom.yaml":
            dst = out_dir / "config" / name
            shutil.copyfile(user_src / name, dst)
            files.append({"path": f"config/{name}", "bytes": dst.stat().st_size,
                          "sha256": sha256_file(dst)})
    pin_mtimes(out_dir)
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
        "markers": markers,
        "budget_bytes": budget,
        "total_bytes": total,
        "files": files,
    }
    (out_dir / "MANIFEST.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    log(f"体积：{human(total)}（预算 {human(budget)}），"
        f"方案：{', '.join(schemas)}，rime {rime_version or '?'}")


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
    listed = {entry["path"]: entry for entry in manifest.get("files", [])}
    if not listed:
        fail.append("MANIFEST.json 的 files 为空")
    for rel, entry in sorted(listed.items()):
        path = out_dir / rel
        if not path.is_file():
            fail.append(f"清单里的文件不存在：{rel}")
            continue
        size = path.stat().st_size
        if size != entry.get("bytes"):
            fail.append(f"{rel} 大小 {size} != 清单 {entry.get('bytes')}")
        digest = sha256_file(path)
        if digest != entry.get("sha256"):
            fail.append(f"{rel} sha256 与清单不一致（产物被改过或没重新生成）")
    # .gitignore 是本目录的入库胶水（反选 prebuilt/build），不算产物
    skip_names = ("MANIFEST.json", ".gitignore")
    for path in sorted(out_dir.rglob("*")):
        if path.is_file() and path.name not in skip_names:
            rel = str(path.relative_to(out_dir))
            if rel not in listed:
                fail.append(f"未登记的文件：{rel}")
    schemas = manifest.get("schemas") or []
    if not schemas:
        fail.append("清单里没有 schemas")
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
    sample = next(iter(sorted(out_dir.glob("build/*.table.bin"))), None)
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
    has(REPO_ROOT / "packaging" / "nsis-hooks.nsh",
        ["resources\\prebuilt", "PREBUILT ok", "PREBUILT failed", "build-source=",
         "RIMESYNC_PREBUILT"])
    for script in ("postinst", "install.sh"):
        has(REPO_ROOT / "packaging" / "linux" / script,
            ["/usr/share/rime-sync/prebuilt", "RIMESYNC_PREBUILT", "seed_prebuilt_build"])
    has(REPO_ROOT / "scripts" / "ime" / "package-ime-deb.sh",
        ["/usr/share/rime-sync/prebuilt"])
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
