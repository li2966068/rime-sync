// rimesync-extra: Chinese punctuation processor (component "rimesync_punct").
//
// Without Shift the main Chinese punctuation of an intercepted key is
// committed immediately (no candidate list, no extra space). With Shift the
// alternatives of that key are offered in the candidate menu; Esc cancels.

#ifndef RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_
#define RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_

#include <map>
#include <string>
#include <vector>

#include <rime/common.h>
#include <rime/processor.h>

namespace rime {
class Context;
class KeyEvent;
}  // namespace rime

namespace rimesync {

struct PunctEntry {
  std::string main;
  std::vector<std::string> alternatives;
};

class PunctProcessor : public rime::Processor {
 public:
  explicit PunctProcessor(const rime::Ticket& ticket);

  rime::ProcessResult ProcessKeyEvent(const rime::KeyEvent& key_event) override;

 private:
  void LoadConfig();
  bool AltMenuActive() const;
  bool ShowAlternatives(rime::Context* ctx, char base, const PunctEntry& entry);
  bool CommitAlternative(size_t index);
  rime::ProcessResult HandleAltMenuKey(const rime::KeyEvent& key_event);

  std::map<char, PunctEntry> table_;
  bool shift_alternatives_ = true;
};

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_PUNCT_PROCESSOR_H_
