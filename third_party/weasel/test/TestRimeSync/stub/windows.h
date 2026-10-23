#pragma once
// Minimal, slightly functional windows.h stub for behavioural tests of include/RimeSync.h.
#include <cwchar>
typedef unsigned long DWORD;
typedef long LONG;
typedef unsigned char BYTE;
typedef BYTE* LPBYTE;
typedef long long INT_PTR;
typedef unsigned int UINT;
typedef unsigned int REGSAM;
typedef void* HKEY;
typedef void* HINSTANCE;
typedef void* HWND;
typedef const wchar_t* LPCWSTR;
typedef wchar_t* LPWSTR;
#define MAX_PATH 260
#define ERROR_SUCCESS 0L
#define ERROR_FILE_NOT_FOUND 2L
#define ERROR_MORE_DATA 234L
#define REG_SZ 1
#define REG_EXPAND_SZ 2
#define KEY_QUERY_VALUE 0x0001
#define KEY_WOW64_64KEY 0x0100
#define KEY_WOW64_32KEY 0x0200
#define MB_OK 0x00000000L
#define MB_ICONINFORMATION 0x00000040L
#define MB_ICONERROR 0x00000010L
#define SW_SHOWNORMAL 1
#define HKEY_LOCAL_MACHINE ((HKEY)0x80000002)
#define HKEY_CURRENT_USER ((HKEY)0x80000001)
extern "C" LONG RegOpenKeyExW(HKEY, LPCWSTR, DWORD, REGSAM, HKEY*);
extern "C" LONG RegQueryValueExW(HKEY, LPCWSTR, DWORD*, DWORD*, LPBYTE, DWORD*);
extern "C" LONG RegCloseKey(HKEY);
extern "C" DWORD GetEnvironmentVariableW(LPCWSTR, LPWSTR, DWORD);
extern "C" int MessageBoxW(HWND, LPCWSTR, LPCWSTR, UINT);
