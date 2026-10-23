#include <RimeSync.h>

#include <cstdio>
#include <cstring>
#include <map>
#include <set>
#include <string>
#include <vector>

namespace {

struct FakeKey {
  HKEY root;
  std::wstring subkey;
  REGSAM view;
  std::map<std::wstring, std::wstring> values;
};

std::vector<FakeKey> g_registry;
std::map<std::wstring, std::wstring> g_env;
std::set<std::wstring> g_files;
std::wstring g_last_message;
std::wstring g_last_caption;
std::wstring g_last_verb;
std::wstring g_last_file;
std::wstring g_last_params;
HINSTANCE g_shell_result = (HINSTANCE)42;
int g_failures = 0;

void Check(bool cond, const char* what) {
  std::printf("%s  %s\n", cond ? "PASS" : "FAIL", what);
  if (!cond) ++g_failures;
}

void Reset() {
  g_registry.clear();
  g_env.clear();
  g_files.clear();
  g_last_message.clear();
  g_last_caption.clear();
  g_last_verb.clear();
  g_last_file.clear();
  g_last_params.clear();
  g_shell_result = (HINSTANCE)42;
}

void AddKey(HKEY root, REGSAM view, const std::wstring& value,
            const std::wstring& directory) {
  FakeKey key{root, L"Software\\rime-sync", view, {}};
  if (!value.empty()) key.values[value] = directory;
  g_registry.push_back(key);
}

std::wstring W(const std::filesystem::path& p) { return std::wstring(p.c_str()); }

}  // namespace

extern "C" LONG RegOpenKeyExW(HKEY root, LPCWSTR subkey, DWORD, REGSAM sam,
                              HKEY* out) {
  const REGSAM view = sam & (KEY_WOW64_64KEY | KEY_WOW64_32KEY);
  for (const FakeKey& key : g_registry) {
    if (key.root == root && key.subkey == subkey && key.view == view) {
      *out = new FakeKey(key);
      return ERROR_SUCCESS;
    }
  }
  return ERROR_FILE_NOT_FOUND;
}

extern "C" LONG RegQueryValueExW(HKEY handle, LPCWSTR name, DWORD*, DWORD* type,
                                 LPBYTE data, DWORD* size) {
  const FakeKey* key = reinterpret_cast<const FakeKey*>(handle);
  std::wstring key_name = name ? name : L"";
  auto it = key->values.find(key_name);
  if (it == key->values.end()) return ERROR_FILE_NOT_FOUND;
  const DWORD needed = static_cast<DWORD>((it->second.size() + 1) * sizeof(wchar_t));
  if (type) *type = REG_SZ;
  if (!data) {
    *size = needed;
    return ERROR_SUCCESS;
  }
  if (*size < needed) return ERROR_MORE_DATA;
  std::memcpy(data, it->second.c_str(), needed);
  *size = needed;
  return ERROR_SUCCESS;
}

extern "C" LONG RegCloseKey(HKEY handle) {
  delete reinterpret_cast<FakeKey*>(handle);
  return ERROR_SUCCESS;
}

extern "C" DWORD GetEnvironmentVariableW(LPCWSTR name, LPWSTR buffer, DWORD size) {
  auto it = g_env.find(name ? name : L"");
  if (it == g_env.end()) return 0;
  if (it->second.size() + 1 > size) return static_cast<DWORD>(it->second.size() + 1);
  std::memcpy(buffer, it->second.c_str(), (it->second.size() + 1) * sizeof(wchar_t));
  return static_cast<DWORD>(it->second.size());
}

extern "C" int MessageBoxW(HWND, LPCWSTR text, LPCWSTR caption, UINT) {
  g_last_message = text ? text : L"";
  g_last_caption = caption ? caption : L"";
  return 1;
}

extern "C" HINSTANCE ShellExecuteW(HWND, LPCWSTR verb, LPCWSTR file,
                                   LPCWSTR params, LPCWSTR, int) {
  g_last_verb = verb ? verb : L"";
  g_last_file = file ? file : L"";
  g_last_params = params ? params : L"";
  return g_shell_result;
}

namespace std {
namespace filesystem {
bool is_regular_file(const path& p, std::error_code&) {
  return g_files.count(std::wstring(p.c_str())) > 0;
}
}  // namespace filesystem
}  // namespace std

int main() {
  Reset();
  AddKey(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, L"InstallDir",
         L"C:\\Program Files\\rime-sync");
  g_files.insert(L"C:\\Program Files\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Program Files\\rime-sync\\rime-sync-app.exe",
        "1a HKLM InstallDir (64-bit) resolved");
  Check(rime_sync::LaunchSetup(nullptr), "1b LaunchSetup succeeds");
  Check(g_last_verb == L"open" && g_last_params == L"--setup",
        "1c ShellExecuteW called with verb=open params=--setup");
  Check(g_last_file == L"C:\\Program Files\\rime-sync\\rime-sync-app.exe",
        "1d ShellExecuteW called with the resolved exe");
  Check(g_last_message.empty(), "1e no MessageBox on success");

  Reset();
  AddKey(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, L"InstallDir", L"D:\\stale\\rime-sync");
  g_env[L"ProgramFiles"] = L"C:\\Program Files";
  g_files.insert(L"C:\\Program Files\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Program Files\\rime-sync\\rime-sync-app.exe",
        "2a missing exe in InstallDir falls back to %ProgramFiles%");

  Reset();
  AddKey(HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, L"InstallDir",
         L"C:\\Program Files (x86)\\rime-sync");
  g_files.insert(L"C:\\Program Files (x86)\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Program Files (x86)\\rime-sync\\rime-sync-app.exe",
        "3a 32-bit (WOW6432Node) InstallDir resolved");

  Reset();
  AddKey(HKEY_CURRENT_USER, KEY_WOW64_64KEY, L"InstallDir",
         L"C:\\Users\\me\\AppData\\Local\\Programs\\rime-sync");
  g_files.insert(
      L"C:\\Users\\me\\AppData\\Local\\Programs\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Users\\me\\AppData\\Local\\Programs\\rime-sync\\rime-sync-app.exe",
        "4a HKCU InstallDir resolved");

  Reset();
  g_registry.push_back(FakeKey{HKEY_LOCAL_MACHINE, L"Software\\rime-sync",
                               KEY_WOW64_64KEY,
                               {{L"", L"C:\\Program Files\\rime-sync"}}});
  g_files.insert(L"C:\\Program Files\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Program Files\\rime-sync\\rime-sync-app.exe",
        "5a registry default value resolved");

  Reset();
  Check(!rime_sync::LaunchSetup(nullptr), "6a LaunchSetup fails when missing");
  Check(g_last_message ==
            L"\u672A\u5B89\u88C5 Rime \u540C\u6B65\u3002\n\n"
            L"Rime Sync is not installed.",
        "6b not-installed MessageBox text (decodes to 未安装 Rime 同步。)");
  Check(g_last_caption == L"Rime \u540C\u6B65", "6c MessageBox caption is Rime 同步");
  Check(g_last_file.empty(), "6d ShellExecuteW not called");

  Reset();
  g_env[L"ProgramFiles"] = L"C:\\Program Files";
  g_files.insert(L"C:\\Program Files\\rime-sync\\rime-sync-app.exe");
  g_shell_result = (HINSTANCE)2;
  Check(!rime_sync::LaunchSetup(nullptr), "7a LaunchSetup fails on ShellExecute error");
  Check(g_last_message.rfind(L"\u65E0\u6CD5\u542F\u52A8", 0) == 0,
        "7b launch-failed MessageBox text (decodes to 无法启动...)");

  Reset();
  g_env[L"LOCALAPPDATA"] = L"C:\\Users\\me\\AppData\\Local";
  g_files.insert(
      L"C:\\Users\\me\\AppData\\Local\\Programs\\rime-sync\\rime-sync-app.exe");
  Check(W(rime_sync::ResolveExecutable()) ==
            L"C:\\Users\\me\\AppData\\Local\\Programs\\rime-sync\\rime-sync-app.exe",
        "8a %LOCALAPPDATA%\\Programs\\rime-sync fallback");

  std::printf("\n%s\n", g_failures ? "FAILURES PRESENT" : "ALL SCENARIOS PASSED");
  return g_failures ? 1 : 0;
}
