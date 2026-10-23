# TestRimeSync — portable checks for `include/RimeSync.h`

The "Rime 同步…" entry cannot be built on this Linux box (no MSVC, no mingw, no
`rc.exe`), so this directory compiles `include/RimeSync.h` against three tiny stub
headers and exercises the path resolution plus the MessageBox / ShellExecute
decisions on any host with a C++17 compiler.

**Not part of the Windows build.** Nothing in `weasel.sln`, `xmake.lua` or
`scripts/ime/build-weasel.ps1` references this directory; it is a local check only.

## Run

```sh
cd third_party/weasel/test/TestRimeSync
g++ -std=c++17 -Wall -Wextra -Istub -I../../include -o /tmp/rimesync-scenario scenario.cpp
/tmp/rimesync-scenario
```

Expected: every line `PASS`, last line `ALL SCENARIOS PASSED`, exit code 0.

## Scenarios

| # | Case |
| --- | --- |
| 1 | `HKLM\Software\rime-sync` `InstallDir` (64-bit view) wins; `ShellExecuteW` gets `open` + `--setup`; no MessageBox |
| 2 | registry points at a deleted install → falls back to `%ProgramFiles%\rime-sync` |
| 3 | 32-bit (WOW6432Node) `InstallDir` |
| 4 | per-user install in `HKCU\Software\rime-sync` |
| 5 | registry default value (Tauri NSIS style, no value name) |
| 6 | nothing installed → MessageBox "未安装 Rime 同步。 / Rime Sync is not installed." with caption "Rime 同步", no ShellExecute |
| 7 | exe found but `ShellExecuteW` returns ≤ 32 → "无法启动 Rime 同步。" |
| 8 | `%LOCALAPPDATA%\Programs\rime-sync` fallback |

## stub/

Minimal stand-ins for `<windows.h>`, `<shellapi.h>` and `<filesystem>` (a
`std::filesystem::path` whose `value_type` is `wchar_t`, like MSVC's). They
implement only what `RimeSync.h` touches, backed by in-memory registry / environment
/ file tables. **Never** put these on the include path of a real Windows build.
