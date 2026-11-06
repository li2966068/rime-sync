// rimesync-extra: numeric keypad support.

#include "numpad_processor.h"

#include <rime/engine.h>
#include <rime/key_event.h>

#include "numpad_key.h"

namespace rimesync {

using namespace rime;

NumpadProcessor::NumpadProcessor(const Ticket& ticket) : Processor(ticket) {}

ProcessResult NumpadProcessor::ProcessKeyEvent(const KeyEvent& key_event) {
  if (key_event.release())
    return kNoop;
  // Ctrl/Alt/Super combinations belong to key_binder and to the client.
  if (key_event.ctrl() || key_event.alt() || key_event.super())
    return kNoop;
  char ch = 0;
  if (!NormalizeNumpadKeycode(key_event.keycode(), &ch))
    return kNoop;
  if (!engine_)
    return kNoop;
  // Re-enter the engine with the normalised key: the nested call cannot loop,
  // because ch is ASCII and this processor returns kNoop for it.  If nothing
  // handles the normalised key either, keep the original KP key alive instead
  // of swallowing it.
  if (!engine_->ProcessKey(KeyEvent(ch, key_event.modifier())))
    return kNoop;
  return kAccepted;
}

}  // namespace rimesync
