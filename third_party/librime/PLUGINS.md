# PLUGINS.md — librime plugins bundled in this repository

The vendored librime (`third_party/librime`, upstream 1.17.0) carries one
locally added plugin. It lives in `third_party/librime/plugins/`, which
`plugins/CMakeLists.txt` discovers with `file(GLOB "*")`; with
`-DBUILD_MERGED_PLUGINS=ON` (the default, and what
`scripts/ime/container-build.sh` and `scripts/ime/build-weasel.ps1` use) its
object files are compiled into `librime` itself, so nothing has to be loaded
at runtime.

## rimesync-extra

```
third_party/librime/plugins/rimesync-extra/
├── CMakeLists.txt              # OBJECT lib merged into librime
├── rimesync_extra_module.cc    # RIME_REGISTER_MODULE(rimesync_extra)
├── vnum_convert.h/.cc          # pure conversion functions (no librime deps)
├── vnum_translator.h/.cc       # component "rimesync_vnum" (translator)
├── punct_processor.h/.cc       # component "rimesync_punct" (processor)
├── vselect_processor.h/.cc     # component "rimesync_vselect" (processor)
├── test/vnum_convert_test.cc   # standalone unit test
└── test/engine_smoke_test.cc   # optional runtime harness
```

| item | value |
| --- | --- |
| directory | `third_party/librime/plugins/rimesync-extra` |
| module name | `rimesync_extra` |
| translator component | `rimesync_vnum` |
| processor components | `rimesync_punct`, `rimesync_vselect` |
| merged CMake target | `rimesync-extra-objs` |
| test target | `rimesync_extra_vnum_test`, custom target `check-rimesync-extra` |
| non-merged artifact | `rime-rimesync-extra` (`RIME_PLUGINS_DIR`) |

## 1. `rimesync_vnum` — “v” mode number conversion

Only numbers; no dates and no arithmetic. The translator inspects the text of
the segment it is queried for and matches `^[vV]` followed by a number.

Accepted input (`v` prefix, optional minus, 1–18 integer digits, optional
`.` + 1–2 fraction digits):

| input | candidate 1 (comment 大写) | candidate 2 (comment 小写) |
| --- | --- | --- |
| `v0` | 零元整 | 零 |
| `v10` | 壹拾元整 | 十 |
| `v123` | 壹佰贰拾叁元整 | 一百二十三 |
| `v1005` | 壹仟零伍元整 | 一千零五 |
| `v10005` | 壹万零伍元整 | 一万零五 |
| `v123456789` | 壹亿贰仟叁佰肆拾伍万陆仟柒佰捌拾玖元整 | 一亿二千三百四十五万六千七百八十九 |
| `v0.05` | 零元零伍分 | 零点零五 |
| `v1.5` | 壹元伍角整 | 一点五 |
| `v-12.3` | 负壹拾贰元叁角整 | 负十二点三 |
| `v123.45` | 壹佰贰拾叁元肆角伍分 | 一百二十三点四五 |

Rules and limits:

* Financial upper case: `0` → `零元整`; whole yuan gets the `元整` suffix;
  jiao without fen gets `…角整`; fen-only gets `零元零伍分`; the usual 零
  bridging applies inside and between groups (1005, 10005, 10000000010).
* Chinese lower case: 10–19 (and 10万 / 10亿 …) drop the leading 一
  (`十`, `十五`, `十万`); the fraction is read digit by digit after 点, and
  trailing zeros are preserved as typed (`1.50` → `一点五零`).
* Negative numbers are prefixed with 负; `-0` behaves like `0`.
* More than 18 integer digits, more than 2 fraction digits, or any other
  character ⇒ no candidate at all (the translator returns `nullptr`).
* Groups of four digits are labelled 万 / 亿 / 万亿 / 亿亿, so the 17th and
  18th digit are rendered as 亿亿 (e.g. `v999999999999999999`).
* `v`/`V` are both accepted, the leading `v` is not part of the candidate
  text (the commit replaces the whole segment).
* Confirm with **space** or Enter (`express_editor`), click a candidate, or
  pick it by letter with `rimesync_vselect` (`a` = 1st, `b` = 2nd, …).
  Number keys cannot pick a candidate: the `recognizer` processor matches
  `input + digit` and keeps extending the amount, exactly as upstream Rime
  behaves with a digit-bearing `recognizer` pattern.

Schema wiring (the schema file is packaged by the packaging line, not here):

```yaml
engine:
  processors:            # recognizer must stay in this list (bundled schemas have it)
    - ascii_composer
    - rimesync_punct
    - recognizer
    # ... unchanged
  segmentors:            # matcher must stay in this list (bundled schemas have it)
    - matcher
    # ... unchanged
  translators:
    - rimesync_vnum      # before script_translator / table_translator
    # ... unchanged

recognizer:
  patterns:
    vnum: "^v[0-9]{1,18}(\\.[0-9]{0,2})?$"
```

Two existing pieces make `v123` reach one segment, so they must not be removed:

* the `recognizer` **processor** pushes each key whose `input + char` matches a
  pattern, bypassing the speller (whose alphabet has no digits);
* the `matcher` **segmentor** turns the match into one segment tagged `vnum`,
  which is what the translator receives.

Both match against `^…$` of the whole input, so the pattern above deliberately
allows an empty fraction (`{0,2}`) and `v12.` as an intermediate state;
with `{1,2}` the dot typed before the first fraction digit would not match and
would fall through to the punctuator. The translator itself only answers for a
complete number, so intermediate states simply show no candidate. The
translator is tag-agnostic: it only looks at the segment text, so a different
tag name works too.

## 2. `rimesync_punct` — Chinese punctuation, committed immediately

Intercepted keys (18) — nothing else is touched, so letters, digits and dead
keys behave exactly as before:

`\` `<` `>` `[` `]` `{` `}` `|` `%` `$` `*` `~` `!` `?` `;` `:` `,` `.`

`/` is **not** intercepted on purpose: it is Rime's symbol prefix
(`/fh`, `/dn`, `/bq`, …, enabled by `punctuator: import_preset: symbols`,
which sets `recognizer/patterns/punct`), and Rime's own `punctuation.yaml`
already maps `/` to the candidate list `[、, /, ／, ÷]`. Taking `/` over
would both break `/fh` and change Rime's existing `、`-candidate behaviour.
`?` (Shift+/) is still committed directly as `？`.

Default table (main symbol = committed directly when the key arrives without
Shift; alternatives = Shift menu, main symbol excluded):

| key | main | Shift alternatives |
| --- | --- | --- |
| `\` | 、 | · ＼ ｜ |
| `;` | ； | ： … |
| `,` | ， | 《 〈 ＜ |
| `.` | 。 | 》 〉 ． |
| `[` | 「 | 『 ［ |
| `]` | 」 | 』 ］ |
| `<` | 《 | 〈 ＜ |
| `>` | 》 | 〉 ＞ |
| `{` | 『 | ｛ |
| `}` | 』 | ｝ |
| `|` | · | ｜ |
| `%` | ％ | ‰ % |
| `$` | ￥ | ＄ € |
| `*` | × | ＊ · |
| `~` | ～ | 〜 ~ |
| `!` | ！ | ¡ ! |
| `?` | ？ | ? |
| `:` | ： | ∶ ︰ |

Behaviour:

* Only in Chinese mode: `ascii_mode` off **and** `ascii_punct` off. English
  mode or an active composition ⇒ `kNoop`, the key is left to other
  processors.
* No composition is created for the main symbol: it is committed immediately
  through `engine->CommitText()`, so `\` no longer needs a space press to
  produce 、. (`/` keeps Rime's own behaviour — see above.)
* Shift opens the alternatives as a candidate menu (number keys / space /
  Enter select, Esc cancels, arrows and paging keys are left to
  selector/navigator, any other key cancels the menu and keeps its own
  meaning). The first alternative of each unshifted key is the main symbol of
  its Shift twin, so Shift+, ⇒ 《, Shift+. ⇒ 》, Shift+; ⇒ ：, Shift+[ ⇒ 『,
  Shift+] ⇒ 』, Shift+\ ⇒ ·. `?` is not a menu: with `/` out of the table
  its base key is unknown, so it commits ？ directly.
* Key codes are X11 keysyms: Shift+, already arrives as `<` with the Shift
  bit set. The table is therefore indexed by *character*, and Shift means
  “show the alternatives of the key this character is produced from”. When
  that base key is not intercepted (`$` from Shift+4, `%`, `*`, `~`, `!`)
  the character’s own main symbol is committed directly. Set
  `rimesync_punct/shift_alternatives: false` to always commit the main symbol
  and never open the menu (this restores the plain “character table” reading
  on layouts where the shifted symbol is the only way to type it).

Schema wiring:

```yaml
engine:
  processors:
    - ascii_composer
    - rimesync_punct    # before punctuator / key_binder / selector / express_editor
    - rimesync_vselect  # letters a..h for the v-mode menu (see section 3)
    - recognizer
    # ... existing processors
```

Optional overrides (defaults above are compiled in):

```yaml
rimesync_punct:
  shift_alternatives: true          # false = never show the alternatives menu
  keys:                             # char -> [main, alternative, ...]
    "\\": ["、", "·", "＼", "｜"]
    "/":  ["、", "？", "／"]
```

Overrides are read once, when the processor is created (i.e. per schema).

## 3. `rimesync_vselect` — letter selection for the v-mode menu

While the context input matches `^[vV][0-9.]*$` **and** a candidate menu is
shown, the first letters pick a candidate instead of extending the input:

| key | action |
| --- | --- |
| `a` | select and commit candidate 1 (大写 amount) |
| `b` | select and commit candidate 2 (小写 amount) |
| `c` … | candidate 3 … |
| `z`, other letters | `kNoop` — the key is left to the normal chain |
| letter past the last candidate | `kNoop` — nothing is swallowed |
| space / Enter / Esc / arrows / paging | `kNoop` — `selector` / `navigator` / `express_editor` keep them |

Outside that context (input is not `v` + digits, no menu, `ascii_mode` on)
every key is `kNoop`, so letters typed during pinyin input are never touched.

Default keys are `abcdefgh` (at most 8); override with a plain string, one
character per candidate:

```yaml
rimesync_vselect:
  keys: "abcdefgh"      # e.g. "12345678" is *not* recommended: digits belong
                        # to the recognizer while typing the amount
```

Wiring: add `rimesync_vselect` to `engine/processors` before `recognizer` /
`speller` (it must see the letter before the speller turns it into input).

## How to disable

* Per schema: drop `rimesync_punct` from `engine/processors` and/or
  `rimesync_vnum` from `engine/translators`.
* Punctuation menu only: `rimesync_punct/shift_alternatives: false`.
* Letter selection only: drop `rimesync_vselect` from `engine/processors`
  (or set `rimesync_vselect/keys: ""`, which leaves it inert).
* Build level: remove `third_party/librime/plugins/rimesync-extra` before
  configuring, or build with `-DRIMESYNC_EXTRA_BUILD_TEST=OFF` to skip only
  the unit tests. `RIME_PLUGINS` (space separated allow-list) can also keep
  the directory on disk while excluding the plugin.

## Building and testing

The build below is the exact command required by the task (Linux, host lacks
Boost/glog/leveldb/yaml-cpp/marisa/opencc, so it runs in the repository's
build image):

```sh
docker run --rm -v "$PWD":/src -v /tmp/lr-vnum:/tmp/lr-vnum -w /src \
  rime-ime-builder:debian_13 /bin/bash -lc '
    cmake -S /src/third_party/librime -B /tmp/lr-vnum \
          -DBUILD_MERGED_PLUGINS=ON -DBUILD_TEST=OFF &&
    cmake --build /tmp/lr-vnum -j"$(nproc)"'
```

Configure output confirms the plugin was picked up
(`Found plugin: …/plugins/rimesync-extra`, `rime_plugins_modules: rimesync_extra`)
and the build produces the `rimesync-extra-objs` object target together with
the standalone test target `rimesync_extra_vnum_test`. The conversion test is
also available as the custom target `check-rimesync-extra`
(`cmake --build <build> --target check-rimesync-extra`), or, without CMake:

```sh
cd third_party/librime/plugins/rimesync-extra
g++ -std=c++17 -I. test/vnum_convert_test.cc vnum_convert.cc -o /tmp/vnum_test &&
  /tmp/vnum_test
```

`test/engine_smoke_test.cc` is an optional runtime harness that needs a built
librime. It checks both layers: component lookup / direct key commits / Shift
menu / Esc, and a real `Engine` with an in-memory schema driven by
`ProcessKey` — `/` keeping Rime's 「、」 menu, `/fh` reaching the symbols
table, `\` still committing 、, Shift+, still opening 《, v-mode
(`v1234.5` + space) producing 壹仟贰佰叁拾肆元伍角整, and letter selection
(`v1234` + `a`/`b`, out-of-range letters and `z` staying `kNoop`, with the
`rimesync_vselect/keys` override covered too):

```sh
g++ -std=c++17 -I<librime>/src -I<librime>/include -I<build>/src \
    test/engine_smoke_test.cc -L<build>/lib -lrime -o /tmp/smoke
LD_LIBRARY_PATH=<build>/lib /tmp/smoke
```

Note: `third_party/librime/.gitignore` ignores `plugins/*/` (upstream expects
third-party plugins to be symlinked checkouts). A `!plugins/rimesync-extra/`
negation was added there so the directory is tracked.

## Differences from upstream

* One added directory (`plugins/rimesync-extra/`); no upstream file is
  modified. Everything is compiled into `librime` because
  `BUILD_MERGED_PLUGINS=ON`, so `rime_api`, the ABI and the header set are
  unchanged.
* The components are only active when a schema names them; a schema that does
  not reference them behaves exactly like upstream.

## Windows / MSVC notes

* All Chinese literals are `u8""`, which is UTF-8 on every compiler; the
  plugin adds `/utf-8` for MSVC as well, so the sources do not depend on the
  system code page (no BOM required).
* No POSIX-only API is used: conversion is hand-written (no `strtoll`), and
  the processor only touches librime interfaces that are identical on
  Weasel builds.
* `plugin_modules` is `rimesync_extra`, matching the module registered by
  `RIME_REGISTER_MODULE(rimesync_extra)`; the directory name
  `rimesync-extra` maps to it exactly like upstream does for
  `librime-*` plugins.
* The environment is built with `-DBUILD_SHARED_LIBS=ON`; when the sources
  are compiled as part of the merged DLL they need no export macro.
* Not verified locally: `scripts/ime/build-weasel.ps1` was not run on this
  Linux host — the Windows side was reviewed statically only.
