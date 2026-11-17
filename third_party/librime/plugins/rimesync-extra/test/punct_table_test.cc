// Standalone unit tests for the Chinese punctuation key table.
// No librime and no test framework is needed:
//   build: cmake --build <build-dir> --target rimesync_extra_punct_test
//   run:   <build-dir>/rimesync_extra_punct_test
//
// The table is also checked for **reachability**: an entry whose alternatives
// nobody can open is dead code.  That was the 2026-11-17 defect: the Shift
// lookup always mapped the pressed character back to its base key, so the
// entries keyed by a shifted character ('$', '%', '*', '~', '!', '?', ':',
// '<', '>', '{', '}') could never be opened.

#include <cstdio>
#include <map>
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

// The menu a Shift press on 'pressed' opens (nullptr = no menu).
// The table is passed in: the returned pointer points into it.
const rimesync::PunctEntry* ShiftMenu(
    const std::map<char, rimesync::PunctEntry>& table,
    char pressed) {
  return rimesync::FindShiftAlternatives(table, pressed);
}

void ExpectShiftMenu(char pressed,
                     char expected_key,
                     const char* what) {
  auto table = rimesync::DefaultPunctTable();
  const rimesync::PunctEntry* found = ShiftMenu(table, pressed);
  if (!found) {
    std::printf("FAIL %s (Shift+%c opens no menu)\n", what, pressed);
    ++g_failures;
    return;
  }
  auto it = table.find(expected_key);
  if (it == table.end() || &it->second != found) {
    std::printf("FAIL %s (Shift+%c opens the menu of another key)\n", what,
                pressed);
    ++g_failures;
  }
}

void ExpectNoShiftMenu(char pressed, const char* what) {
  auto table = rimesync::DefaultPunctTable();
  if (ShiftMenu(table, pressed) == nullptr)
    return;
  std::printf("FAIL %s (Shift+%c must not open a menu)\n", what, pressed);
  ++g_failures;
}

// Every entry that has alternatives must be openable by some Shift press.
// This is the assertion that catches the 2026-11-17 defect: the old lookup
// made 11 of the 18 entries unreachable, and the table test still passed.
void TestEveryEntryWithAlternativesIsReachable() {
  auto table = rimesync::DefaultPunctTable();
  for (const auto& item : table) {
    const char key = item.first;
    const rimesync::PunctEntry& entry = item.second;
    if (entry.alternatives.empty())
      continue;
    bool reachable = false;
    char by_which = 0;
    for (int c = 0x21; c < 0x7f && !reachable; ++c) {
      const rimesync::PunctEntry* found =
          rimesync::FindShiftAlternatives(table, static_cast<char>(c));
      if (found == &entry) {
        reachable = true;
        by_which = static_cast<char>(c);
      }
    }
    if (!reachable) {
      std::printf("FAIL entry '%c' has alternatives but no Shift press opens "
                  "them\n", key);
      ++g_failures;
      continue;
    }
    std::printf("ok   entry '%c' is reachable with Shift+%c\n", key, by_which);
  }
}

// The other direction: no entry without alternatives may be opened as a menu.
void TestEntriesWithoutAlternativesAreNotOpened() {
  auto table = rimesync::DefaultPunctTable();
  for (const auto& item : table) {
    if (!item.second.alternatives.empty())
      continue;
    for (int c = 0x21; c < 0x7f; ++c) {
      const rimesync::PunctEntry* found =
          rimesync::FindShiftAlternatives(table, static_cast<char>(c));
      if (found == &item.second) {
        std::printf("FAIL entry '%c' has no alternatives but Shift+%c opens it\n",
                    item.first, static_cast<char>(c));
        ++g_failures;
      }
    }
  }
}

// The shifted twin of a key must be reachable with the very character the
// user presses, and the menu must start with that twin's main character.
// The backslash key is the exception: its Shift twin '|' has no entry of its
// own, so its menu lives on the '\\' entry and does not contain '\\'s own
// main character (the convention is "the menu never offers the key's own
// main character").
void TestShiftTwinsOwnTheirMenu() {
  struct Expectation {
    char pressed;
    char key;
  };
  const Expectation expectations[] = {
      {'<', '<'}, {',', 0}, {'>', '>'}, {'.', 0}, {':', ':'}, {';', 0},
      {'{', '{'}, {'[', 0}, {'}', '}'}, {']', 0},  {'?', '?'}, {'/', 0},
      {'!', '!'}, {'$', '$'}, {'%', '%'}, {'*', '*'}, {'~', '~'},
  };
  auto table = rimesync::DefaultPunctTable();
  for (const Expectation& expectation : expectations) {
    if (expectation.key == 0)
      continue;
    ExpectShiftMenu(expectation.pressed, expectation.key,
                    "shift twin owns its menu");
    const rimesync::PunctEntry* found = ShiftMenu(table, expectation.pressed);
    if (!found)
      continue;
    if (found->alternatives.empty() ||
        found->alternatives.front() != found->main) {
      std::printf("FAIL the menu of '%c' must start with its main character "
                  "[%s]\n", expectation.key, found->main.c_str());
      ++g_failures;
    }
  }
}

}  // namespace

int main() {
  // 2026-11-11 requirement: '/' commits the Chinese comma, backslash
  // commits the fullwidth backslash.
  ExpectMain('/', "\u3001", "slash key commits the Chinese comma");
  ExpectMain('\\', "\uff3c", "backslash key commits fullwidth backslash");

  // '/' must have no alternatives of its own: its Shift twin '?' carries the
  // menu (Shift+/ opens '?' '?').
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

  // The comma family: Shift+, presses '<' and opens '<'s menu.
  ExpectShiftMenu('<', '<', "shift comma opens the less-than menu");
  ExpectShiftMenu('>', '>', "shift period opens the greater-than menu");
  ExpectShiftMenu(':', ':', "shift semicolon opens the colon menu");
  ExpectShiftMenu('{', '{', "shift bracket-left opens the brace menu");
  ExpectShiftMenu('}', '}', "shift bracket-right opens the brace menu");
  ExpectShiftMenu('|', '\\', "shift backslash opens the backslash menu");
  ExpectShiftMenu('$', '$', "shift 4 opens the dollar menu");
  ExpectShiftMenu('%', '%', "shift 5 opens the percent menu");
  ExpectShiftMenu('*', '*', "shift 8 opens the asterisk menu");
  ExpectShiftMenu('~', '~', "shift backtick opens the tilde menu");
  ExpectShiftMenu('!', '!', "shift 1 opens the exclamation menu");
  ExpectShiftMenu('?', '?', "shift slash opens the question mark menu");

  // The unshifted characters of those keys have no menu of their own.
  ExpectNoShiftMenu(',', "comma key");
  ExpectNoShiftMenu('.', "period key");
  ExpectNoShiftMenu(';', "semicolon key");
  ExpectNoShiftMenu('[', "bracket-left key");
  ExpectNoShiftMenu(']', "bracket-right key");
  ExpectNoShiftMenu('/', "slash key");

  TestEveryEntryWithAlternativesIsReachable();
  TestEntriesWithoutAlternativesAreNotOpened();
  TestShiftTwinsOwnTheirMenu();

  if (g_failures) {
    std::printf("%d check(s) failed\n", g_failures);
    return 1;
  }
  std::printf("all punctuation table checks passed\n");
  return 0;
}
