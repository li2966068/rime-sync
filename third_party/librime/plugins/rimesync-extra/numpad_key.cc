// rimesync-extra: numpad keycode normalisation.

#include "numpad_key.h"

namespace rimesync {

bool NormalizeNumpadKeycode(int keycode, char* out) {
  if (!out)
    return false;
  if (keycode >= kKp0 && keycode <= kKp9) {
    *out = static_cast<char>('0' + (keycode - kKp0));
    return true;
  }
  if (keycode == kKpDecimal) {
    *out = '.';
    return true;
  }
  return false;
}

}  // namespace rimesync
