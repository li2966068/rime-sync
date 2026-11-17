// rimesync-extra: Chinese punctuation processor (component "rimesync_punct").
//
// Without Shift the main Chinese punctuation of an intercepted key is
// committed immediately (no candidate list, no extra space). With Shift the
// alternatives of that key are offered in the candidate menu; Esc cancels.
//
// The table is indexed by the character that arrives from the keyboard, which
// for a shifted key is already the shifted character (Shift+, arrives as
// '<').  The Shift menu of a key is therefore looked up by that character
// first (see FindShiftAlternatives in punct_table.h).
//
// The config item rimesync_punct/keys overrides single characters of the
// table; the key is the character as it arrives from the keyboard, so a
// Shift menu belongs on the shifted character ('<', '$', ...).  The value is
// either a string (the main character) or a list (first item = main
// character, the rest = the Shift alternatives menu).

#ifndef RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_
#define RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_

#include <map>
#include <string>
#include <vector>

#include <rime/common.h>
#include <rime/processor.h>

#include "punct_table.h"

namespace rime {
class Context;
class KeyEvent;
}  // namespace rime

namespace rimesync {

class PunctProcessor : public rime::Processor {
 public:
  explicit PunctProcessor(const rime::Ticket& ticket);

  rime::ProcessResult ProcessKeyEvent(const rime::KeyEvent& key_event) override;

 private:
  void LoadConfig();
  bool AltMenuActive() const;
  bool ShowAlternatives(rime::Context* ctx, char pressed, const PunctEntry& entry);
  bool CommitAlternative(size_t index);
  rime::ProcessResult HandleAltMenuKey(const rime::KeyEvent& key_event);

  std::map<char, PunctEntry> table_;
  bool shift_alternatives_ = true;
};

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_
