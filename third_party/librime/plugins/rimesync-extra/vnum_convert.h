// rimesync-extra: number to Chinese conversion for the v-mode translator.
//
// These functions are deliberately free of any librime dependency so that
// they can be compiled and unit tested on their own (see test/).

#ifndef RIMESYNC_EXTRA_VNUM_CONVERT_H_
#define RIMESYNC_EXTRA_VNUM_CONVERT_H_

#include <cstdint>
#include <string>

namespace rimesync {

// A plain decimal number without a 'v' prefix: optional '-', 1..18 integer
// digits, optionally '.' followed by 1..2 digits.
struct VnumValue {
  bool negative = false;
  std::string int_digits;   // as typed, sign removed
  std::string frac_digits;  // as typed, '.' removed
  bool has_fraction = false;
};

// Parses "123", "1234.5", "0.05", "-12.3".
// Returns false (and leaves *value untouched) on any other shape, including
// empty input, more than 18 integer digits and more than 2 fractional digits.
bool ParseVnum(const std::string& text, VnumValue* value);

// Splits a parsed value into whole yuan and fen (0..99).
bool VnumToAmount(const VnumValue& value, int64_t* yuan, int* fen);

// Financial upper case: 12345 -> "壹佰贰拾叁元肆角伍分", 10 -> "壹拾元整",
// 1005 -> "壹仟零伍元整", 10005 -> "壹万零伍元整", 0.05 -> "零元零伍分".
// fen must be in [0, 99]; yuan may be negative (prefix "负").
std::string to_upper_amount(int64_t yuan, int fen);

// Chinese lower case: "1234.5" -> "一千二百三十四点五", "10" -> "十",
// "0.05" -> "零点零五", "-12.3" -> "负十二点三".
// Returns an empty string when the text is not a valid number (see
// ParseVnum), which is how over-long input is rejected.
std::string to_lower_chinese(const std::string& digits);

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_VNUM_CONVERT_H_
