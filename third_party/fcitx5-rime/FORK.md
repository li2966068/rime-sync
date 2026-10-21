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

None.

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
