#pragma once

// "Rime Sync..." entry point for Weasel (rime-sync local change, see FORK.md).
//
// Used by both Weasel surfaces that expose the entry:
//   * WeaselServer  - tray icon context menu (IDR_MENU_POPUP)
//   * WeaselDeployer- "Weasel Settings" dialog (IDD_SWITCHER_SETTING)
//
// It only does two things: locate rime-sync-app.exe and start it with --setup
// via ShellExecuteW, or tell the user (MessageBoxW) that it is not installed.
//
// Design notes:
//   * Header-only (inline). Each module includes it once, so no new translation
//     unit has to be registered in *.vcxproj / xmake.lua.
//   * Wide-character only: std::filesystem::path + ShellExecuteW + MessageBoxW.
//     The Chinese literals are written as \uXXXX escapes so this file stays
//     pure ASCII and cannot be mangled by the compiler code page or /utf-8.
//   * Read-only: it never touches Weasel's own IME behaviour.

#include <windows.h>
#include <shellapi.h>

#include <cwchar>
#include <filesystem>
#include <string>

namespace rime_sync {

// Registry key written by the rime-sync NSIS installer (see packaging/nsis-hooks.nsh).
inline constexpr const wchar_t* kRegistrySubKey = L"Software\\rime-sync";
// Value holding the installation directory.
inline constexpr const wchar_t* kInstallDirValue = L"InstallDir";
inline constexpr const wchar_t* kExecutableName = L"rime-sync-app.exe";
inline constexpr const wchar_t* kInstallDirName = L"rime-sync";
inline constexpr const wchar_t* kSetupArgument = L"--setup";
// Menu / message caption: U+540C U+6B65 == "Tong Bu" (sync).
inline constexpr const wchar_t* kCaption = L"Rime \u540C\u6B65";

inline bool FileExists(const std::filesystem::path& path) {
  std::error_code ec;
  return std::filesystem::is_regular_file(path, ec) && !ec;
}

// Reads a REG_SZ / REG_EXPAND_SZ value. An empty value_name reads the key's
// default value, which is what the Tauri NSIS template writes for InstallDir.
// sam_desired selects the registry view: Weasel ships both x64 and Win32
// binaries, and a 32-bit process sees the WOW6432Node view by default.
inline bool ReadRegistryString(HKEY root,
                               const wchar_t* value_name,
                               REGSAM sam_desired,
                               std::wstring& out) {
  HKEY key = nullptr;
  if (RegOpenKeyExW(root, kRegistrySubKey, 0, KEY_QUERY_VALUE | sam_desired,
                    &key) != ERROR_SUCCESS) {
    return false;
  }

  DWORD type = 0;
  DWORD size = 0;
  LONG status =
      RegQueryValueExW(key, value_name, nullptr, &type, nullptr, &size);
  if (status != ERROR_SUCCESS ||
      (type != REG_SZ && type != REG_EXPAND_SZ) || size < sizeof(wchar_t)) {
    RegCloseKey(key);
    return false;
  }

  std::wstring buffer(size / sizeof(wchar_t) + 1, L'\0');
  status = RegQueryValueExW(key, value_name, nullptr, &type,
                            reinterpret_cast<LPBYTE>(&buffer[0]), &size);
  RegCloseKey(key);
  if (status != ERROR_SUCCESS) {
    return false;
  }

  buffer.resize(std::wcslen(buffer.c_str()));
  out.swap(buffer);
  return !out.empty();
}

inline std::filesystem::path ExecutableIn(const std::wstring& directory,
                                          const wchar_t* sub_directory) {
  if (directory.empty()) {
    return std::filesystem::path();
  }
  std::filesystem::path candidate(directory);
  if (sub_directory && *sub_directory) {
    candidate /= sub_directory;
  }
  candidate /= kExecutableName;
  return FileExists(candidate) ? candidate : std::filesystem::path();
}

inline std::filesystem::path FromRegistry(HKEY root, REGSAM view) {
  const wchar_t* const values[] = {kInstallDirValue, L""};
  for (const wchar_t* value : values) {
    std::wstring directory;
    if (!ReadRegistryString(root, value, view, directory)) {
      continue;
    }
    std::filesystem::path candidate = ExecutableIn(directory, nullptr);
    if (!candidate.empty()) {
      return candidate;
    }
  }
  return std::filesystem::path();
}

inline std::filesystem::path FromEnvironment(const wchar_t* variable,
                                             const wchar_t* sub_directory) {
  wchar_t buffer[MAX_PATH * 2] = {0};
  DWORD length = GetEnvironmentVariableW(variable, buffer,
                                        sizeof(buffer) / sizeof(buffer[0]));
  if (length == 0 || length >= sizeof(buffer) / sizeof(buffer[0])) {
    return std::filesystem::path();
  }
  return ExecutableIn(buffer, sub_directory);
}

// Resolution order:
//   1. HKLM\Software\rime-sync  InstallDir / default value (64-bit view)
//   2. same, 32-bit (WOW6432Node) view
//   3. HKCU\Software\rime-sync  (per-user install)
//   4. %ProgramW6432%\rime-sync\rime-sync-app.exe
//   5. %ProgramFiles%\rime-sync\rime-sync-app.exe
//   6. %LOCALAPPDATA%\Programs\rime-sync\rime-sync-app.exe (per-user default)
inline std::filesystem::path ResolveExecutable() {
  const HKEY roots[] = {HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER};
  const REGSAM views[] = {KEY_WOW64_64KEY, KEY_WOW64_32KEY};
  for (HKEY root : roots) {
    for (REGSAM view : views) {
      std::filesystem::path candidate = FromRegistry(root, view);
      if (!candidate.empty()) {
        return candidate;
      }
    }
  }

  struct EnvironmentEntry {
    const wchar_t* variable;
    const wchar_t* sub_directory;
  };
  const EnvironmentEntry entries[] = {
      {L"ProgramW6432", kInstallDirName},
      {L"ProgramFiles", kInstallDirName},
      {L"LOCALAPPDATA", L"Programs\\rime-sync"},
  };
  for (const EnvironmentEntry& entry : entries) {
    std::filesystem::path candidate =
        FromEnvironment(entry.variable, entry.sub_directory);
    if (!candidate.empty()) {
      return candidate;
    }
  }
  return std::filesystem::path();
}

// "Rime Sync is not installed." (Chinese first for zh UI, English second)
inline const wchar_t* NotInstalledMessage() {
  return L"\u672A\u5B89\u88C5 Rime \u540C\u6B65\u3002\n\n"
         L"Rime Sync is not installed.";
}

// "Could not start Rime Sync."
inline const wchar_t* LaunchFailedMessage() {
  return L"\u65E0\u6CD5\u542F\u52A8 Rime \u540C\u6B65\u3002\n\n"
         L"Could not start Rime Sync.";
}

// Starts "<install dir>\rime-sync-app.exe --setup".
// Returns true when ShellExecuteW accepted the request; otherwise shows a
// MessageBoxW (owner may be nullptr, e.g. from the tray menu) and returns false.
inline bool LaunchSetup(HWND owner = nullptr) {
  std::filesystem::path executable = ResolveExecutable();
  if (executable.empty()) {
    MessageBoxW(owner, NotInstalledMessage(), kCaption,
                MB_OK | MB_ICONINFORMATION);
    return false;
  }

  HINSTANCE result = ShellExecuteW(owner, L"open", executable.c_str(),
                                   kSetupArgument, nullptr, SW_SHOWNORMAL);
  if (reinterpret_cast<INT_PTR>(result) <= 32) {
    MessageBoxW(owner, LaunchFailedMessage(), kCaption, MB_OK | MB_ICONERROR);
    return false;
  }
  return true;
}

}  // namespace rime_sync
