# FORK.md — third_party/weasel

## Upstream

- URL: https://github.com/rime/weasel
- License: GPL-3.0 (see `LICENSE.txt`)
- Pinned ref: tag `0.17.4`
- Pinned commit: `9cc96e20dc71b80876b12f689bb5863c76c2a7ed` (annotated tag object `f53a92543f2ef890503d0391e595af8fa9abdb02`)
- Vendored on: 2026-10-21
- Vendored size: ~13 MB (no `.git`)

## Status: source only, not built

Weasel is the **Windows** front-end (TSF + WeaselServer/Deployer, MSVC + NSIS).
It is vendored for reference and for the future Windows installer work, and is
deliberately **not** touched by `scripts/ime/build-linux.sh` — nothing in the
Linux deb is produced from it.

`librime/` and `plum/` are upstream git submodules and are **empty** here; do not
expect a self-contained Windows build from this tree without initialising them.

`output/` (~9.6 MB) is upstream-tracked installer scaffolding, including
prebuilt third-party helper binaries (`7z.exe`, `7z.dll`, `curl.exe`,
`WinSparkle.dll`, `curl-ca-bundle.crt`) used by `install.nsi`. It is kept as-is so
the tree stays a faithful copy of the pin; drop it only if repository size
becomes a problem.

## Local modifications

None.

## How to upgrade

```sh
cd third_party
rm -rf weasel
git clone --depth 1 --branch <NEW_TAG> https://gh-proxy.com/https://github.com/rime/weasel weasel
rm -rf weasel/.git
```
