// Standalone unit tests for the rimesync-extra conversion functions.
// No librime and no test framework is needed:
//   build: cmake --build <build-dir> --target rimesync_extra_vnum_test
//   run:   <build-dir>/rimesync_extra_vnum_test
//   or:    g++ -std=c++17 -I. vnum_convert.cc test/vnum_convert_test.cc

#include <cstdio>
#include <string>

#include "vnum_convert.h"

namespace {

int g_failures = 0;

void ExpectEq(const std::string& actual,
              const std::string& expected,
              const char* what) {
  if (actual == expected)
    return;
  std::printf("FAIL %s\n  got:  [%s]\n  want: [%s]\n", what, actual.c_str(),
              expected.c_str());
  ++g_failures;
}

void ExpectTrue(bool condition, const char* what) {
  if (condition)
    return;
  std::printf("FAIL %s\n", what);
  ++g_failures;
}

void TestUpperAmount() {
  using rimesync::to_upper_amount;
  ExpectEq(to_upper_amount(0, 0), u8"零元整", "0");
  ExpectEq(to_upper_amount(10, 0), u8"壹拾元整", "10");
  ExpectEq(to_upper_amount(1005, 0), u8"壹仟零伍元整", "1005");
  ExpectEq(to_upper_amount(10005, 0), u8"壹万零伍元整", "10005");
  ExpectEq(to_upper_amount(123456789, 0),
           u8"壹亿贰仟叁佰肆拾伍万陆仟柒佰捌拾玖元整", "123456789");
  ExpectEq(to_upper_amount(0, 5), u8"零元零伍分", "0.05");
  ExpectEq(to_upper_amount(1, 50), u8"壹元伍角整", "1.5");
  ExpectEq(to_upper_amount(-12, 30), u8"负壹拾贰元叁角整", "-12.3");
  ExpectEq(to_upper_amount(123, 45), u8"壹佰贰拾叁元肆角伍分", "123.45");
  ExpectEq(to_upper_amount(1, 5), u8"壹元零伍分", "1.05");
  ExpectEq(to_upper_amount(100000000, 0), u8"壹亿元整", "100000000");
  ExpectEq(to_upper_amount(10000000010LL, 0), u8"壹佰亿零壹拾元整",
           "10000000010");
  ExpectEq(to_upper_amount(999999999999999999LL, 0),
           u8"玖拾玖亿亿玖仟玖佰玖拾玖万亿玖仟玖佰玖拾玖亿玖仟玖佰玖拾玖万玖"
           u8"仟玖佰玖拾玖元整",
           "18 digits");
}

void TestLowerChinese() {
  using rimesync::to_lower_chinese;
  ExpectEq(to_lower_chinese(u8"0"), u8"零", "lower 0");
  ExpectEq(to_lower_chinese(u8"10"), u8"十", "lower 10");
  ExpectEq(to_lower_chinese(u8"15"), u8"十五", "lower 15");
  ExpectEq(to_lower_chinese(u8"1005"), u8"一千零五", "lower 1005");
  ExpectEq(to_lower_chinese(u8"10005"), u8"一万零五", "lower 10005");
  ExpectEq(to_lower_chinese(u8"123456789"), u8"一亿二千三百四十五万六千七百八十九",
           "lower 123456789");
  ExpectEq(to_lower_chinese(u8"0.05"), u8"零点零五", "lower 0.05");
  ExpectEq(to_lower_chinese(u8"1.5"), u8"一点五", "lower 1.5");
  ExpectEq(to_lower_chinese(u8"-12.3"), u8"负十二点三", "lower -12.3");
  ExpectEq(to_lower_chinese(u8"123.45"), u8"一百二十三点四五", "lower 123.45");
  ExpectEq(to_lower_chinese(u8"110"), u8"一百一十", "lower 110");
  ExpectEq(to_lower_chinese(u8"100000"), u8"十万", "lower 100000");
}

void TestParseLimits() {
  using rimesync::ParseVnum;
  using rimesync::VnumValue;
  VnumValue value;

  ExpectTrue(ParseVnum(u8"123", &value), "parse 123");
  ExpectTrue(ParseVnum(u8"-12.3", &value), "parse -12.3");
  ExpectTrue(ParseVnum(u8"123456789012345678", &value), "parse 18 digits");

  ExpectTrue(!ParseVnum(u8"1234567890123456789", &value), "19 digits rejected");
  ExpectTrue(!ParseVnum(u8"1.234", &value), "3 fraction digits rejected");
  ExpectTrue(!ParseVnum(u8"", &value), "empty rejected");
  ExpectTrue(!ParseVnum(u8"12a", &value), "non digit rejected");
  ExpectTrue(!ParseVnum(u8"1.", &value), "trailing dot rejected");
  ExpectTrue(!ParseVnum(u8".5", &value), "missing integer part rejected");
  ExpectTrue(!ParseVnum(u8"--1", &value), "double sign rejected");

  // Over-long input produces no candidate at all.
  ExpectEq(rimesync::to_lower_chinese(u8"1234567890123456789"), std::string(),
           "over-long lower rejected");
}

void TestVnumToAmount() {
  using rimesync::ParseVnum;
  using rimesync::VnumToAmount;
  using rimesync::VnumValue;
  VnumValue value;
  int64_t yuan = 0;
  int fen = 0;

  ExpectTrue(ParseVnum(u8"1234.5", &value) && VnumToAmount(value, &yuan, &fen),
             "amount 1234.5");
  ExpectTrue(yuan == 1234 && fen == 50, "amount 1234.5 = 1234 yuan 50 fen");
  ExpectTrue(ParseVnum(u8"0.05", &value) && VnumToAmount(value, &yuan, &fen),
             "amount 0.05");
  ExpectTrue(yuan == 0 && fen == 5, "amount 0.05 = 0 yuan 5 fen");
  ExpectTrue(ParseVnum(u8"-12.3", &value) && VnumToAmount(value, &yuan, &fen),
             "amount -12.3");
  ExpectTrue(yuan == -12 && fen == 30, "amount -12.3 = -12 yuan 30 fen");
}

}  // namespace

int main() {
  TestUpperAmount();
  TestLowerChinese();
  TestParseLimits();
  TestVnumToAmount();
  if (g_failures != 0) {
    std::printf("\n%d check(s) failed\n", g_failures);
    return 1;
  }
  std::printf("all rimesync-extra conversion checks passed\n");
  return 0;
}
