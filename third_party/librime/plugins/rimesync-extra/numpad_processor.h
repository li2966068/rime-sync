// rimesync-extra: numeric keypad support (component "numpad").
//
// The numeric keypad reaches librime as KP_0..KP_9 / KP_Decimal, which no
// schema alphabet and no vnum pattern accepts.  This processor rewrites such a
// key into the plain ASCII key it stands for and lets the engine process that
// key, so the rest of the chain (recognizer, key_binder, speller, punctuator,
// selector) sees exactly what a main-keyboard key produces.
//
// It is intentionally schema agnostic and applies to ordinary pinyin input as
// well: digits behave the same on both keyboards, on both platforms.
//
// KP_Decimal is the one exception to "replay the normalised key": outside a
// composition it is committed directly as ASCII '.', because the punctuation
// chain maps '.' to '。' in Chinese mode and the numpad decimal point must
// always be '.'.  While a composition is in progress it is still replayed as
// '.', so v-mode decimals (v12.34) and ordinary pinyin keep working.

#ifndef RIMESYNC_EXTRA_NUMPAD_PROCESSOR_H_
#define RIMESYNC_EXTRA_NUMPAD_PROCESSOR_H_

#include <rime/common.h>
#include <rime/processor.h>

namespace rime {
class KeyEvent;
}  // namespace rime

namespace rimesync {

class NumpadProcessor : public rime::Processor {
 public:
  explicit NumpadProcessor(const rime::Ticket& ticket);

  rime::ProcessResult ProcessKeyEvent(const rime::KeyEvent& key_event) override;
};

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_NUMPAD_PROCESSOR_H_
