// rimesync-extra: number to Chinese conversion.
//
// All literals are written as u8"" so the bytes are UTF-8 on every compiler
// (MSVC converts narrow literals to the execution charset unless /utf-8 is
// used; u8 literals are UTF-8 by definition).

#include "vnum_convert.h"

#include <cstddef>
#include <cstdint>
#include <string>

namespace rimesync {
namespace {

const char* const kUpperDigit[10] = {u8"零", u8"壹", u8"贰", u8"叁", u8"肆",
                                     u8"伍", u8"陆", u8"柒", u8"捌", u8"玖"};
const char* const kLowerDigit[10] = {u8"零", u8"一", u8"二", u8"三", u8"四",
                                     u8"五", u8"六", u8"七", u8"八", u8"九"};
const char* const kUpperPlace[4] = {u8"仟", u8"佰", u8"拾", u8""};
const char* const kLowerPlace[4] = {u8"千", u8"百", u8"十", u8""};

// 10^0, 10^4, 10^8, 10^12, 10^16.  Five groups cover 18 integer digits.
const char* const kGroupUnit[5] = {u8"", u8"万", u8"亿", u8"万亿", u8"亿亿"};

const int kPlaceValue[4] = {1000, 100, 10, 1};
const size_t kMaxIntegerDigits = 18;
const size_t kMaxFractionDigits = 2;

// Renders a 1..4 digit group, inserting 零 between digits separated by zeros
// and dropping trailing zeros: 1005 -> 壹仟零伍, 10 -> 壹拾, 1000 -> 壹仟.
std::string FourDigitGroup(int value,
                           const char* const* digits,
                           const char* const* places) {
  std::string out;
  bool pending_zero = false;
  for (int i = 0; i < 4; ++i) {
    int digit = value / kPlaceValue[i] % 10;
    if (digit == 0) {
      if (!out.empty())
        pending_zero = true;
      continue;
    }
    if (pending_zero) {
      out += digits[0];
      pending_zero = false;
    }
    out += digits[digit];
    out += places[i];
  }
  return out;
}

// Renders a non-negative integer, joining 4 digit groups with 万/亿/万亿/亿亿
// and inserting the 零 that bridges a group below 1000 into the previous one:
// 10005 -> 壹万零伍, 10000000010 -> 壹亿零壹拾.
std::string IntegerText(uint64_t value,
                        const char* const* digits,
                        const char* const* places) {
  if (value == 0)
    return digits[0];
  uint64_t group[5] = {0, 0, 0, 0, 0};
  for (int i = 0; i < 5; ++i) {
    group[i] = value % 10000;
    value /= 10000;
  }
  std::string out;
  bool pending_zero = false;
  for (int i = 4; i >= 0; --i) {
    if (group[i] == 0) {
      if (!out.empty())
        pending_zero = true;
      continue;
    }
    if (!out.empty() && group[i] < 1000)
      pending_zero = true;
    if (pending_zero) {
      out += digits[0];
      pending_zero = false;
    }
    out += FourDigitGroup(static_cast<int>(group[i]), digits, places);
    out += kGroupUnit[i];
  }
  return out;
}

uint64_t AbsYuan(int64_t yuan) {
  if (yuan >= 0)
    return static_cast<uint64_t>(yuan);
  // Avoid the undefined behaviour of -INT64_MIN.
  return static_cast<uint64_t>(-(yuan + 1)) + 1;
}

bool ParseDigits(const std::string& text,
                 size_t begin,
                 size_t* end,
                 std::string* digits) {
  size_t i = begin;
  while (i < text.size() && text[i] >= '0' && text[i] <= '9')
    ++i;
  if (i == begin)
    return false;
  *digits = text.substr(begin, i - begin);
  *end = i;
  return true;
}

}  // namespace

bool ParseVnum(const std::string& text, VnumValue* value) {
  if (!value)
    return false;
  VnumValue parsed;
  size_t pos = 0;
  if (pos < text.size() && text[pos] == '-') {
    parsed.negative = true;
    ++pos;
  }
  std::string digits;
  size_t end = 0;
  if (!ParseDigits(text, pos, &end, &digits))
    return false;
  if (digits.size() > kMaxIntegerDigits)
    return false;
  parsed.int_digits = digits;
  pos = end;
  if (pos < text.size()) {
    if (text[pos] != '.')
      return false;
    ++pos;
    if (!ParseDigits(text, pos, &end, &digits))
      return false;
    if (digits.size() > kMaxFractionDigits)
      return false;
    parsed.has_fraction = true;
    parsed.frac_digits = digits;
    pos = end;
  }
  if (pos != text.size())
    return false;
  *value = parsed;
  return true;
}

bool VnumToAmount(const VnumValue& value, int64_t* yuan, int* fen) {
  if (value.int_digits.empty() ||
      value.int_digits.size() > kMaxIntegerDigits)
    return false;
  uint64_t integer = 0;
  for (char c : value.int_digits) {
    if (c < '0' || c > '9')
      return false;
    integer = integer * 10 + static_cast<uint64_t>(c - '0');
  }
  int fraction = 0;
  for (size_t i = 0; i < value.frac_digits.size() && i < 2; ++i) {
    char c = value.frac_digits[i];
    if (c < '0' || c > '9')
      return false;
    fraction = fraction * 10 + (c - '0');
  }
  if (value.frac_digits.size() == 1)
    fraction *= 10;  // "1.5" is five jiao, i.e. fifty fen
  if (yuan)
    *yuan = value.negative ? -static_cast<int64_t>(integer)
                           : static_cast<int64_t>(integer);
  if (fen)
    *fen = fraction;
  return true;
}

std::string to_upper_amount(int64_t yuan, int fen) {
  if (fen < 0)
    fen = -fen;
  const uint64_t integer = AbsYuan(yuan);
  const bool negative = yuan < 0 && (integer != 0 || fen != 0);

  std::string out;
  if (negative)
    out += u8"负";
  out += IntegerText(integer, kUpperDigit, kUpperPlace);
  out += u8"元";

  const int jiao = fen / 10;
  const int cent = fen % 10;
  if (jiao == 0 && cent == 0) {
    out += u8"整";
  } else if (cent == 0) {
    out += kUpperDigit[jiao];
    out += u8"角整";
  } else {
    if (jiao == 0) {
      out += kUpperDigit[0];
    } else {
      out += kUpperDigit[jiao];
      out += u8"角";
    }
    out += kUpperDigit[cent];
    out += u8"分";
  }
  return out;
}

std::string to_lower_chinese(const std::string& digits) {
  VnumValue value;
  if (!ParseVnum(digits, &value))
    return std::string();

  uint64_t integer = 0;
  for (char c : value.int_digits)
    integer = integer * 10 + static_cast<uint64_t>(c - '0');
  bool all_zero = integer == 0;
  for (char c : value.frac_digits) {
    if (c != '0')
      all_zero = false;
  }

  std::string out;
  if (value.negative && !all_zero)
    out += u8"负";
  std::string text = IntegerText(integer, kLowerDigit, kLowerPlace);
  // 10..19 (and 10万, 10亿, ...) read more naturally without the leading 一:
  // drop the 一 of a leading 一十, keep the 十.
  const std::string kOneTen = u8"一十";
  const std::string kOne = u8"一";
  if (text.compare(0, kOneTen.size(), kOneTen) == 0)
    text.erase(0, kOne.size());
  out += text;
  if (value.has_fraction) {
    out += u8"点";
    for (char c : value.frac_digits)
      out += kLowerDigit[c - '0'];
  }
  return out;
}

}  // namespace rimesync
