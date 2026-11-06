// Standalone unit tests for the numpad keycode normalisation.
// No librime and no test framework is needed:
//   build: cmake --build <build-dir> --target rimesync_extra_numpad_test
//   run:   <build-dir>/rimesync_extra_numpad_test

#include <cstdio>

#include "numpad_key.h"

namespace {

int g_failures = 0;

void ExpectChar(int keycode, char expected, const char* what) {
  char out = '?';
  const bool ok = rimesync::NormalizeNumpadKeycode(keycode, &out);
  if (ok && out == expected)
    return;
  std::printf("FAIL %s (keycode 0x%x -> %s [%c], want [%c])\n", what, keycode,
              ok ? "normalised" : "not normalised", out, expected);
  ++g_failures;
}

void ExpectUnchanged(int keycode, const char* what) {
  char out = '#';
  if (!rimesync::NormalizeNumpadKeycode(keycode, &out) && out == '#')
    return;
  std::printf("FAIL %s (keycode 0x%x was normalised to [%c])\n", what, keycode,
              out);
  ++g_failures;
}

void TestNumpadDigits() {
  ExpectChar(0xffb0, '0', "KP_0 -> 0");
  ExpectChar(0xffb1, '1', "KP_1 -> 1");
  ExpectChar(0xffb2, '2', "KP_2 -> 2");
  ExpectChar(0xffb3, '3', "KP_3 -> 3");
  ExpectChar(0xffb4, '4', "KP_4 -> 4");
  ExpectChar(0xffb5, '5', "KP_5 -> 5");
  ExpectChar(0xffb6, '6', "KP_6 -> 6");
  ExpectChar(0xffb7, '7', "KP_7 -> 7");
  ExpectChar(0xffb8, '8', "KP_8 -> 8");
  ExpectChar(0xffb9, '9', "KP_9 -> 9");
}

void TestNumpadDecimal() {
  ExpectChar(0xffae, '.', "KP_Decimal -> .");
}

void TestOtherKeysAreLeftAlone() {
  ExpectUnchanged('1', "plain 1 is not a numpad key");
  ExpectUnchanged('0', "plain 0 is not a numpad key");
  ExpectUnchanged('.', "plain . is not a numpad key");
  ExpectUnchanged('v', "letters are not numpad keys");
  ExpectUnchanged(0xff8d, "KP_Enter is left alone");
  ExpectUnchanged(0xffab, "KP_Add is left alone");
  ExpectUnchanged(0xffaf, "KP_Divide is left alone");
  ExpectUnchanged(0xff95, "KP_Home is left alone");
  ExpectUnchanged(0x0020, "space is left alone");
  ExpectUnchanged(0xffae - 1, "just below KP_Decimal is left alone");
  ExpectUnchanged(0xffba, "just above KP_9 is left alone");
  ExpectUnchanged(-1, "negative keycode is left alone");
}

void TestNullOut() {
  if (rimesync::NormalizeNumpadKeycode(0xffb1, nullptr)) {
    std::printf("FAIL a null out pointer must not be written\n");
    ++g_failures;
  }
}

}  // namespace

int main() {
  TestNumpadDigits();
  TestNumpadDecimal();
  TestOtherKeysAreLeftAlone();
  TestNullOut();
  if (g_failures != 0) {
    std::printf("\n%d check(s) failed\n", g_failures);
    return 1;
  }
  std::printf("all numpad key checks passed\n");
  return 0;
}
