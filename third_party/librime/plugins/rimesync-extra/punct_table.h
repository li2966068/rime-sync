// rimesync-extra: Chinese punctuation key table.
//
// Kept in its own header so it can be unit-tested without librime
// (see test/punct_table_test.cc).

#ifndef RIMESYNC_EXTRA_PUNCT_TABLE_H_
#define RIMESYNC_EXTRA_PUNCT_TABLE_H_

#include <map>
#include <string>
#include <vector>

namespace rimesync {

// One key: what it commits, and the Shift alternatives menu.
struct PunctEntry {
  std::string main;
  std::vector<std::string> alternatives;
};

// The Chinese punctuation key table.
//
// Keys are characters, not physical key positions: librime delivers printable
// keys already shifted (Shift+, arrives as '<'), so the table is indexed by
// character.
//
// 2026-11-11 requirement:
//   - the '/' key (question mark / slash) commits the Chinese comma directly
//   - the backslash key (pipe / backslash) commits the fullwidth backslash
//
// How the Shift menu is found (2026-11-17 fix):
//   Shift + <key> is looked up by **the character the user actually pressed**
//   (see FindShiftAlternatives).  The old code always mapped the pressed
//   character back to its base ASCII key first, so every entry whose key is a
//   *shifted* character ('$', '%', '*', '~', '!', '?', ':', '<', '>', '{', '}')
//   was dead code: BaseKeyOf('$') is '4', and no '4' entry exists, so the
//   alternatives of those entries could never be opened.  11 of the 18 entries
//   were unreachable that way.
//
// Table layout after the fix — one entry per printed character:
//   * the unshifted character of a key holds the main character and has **no**
//     alternatives (the Shift menu of that key lives on its shifted twin);
//   * the shifted character holds the menu of that key.  The menu contains the
//     shifted twin's main character first (that is what the key would have
//     committed) and never the unshifted key's own main character.
//
//   key   unshifted main   Shift menu (first item = the shifted twin)
//   ----  ---------------  ----------------------------------------------
//   '/'   '、'             (none: Shift+/ opens the '?' menu below)
//   '\\'  '＼'            '｜' '·' '\\'
//   ';'   '；'             (Shift+; opens the ':' menu below)
//   ','   '，'             (Shift+, opens the '<' menu below)
//   '.'   '。'             (Shift+. opens the '>' menu below)
//   '['   '「'             (Shift+[ opens the '{' menu below)
//   ']'   '」'             (Shift+] opens the '}' menu below)
//   '<'   '《'            '《' '〈' '＜'
//   '>'   '》'            '》' '〉' '．' '＞'
//   '{'   '『'            '『' '［' '｛'
//   '}'   '』'            '』' '］' '｝'
//   ':'   '：'            '：' '…' '∶' '︰'
//   '?'   '？'            '？' '?'
//   '!'   '！'            '！' '¡' '!'
//   '$'   '￥'            '￥' '＄' '€'
//   '%'   '％'            '％' '‰' '%'
//   '*'   '×'            '×' '＊' '·'
//   '~'   '～'            '～' '〜' '~'
//
// '|' deliberately has **no** entry: it is the Shift twin of '\\', and the
// backslash menu already lives on the '\\' entry (the fallback in
// FindShiftAlternatives).  Typing a halfwidth '/' or '\\' in Chinese mode:
// press Ctrl+. to switch to ascii_mode, or override this table with the
// rimesync_punct/keys config item.
inline std::map<char, PunctEntry> DefaultPunctTable() {
  std::map<char, PunctEntry> table;
  // Key             main      alternatives (first one is the Shift twin)
  table['/'] = PunctEntry{u8"、", {}};
  table['\\'] = PunctEntry{u8"＼", {u8"｜", u8"·", u8"\\"}};
  table[';'] = PunctEntry{u8"；", {}};
  table[','] = PunctEntry{u8"，", {}};
  table['.'] = PunctEntry{u8"。", {}};
  table['['] = PunctEntry{u8"「", {}};
  table[']'] = PunctEntry{u8"」", {}};
  table['<'] = PunctEntry{u8"《", {u8"《", u8"〈", u8"＜"}};
  table['>'] = PunctEntry{u8"》", {u8"》", u8"〉", u8"．", u8"＞"}};
  table['{'] = PunctEntry{u8"『", {u8"『", u8"［", u8"｛"}};
  table['}'] = PunctEntry{u8"』", {u8"』", u8"］", u8"｝"}};
  table['%'] = PunctEntry{u8"％", {u8"％", u8"‰", u8"%"}};
  table['$'] = PunctEntry{u8"￥", {u8"￥", u8"＄", u8"€"}};
  table['*'] = PunctEntry{u8"×", {u8"×", u8"＊", u8"·"}};
  table['~'] = PunctEntry{u8"～", {u8"～", u8"〜", u8"~"}};
  table['!'] = PunctEntry{u8"！", {u8"！", u8"¡", u8"!"}};
  table['?'] = PunctEntry{u8"？", {u8"？", u8"?"}};
  table[':'] = PunctEntry{u8"：", {u8"：", u8"…", u8"∶", u8"︰"}};
  return table;
}

// Maps a shifted symbol back to the ASCII key it is produced from on a US
// layout.  Unmapped characters are their own base.
inline char BaseKeyOf(char ch) {
  switch (ch) {
    case '<': return ',';
    case '>': return '.';
    case '?': return '/';
    case ':': return ';';
    case '{': return '[';
    case '}': return ']';
    case '|': return '\\';
    case '~': return '`';
    case '!': return '1';
    case '@': return '2';
    case '#': return '3';
    case '$': return '4';
    case '%': return '5';
    case '^': return '6';
    case '&': return '7';
    case '*': return '8';
    case '(': return '9';
    case ')': return '0';
    case '_': return '-';
    case '+': return '=';
    case '"': return '\'';
    default: return ch;
  }
}

// Finds the alternatives menu a Shift press should open.
//
// 'pressed' is the character the user actually pressed, already shifted
// (Shift+, arrives as '<' on a US layout).  The lookup order is:
//
//   1. the pressed character itself — Shift+, presses '<' and '<' is an entry
//      of its own, so its menu opens;
//   2. if the pressed character is not in the table, or its entry has no
//      alternatives, the base key it belongs to — Shift+\ presses '|', which
//      is deliberately not an entry, so the backslash menu opens.
//
// Returns nullptr when no menu should be shown.
inline const PunctEntry* FindShiftAlternatives(
    const std::map<char, PunctEntry>& table,
    char pressed) {
  auto direct = table.find(pressed);
  if (direct != table.end() && !direct->second.alternatives.empty())
    return &direct->second;
  const char base = BaseKeyOf(pressed);
  if (base != pressed) {
    auto it = table.find(base);
    if (it != table.end() && !it->second.alternatives.empty())
      return &it->second;
  }
  return nullptr;
}

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_PUNCT_TABLE_H_
