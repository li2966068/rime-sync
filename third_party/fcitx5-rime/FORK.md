# FORK.md — third_party/fcitx5-rime

## Upstream

- URL: https://github.com/fcitx/fcitx5-rime
- License: LGPL-2.1-or-later (see `LICENSES/LGPL-2.1-or-later.txt`)
- Pinned ref: tag `5.0.16` (last tag of the 5.0.x line)
- Pinned commit: `c0f73b76a24ebfe2af739623c35071ced7e0cee8` (annotated tag object `e98dfadc72e2bf4de0cda339e1fa7893f57a1abc`)
- Vendored on: 2026-10-21
- Vendored size: 259 KB (no `.git`)

## Why 5.0.16 and not `master`

- `master` is 5.1.17 and requires `Fcitx5Core >= 5.1.22` plus the
  `Fcitx5Module` `Notifications` component. Ubuntu 22.04 ships **fcitx5
  5.0.14**, so master cannot even be configured against the target runtime,
  let alone load into it.
- `5.0.16` requires only `Fcitx5Core >= 5.0.6`, which matches 22.04's
  `libfcitx5core7 5.0.14` — the same fcitx5 generation the app's existing
  dependency bundle (`packaging/linux/vendor/`) already ships.
- The pin is a *tag*, not a moving branch, and the commit id is recorded above.
  Every 5.0.x tag from 5.0.12 on also needs `Fcitx5Module` (see next section).

## Ubuntu 22.04 needs a build-time shim (not a source patch)

`fcitx5 5.0.14`'s Debian packages install **neither**
`Fcitx5ModuleConfig.cmake` **nor** the per-module public headers
(`notifications_public.h`, `dbus_public.h`), yet `CMakeLists.txt` line 14 does
`find_package(Fcitx5Module REQUIRED COMPONENTS Notifications DBus)` and
`src/rimeservice.cpp` includes `dbus_public.h`. Without help this tree cannot
be built on 22.04 at all.

`scripts/ime/Dockerfile` therefore installs, **only when the distribution lacks
them**:
- `scripts/ime/Fcitx5ModuleConfig.cmake` — a header-only package config that
  creates the `Fcitx5::Module::<Component>` INTERFACE targets (nothing is
  linked: addons are looked up by name at runtime through
  `FCITX_ADDON_DEPENDENCY_LOADER`, and the actual `libnotifications.so` /
  `libdbus.so` addons are present in 5.0.14);
- `notifications_public.h` and `dbus_public.h` fetched from the fcitx5 source
  tag matching the installed `fcitx5` version (5.0.14), so the plugin is
  compiled against exactly the interface it will meet at runtime.

On a distribution whose fcitx5 ships `Fcitx5Module` (24.04 / fcitx5 5.1.x) the
shim step is skipped automatically.

## Local modifications

### 1. `UserDataDirPath` — a real Rime user directory setting (2026-10-22)

Problem: upstream ships only `ExternalOption userDataDir` (`src/rimeengine.h`,
key `UserDataDir`), which renders as a button running `xdg-open <default dir>`.
Picking a different directory in the file manager had no effect, because
`rimeStart()` (`src/rimeengine.cpp`) always used
`StandardPath::userDirectory(PkgData)/rime`.

Changes:

- `src/rimeengine.h`: new `Option<std::string> userDataDirPath` with key
  `UserDataDirPath`, label `_("User data directory (empty = default)")`,
  default `""`. The old ExternalOption was renamed to key `OpenUserDataDir`
  with label `_("Open user data directory")` so the two keys cannot collide
  (fcitx5-configtool writes one ini entry per key; reusing `UserDataDir` for
  both an external action and a string would corrupt `conf/rime.conf`).
- `src/rimeengine.cpp`: `resolveRimeUserDir()` returns the configured path when
  non-empty (`~` expanded from `HOME`, trailing slashes stripped, relative
  paths refused with a log line and fallback to the default), otherwise the
  upstream default. `rimeStart()` creates the directory (`fs::makePath`) and
  falls back to the default if creation fails; the resolved path is logged via
  `RIME_INFO()` ("Rime user data dir: ...").
- `src/rimeengine.h`: added the `RIME_INFO()` macro next to `RIME_DEBUG()`.
- `po/fcitx5-rime.pot`, `po/zh_CN.po`: the two new strings, with Chinese
  translations (`打开用户目录`, `用户数据目录（留空 = 默认）`); the obsolete
  `User data dir` msgid was dropped.

Behaviour notes:

- Empty value = upstream behaviour, so existing installs keep using
  `~/.local/share/fcitx5/rime`; no migration is performed when the value
  changes.
- The `OpenUserDataDir` button still opens the *default* directory, because an
  `ExternalOption` URI is computed once at startup and cannot follow the
  string option.
- Config page changes take effect on save: `setConfig()` → `updateConfig()` →
  `rimeStart()`.

### 2. Sync action buttons on the Rime config page (2026-10-24)

The `rime-sync` package ships three entry points that the Rime addon page now
exposes as `ExternalOption` buttons, right after `OpenUserDataDir`
(`src/rimeengine.h`):

| key | label | command |
|---|---|---|
| `OpenSyncPanel` | `打开同步面板 / Open sync panel` | `sh -c "if [ -x '/usr/bin/rime-sync-app' ]; then exec '/usr/bin/rime-sync-app' --setup; fi"` |
| `SyncNow` | `立即同步 / Sync now` | `sh -c "if [ -x '/usr/bin/rime-sync-cli' ]; then exec '/usr/bin/rime-sync-cli' transport; fi"` |
| `ShowSyncStatus` | `查看同步状态 / Sync status` | `sh -c "if [ -x '/usr/bin/rime-sync-status' ]; then exec '/usr/bin/rime-sync-status'; fi"` |

Why this shape:

- fcitx5-configtool turns an `ExternalOption` value into
  `QProcess::startDetached(program, args)`, and Plasma's QML widget calls
  `launchExternal()`; both split the string into program + arguments. The whole
  shell snippet is therefore double quoted, and each absolute path is single
  quoted inside it — nothing else needs escaping.
- `sh -c` plus the `[ -x ... ]` test make a missing `rime-sync` package a
  silent no-op instead of a button that fails with no feedback
  (`rime-sync-status` is provided by that package; this tree does not ship it).
- The commands are fixed literals; no user input is interpolated. The
  `xdg-open` button keeps its own `stringutils::replaceAll` escaping because
  its path *is* derived from the environment.
- `po/fcitx5-rime.pot` and `po/zh_CN.po` carry the three bilingual msgids
  (`打开同步面板`, `立即同步`, `查看同步状态`). Other catalogs are untouched:
  the labels already contain both languages, so deleting `*.mo` (localepurge)
  never hides them.

Verification:

- `msgfmt -c` on `po/fcitx5-rime.pot` and `po/zh_CN.po`.
- Build the addon in the container image (`scripts/ime/build-linux.sh` →
  `scripts/ime/container-build.sh` compiles `third_party/fcitx5-rime`), then
  check the config page over DBus:
  `gdbus call --session --dest org.fcitx.Fcitx5 --object-path /controller
  --method org.fcitx.Fcitx.Controller1.GetConfig 'fcitx://config/addon/rime'`
  and look for `External=sh -c ...` entries under `OpenSyncPanel`,
  `SyncNow`, `ShowSyncStatus`.
- Manual: in `fcitx5-configtool` → Rime → the three buttons must open the
  setup wizard, run `rime-sync-cli transport`, and show the notification. On a
  machine without the `rime-sync` package the buttons must do nothing.

## How to upgrade

```sh
cd third_party
rm -rf fcitx5-rime
git clone --depth 1 --branch <NEW_TAG> https://gh-proxy.com/https://github.com/fcitx/fcitx5-rime fcitx5-rime
rm -rf fcitx5-rime/.git
git ls-remote --tags https://github.com/fcitx/fcitx5-rime | grep <NEW_TAG>   # record commit id above
```

Only move to a 5.1.x tag together with a base image that ships fcitx5 >= 5.1.22
(Ubuntu 24.04), and rebuild both sides in the same image — the addon ABI is not
guaranteed across fcitx5 minor versions.
