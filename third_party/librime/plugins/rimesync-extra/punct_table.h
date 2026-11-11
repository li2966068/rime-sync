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
// character.  A Shift press is mapped back to its base ASCII key by
// BaseKeyOf() to look up the alternatives menu.
//
// 2026-11-11 requirement:
//   - the '/' key (question mark / slash) commits the Chinese comma directly
//   - the backslash key (pipe / backslash) commits the fullwidth backslash
//
// '/' deliberately has no alternatives: its Shift twin '?' is committed
// directly by table['?'] below.  Giving '/' alternatives would turn Shift+/
// from 'commit ?' into 'show a menu'.  To type the halfwidth '/' or '\\' in
// Chinese mode use Ctrl+. to switch to ascii_mode, or override this table
// with the rimesync_punct/keys config item.
inline std::map<char, PunctEntry> DefaultPunctTable() {
  std::map<char, PunctEntry> table;
  // Key             main      alternatives (first one is the Shift twin)
  table['/'] = PunctEntry{u8"、", {}};
  table['\\'] = PunctEntry{u8"＼", {u8"｜", u8"·", u8"\\"}};
  table[';'] = PunctEntry{u8"；", {u8"：", u8"…"}};
  table[','] = PunctEntry{u8"，", {u8"《", u8"〈", u8"＜"}};
  table['.'] = PunctEntry{u8"。", {u8"》", u8"〉", u8"．"}};
  table['['] = PunctEntry{u8"「", {u8"『", u8"［"}};
  table[']'] = PunctEntry{u8"」", {u8"』", u8"］"}};
  table['<'] = PunctEntry{u8"《", {u8"〈", u8"＜"}};
  table['>'] = PunctEntry{u8"》", {u8"〉", u8"＞"}};
  table['{'] = PunctEntry{u8"『", {u8"｛"}};
  table['}'] = PunctEntry{u8"』", {u8"｝"}};
  table['%'] = PunctEntry{u8"％", {u8"‰", u8"%"}};
  table['$'] = PunctEntry{u8"￥", {u8"＄", u8"€"}};
  table['*'] = PunctEntry{u8"×", {u8"＊", u8"·"}};
  table['~'] = PunctEntry{u8"～", {u8"〜", u8"~"}};
  table['!'] = PunctEntry{u8"！", {u8"¡", u8"!"}};
  table['?'] = PunctEntry{u8"？", {u8"?"}};
  table[':'] = PunctEntry{u8"：", {u8"∶", u8"︰"}};
  return table;
}

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_PUNCT_TABLE_H_