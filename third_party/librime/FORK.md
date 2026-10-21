# FORK.md — third_party/librime

## Upstream

- URL: https://github.com/rime/librime
- License: BSD-3-Clause (see `LICENSE`)
- Pinned ref: tag `1.17.0`
- Pinned commit: `33e78140250125871856cdc5b42ddc6a5fcd3cd4` (annotated tag object `a52a3400f8b7679e839bc5fb8e6309a0fc4424da`)
- Vendored on: 2026-10-21
- Vendored size: 7.1 MB (no `.git`)

## What is vendored

The upstream tree at the tag, **without git metadata and without the `deps/`
submodules** (`deps/{glog,leveldb,yaml-cpp,googletest,marisa-trie,opencc}` are
present but empty). Those libraries come from the build image's system packages,
which keeps the vendored tree small and the build reproducible per distribution.

The dependency mechanism is **not** a set of `USE_SYSTEM_*` switches: librime
1.17.0 has no such options. It resolves every dependency through
`cmake/Find<Dep>.cmake` (`find_path` + `find_library`), so anything already
installed on the system is used automatically and `deps/` is only the fallback
of upstream's full-source `deps.mk` workflow. On Linux the required pieces are:
Boost (>= 1.74, component `regex`), glog, leveldb, yaml-cpp, marisa-trie,
opencc, Threads; googletest only with `-DBUILD_TEST=ON` (we build with it off).

## Local modifications

None — the tree is byte-identical to upstream `1.17.0` apart from the removed
`.git` directory and the empty `deps/` working trees.

## Build-knowledge that belongs with this pin

Both items are handled in `scripts/ime/container-build.sh`; no source is patched.

1. **`-DLINUX=ON` is mandatory.** `CMakeLists.txt` only calls
   `find_package(Boost 1.74.0 REQUIRED COMPONENTS regex)` inside `if(LINUX)`,
   and nothing in the tree ever defines `LINUX`. Without it `librime.so` is
   linked with undefined `boost::re_detail_*` symbols and every executable
   linking it fails; with it, `Boost_LIBRARIES` (= `Boost::regex`) ends up in
   `rime_core_deps` and `libboost_regex` lands in `DT_NEEDED`.
2. **Logging requires glog >= 0.6.** `src/rime/setup.cc` calls
   `google::IsGoogleLoggingInitialized()`, which is only *declared* from glog
   0.6 on (0.5.0 exports the symbol but not the declaration). The build script
   greps the header and turns `-DENABLE_LOGGING` off on older systems, so on
   Ubuntu 22.04 (glog 0.5.0) rime writes no `rime.log`; on 24.04 it does.

## How to upgrade

```sh
cd third_party
rm -rf librime
git clone --depth 1 --branch <NEW_TAG> https://gh-proxy.com/https://github.com/rime/librime librime
rm -rf librime/.git
git ls-remote --tags https://github.com/rime/librime | grep <NEW_TAG>   # record commit id above
```

Re-check the dependency list in `scripts/ime/Dockerfile` (librime occasionally
adds a required system library between minor releases) and rebuild.
