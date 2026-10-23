# FORK.md — third_party/weasel

## Upstream

- URL: https://github.com/rime/weasel
- License: GPL-3.0 (see LICENSE.txt)
- Pinned ref: tag 0.17.4
- Pinned commit: 9cc96e20dc71b80876b12f689bb5863c76c2a7ed (annotated tag object f53a92543f2ef890503d0391e595af8fa9abdb02)
- Vendored on: 2026-10-21
- Vendored size: ~13 MB (no .git)

## Status: source only, not built on Linux

Weasel is the **Windows** front-end (TSF + WeaselServer/Deployer, MSVC + NSIS).
It is vendored so that scripts/ime/build-weasel.ps1 can build the IME from source on
Windows CI and embed the installer into the rime-sync NSIS package. Nothing in the
Linux deb is produced from it, and scripts/ime/build-linux.sh never touches it.

librime/ and plum/ are upstream git submodules and are **empty** here; the Windows
build fetches a prebuilt librime instead (see build-weasel.ps1).

output/ (~9.6 MB) is upstream-tracked installer scaffolding, including prebuilt
third-party helper binaries (7z.exe, 7z.dll, curl.exe, WinSparkle.dll,
curl-ca-bundle.crt) used by install.nsi. It is kept as-is so the tree stays a
faithful copy of the pin; drop it only if repository size becomes a problem.

## Local modifications

### 1. "Rime 同步…" entry (2026-10-23)

Starts the rime-sync application that ships with our installer
(%ProgramFiles%\rime-sync\rime-sync-app.exe --setup), i.e. the executable the NSIS
installer puts into $INSTDIR and that the Start Menu shortcut "Rime 同步" launches.
The --setup argument opens the sync program straight in its setup wizard.

| File | Change |
| --- | --- |
| include/RimeSync.h | **new**, header-only (inline). rime_sync::ResolveExecutable() resolves the path; rime_sync::LaunchSetup(HWND) starts it with ShellExecuteW and reports failures with MessageBoxW |
| WeaselServer/WeaselServerApp.cpp | SetupMenuHandlers(): handler for ID_WEASELTRAY_RIMESYNC -> rime_sync::LaunchSetup(nullptr) |
| WeaselServer/resource.h | #define ID_WEASELTRAY_RIMESYNC 40017 |
| WeaselServer/WeaselServer.rc | tray menu IDR_MENU_POPUP: one MENUITEM in each of the three LANGUAGE blocks (zh-CN / zh-TW / en) |
| WeaselDeployer/SwitcherSettingsDialog.h .cpp | settings dialog: click handler OnRimeSync -> rime_sync::LaunchSetup(m_hWnd) |
| WeaselDeployer/WeaselDeployer.rc | IDD_SWITCHER_SETTING: one PUSHBUTTON in each of the three LANGUAGE blocks |
| WeaselDeployer/resource.h | #define IDC_BUTTON_RIMESYNC 1012 |

**Tray menu position** — 4th item, right after "用户资料同步 (&N)". The two entries are
adjacent but different: "用户资料同步" is still librime's own user-data sync
(WeaselDeployer.exe /sync), "Rime 同步…" is our rime-sync program.

~~~
输入法设定 (&S)
用户词典管理 (&D)
用户资料同步 (&N)      -> WeaselDeployer.exe /sync     (librime: install/backup)
Rime 同步… (&Y)         -> rime-sync-app.exe --setup    (this repository)
--- separator ---
用户文件夹 (&C) ...
~~~

**Settings dialog position** — IDD_SWITCHER_SETTING, the first window opened by the tray
item "输入法设定" / the Start Menu shortcut "Weasel Settings" (caption
"【小狼毫】方案选单设定"). The button sits in the bottom button row, right of OK.
Weasel has no MENU resource in that dialog, so a push button is the equivalent entry.

**Path resolution order** (rime_sync::ResolveExecutable(); first existing
rime-sync-app.exe wins):

1. HKLM\Software\rime-sync -> InstallDir (64-bit view; written by the NSIS installer)
2. same key, default value (what the Tauri NSIS template writes), 64-bit view
3. both of the above through the 32-bit (WOW6432Node) view — Weasel ships x64 and Win32
4. HKCU\Software\rime-sync -> InstallDir / default value (per-user install)
5. %ProgramW6432%\rime-sync\rime-sync-app.exe
6. %ProgramFiles%\rime-sync\rime-sync-app.exe
7. %LOCALAPPDATA%\Programs\rime-sync\rime-sync-app.exe (Tauri NSIS per-user default)

Nothing found -> MessageBoxW "未安装 Rime 同步。\n\nRime Sync is not installed." with the
caption "Rime 同步". Found but ShellExecuteW returns <= 32 -> "无法启动 Rime 同步。".

**Encoding** — wide-character throughout (std::filesystem::path, ShellExecuteW,
MessageBoxW). The Chinese literals in RimeSync.h are written as \uXXXX escapes so that
file stays pure ASCII and cannot be mangled by the compiler code page or /utf-8. Menu and
button labels live in the UTF-16 .rc files, one copy per LANGUAGE block.

**Build impact**

- RimeSync.h is header-only, included once per module. **No** change is needed to the
  <ClCompile> lists in *.vcxproj, to *.vcxproj.filters, or to xmake.lua's
  add_files("./*.cpp"). scripts/ime/build-weasel.ps1 needs **no** change either:
  msbuild weasel.sln recompiles the changed .rc files and the two .cpp files that now
  depend on the new header.
- All required import libraries are already linked: ShellExecuteW (shell32) was already
  used by WeaselServerApp.cpp and SwitcherSettingsDialog.cpp, MessageBoxW is user32.
- The .rc edits keep the file UTF-16LE with BOM and CRLF line endings, which rc.exe and
  the upstream tooling expect.

### Deliberately not done

- WeaselTSF language-bar menu (WeaselTSF/WeaselTSF.rc, IDR_MENU_POPUP / _HANT / _HANS):
  a TSF DLL loaded into every host process; the tray menu already covers the entry, and
  ID_WEASELTRAY_* is shared through include/resource.h, so touching it would also change
  the TSF build. To add it later: three MENUITEM lines plus one case in
  WeaselTSF.cpp _HandleLangBarMenuSelect.
- WeaselSetup.exe "installation options" dialog (IDD_INSTALL_OPTIONS): that page is shown
  while NSIS installs Weasel, before rime-sync is finalized; an entry point there has no
  value. The real settings window is the WeaselDeployer one above.

## Verification

Done locally (Linux, no Windows toolchain):

~~~
# menu item: three LANGUAGE blocks, each right after ID_WEASELTRAY_SYNC
iconv -f UTF-16LE -t UTF-8 third_party/weasel/WeaselServer/WeaselServer.rc | grep -n RIMESYNC
# settings-dialog button: three LANGUAGE blocks
iconv -f UTF-16LE -t UTF-8 third_party/weasel/WeaselDeployer/WeaselDeployer.rc | grep -n RIMESYNC
# wiring
grep -rn "ID_WEASELTRAY_RIMESYNC\|rime_sync::LaunchSetup" third_party/weasel --include=*.cpp --include=*.h
~~~

include/RimeSync.h is covered by test/TestRimeSync (see its README): it compiles the
header against stub windows.h / shellapi.h / std::filesystem (path value_type = wchar_t)
and asserts the resolution order and both MessageBox paths on any C++17 host:

~~~
cd third_party/weasel/test/TestRimeSync
g++ -std=c++17 -Wall -Wextra -Istub -I../../include -o /tmp/rimesync-scenario scenario.cpp
/tmp/rimesync-scenario          # 16 PASS lines, ends with ALL SCENARIOS PASSED
~~~

That directory is not referenced by weasel.sln / xmake.lua / build-weasel.ps1 and is never
part of the Windows build.

There is no x86_64-w64-mingw32-g++, clang, cl or rc.exe on this machine, so the .rc files
were only structurally validated (UTF-16LE BOM, CRLF, BEGIN/END balance, control
rectangles, unique IDs), not compiled.

### Windows verification steps (required before shipping)

Build (CI or a Windows box):

1. pwsh -NoProfile -File scripts/ime/build-weasel.ps1 -Check  -> prints the plan; no error.
2. pwsh -NoProfile -File scripts/ime/build-weasel.ps1  -> must print
   "[weasel] 产物齐全（x64 + Win32）". A bad .rc edit fails here with RC2101/RC2135;
   a bad RimeSync.h fails with a C++ error in WeaselServerApp.cpp /
   SwitcherSettingsDialog.cpp. Both must be silent.
3. Optional non-GUI assertion that the labels really got compiled into the resources:

~~~
function Test-EmbeddedMenuLabel([string]$Exe) {
  $needle = [Text.Encoding]::Unicode.GetBytes('Rime 同步')
  $hay = [IO.File]::ReadAllBytes($Exe)
  for ($i = 0; $i -le $hay.Length - $needle.Length; $i++) {
    if ($hay[$i] -ne $needle[0]) { continue }
    $ok = $true
    for ($j = 1; $j -lt $needle.Length; $j++) {
      if ($hay[$i + $j] -ne $needle[$j]) { $ok = $false; break }
    }
    if ($ok) { return $true }
  }
  return $false
}
Test-EmbeddedMenuLabel 'output\WeaselServer.exe'    # expect True  (menu label)
Test-EmbeddedMenuLabel 'output\WeaselDeployer.exe'  # expect True  (dialog button)
~~~

Manual acceptance (install the rime-sync NSIS package that embeds this Weasel build):

4. %ProgramFiles%\rime-sync\rime-sync-app.exe exists. Right-click the tray icon:
   item 4 reads "Rime 同步… (&Y)" (zh-TW: "Rime 同步… (&Y)", English UI:
   "Rime Sync... (&Y)"). Click it: the rime-sync window opens directly in the setup
   wizard, and Task Manager shows rime-sync-app.exe with a --setup argument.
5. Start Menu -> "Weasel Settings" (or tray -> 输入法设定): the "方案选单设定" dialog shows
   the new "Rime 同步…" / "Rime Sync..." button next to OK; clicking it behaves the same.
   Cancelling that dialog must not deploy or change anything.
6. Not-installed path: rename %ProgramFiles%\rime-sync to %ProgramFiles%\rime-sync.bak and
   delete HKLM\Software\rime-sync, then click either entry: expect a MessageBox titled
   "Rime 同步" saying "未安装 Rime 同步。/ Rime Sync is not installed.". Weasel itself must
   keep working (no crash, no redeploy). Restore both afterwards.
7. Encoding: switch the UI language (WeaselSetup.exe /le, or
   HKCU\Software\Rime\Weasel -> Language = chs / cht / eng) and re-check the tray menu and
   the dialog button in all three languages; no question marks or boxes.
8. Regression: typing, 中/英 switching, 重新部署, 用户资料同步 and 用户词典管理 still work.

## How to upgrade

~~~
cd third_party
rm -rf weasel
git clone --depth 1 --branch <NEW_TAG> https://gh-proxy.com/https://github.com/rime/weasel weasel
rm -rf weasel/.git
~~~

After upgrading, re-apply the local modification above: keep include/RimeSync.h, then
re-add the tray MENUITEM (x3), the ID_WEASELTRAY_RIMESYNC define, the SetupMenuHandlers()
entry, the dialog PUSHBUTTON (x3), the IDC_BUTTON_RIMESYNC define and the OnRimeSync
handler. git diff of the previous vendored commit is the authoritative patch.
