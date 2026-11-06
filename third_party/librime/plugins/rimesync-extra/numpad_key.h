// rimesync-extra: numpad keycode normalisation (component "numpad").
//
// librime hands the numeric keypad through as X11 keysyms, i.e. KP_0..KP_9
// (0xffb0..0xffb9) and KP_Decimal (0xffae), while the alphabet, the vnum
// pattern and the translator only understand ASCII digits and '.'.  Weasel
// uses the same keycodes (see third_party/weasel/include/KeyEvent.h), so one
// table serves both platforms.
//
// The function below is free of any librime dependency so that it can be
// compiled and unit tested on its own (see test/numpad_key_test.cc).

#ifndef RIMESYNC_EXTRA_NUMPAD_KEY_H_
#define RIMESYNC_EXTRA_NUMPAD_KEY_H_

namespace rimesync {

// X11 keysyms as delivered by librime / Weasel.
const int kKpDecimal = 0xffae;
const int kKp0 = 0xffb0;
const int kKp9 = 0xffb9;

// Writes the ASCII character a numpad key should behave as ('0'..'9' or '.')
// and returns true.  Returns false (and leaves *out untouched) for every other
// keycode, including KP_Enter, KP_Add and the cursor keys the numpad also
// produces.
bool NormalizeNumpadKeycode(int keycode, char* out);

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_NUMPAD_KEY_H_
