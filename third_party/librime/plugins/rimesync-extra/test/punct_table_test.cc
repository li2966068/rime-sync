// Standalone unit tests for the Chinese punctuation key table.
// No librime and no test framework is needed:
//   build: cmake --build <build-dir> --target rimesync_extra_punct_test
//   run:   <build-dir>/rimesync_extra_punct_test

#include <cstdio>
#include <string>
#include <vector>

#include "punct_table.h"

namespace {

int g_failures = 0;

void ExpectEq(const std::string& actual,
              const std::string& expected,
              const char* what) {
  if (actual == expected)
    return;
  std::printf("FAIL %s (got [%s], want [%s])\n", what, actual.c_str(),
              expected.c_str());
  ++g_failures;
}

void ExpectMain(char key, const char* expected, const char* what) {
  auto table = rimesync::DefaultPunctTable();
  auto it = table.find(key);
  if (it == table.end()) {
    std::printf("FAIL %s (key '%c' is not in the table)\n", what, key);
    ++g_failures;
    return;
  }
  ExpectEq(it->second.main, expected, what);
}

void ExpectAlternatives(char key,
                        const std::vector<std::string>& expected,
                        const char* what) {
  auto table = rimesync::DefaultPunctTable();
  auto it = table.find(key);
  if (it == table.end()) {
    std::printf("FAIL %s (key '%c' is not in the table)\n", what, key);
    ++g_failures;
    return;
  }
  if (it->second.alternatives != expected) {
    std::printf("FAIL %s (got %zu alternatives, want %zu)\n", what,
                it->second.alternatives.size(), expected.size());
    ++g_failures;
  }
}

void ExpectAbsent(char key, const char* what) {
  auto table = rimesync::DefaultPunctTable();
  if (table.find(key) == table.end())
    return;
  std::printf("FAIL %s (key '%c' must not have its own entry)\n", what, key);
  ++g_failures;
}

}  // namespace

int main() {
  // 2026-11-11 requirement: '/' commits the Chinese comma, backslash
  // commits the fullwidth backslash.
  ExpectMain('/', "\u3001", "slash key commits the Chinese comma");
  ExpectMain('\\', "\uff3c", "backslash key commits fullwidth backslash");

  // '/' must have no alternatives: Shift+/ has to keep committing '?'
  // directly instead of opening a menu.
  ExpectAlternatives('/', std::vector<std::string>(),
                     "slash key has no alternatives");

  // Shift+backslash opens the menu; first entry is the Shift twin.
  std::vector<std::string> backslash_alts;
  backslash_alts.push_back("\uff5c");
  backslash_alts.push_back("\u00b7");
  backslash_alts.push_back("\\");
  ExpectAlternatives('\\', backslash_alts,
                   "backslash key alternatives");

  // Shift+/ commits the fullwidth question mark.
  ExpectMain('?', "\uff1f", "question mark is committed directly");

  // '|' is the Shift twin of backslash and always resolves through the
  // base key's menu, so it must not have an entry of its own.
  ExpectAbsent('|', "pipe key");

  if (g_failures) {
    std::printf("%d check(s) failed\n", g_failures);
    return 1;
  }
  std::printf("all punctuation table checks passed\n");
  return 0;
}