// rimesync-extra: letter selection for the v-mode candidate menu
// (component "rimesync_vselect").
//
// While the input is a v-mode number (^[vV][0-9.]*$) and a candidate menu is
// shown, a/b/c... pick the 1st/2nd/3rd candidate.  Everything else - other
// letters, out-of-range letters, space, Enter, Esc, arrows, paging - is left
// to the normal processor chain.

#ifndef RIMESYNC_EXTRA_VSELECT_PROCESSOR_H_
#define RIMESYNC_EXTRA_VSELECT_PROCESSOR_H_

#include <string>

#include <rime/common.h>
#include <rime/processor.h>

namespace rime {
class Context;
class KeyEvent;
}  // namespace rime

namespace rimesync {

class VselectProcessor : public rime::Processor {
 public:
  explicit VselectProcessor(const rime::Ticket& ticket);

  rime::ProcessResult ProcessKeyEvent(const rime::KeyEvent& key_event) override;

 private:
  void LoadConfig();
  bool VmodeMenuActive() const;
  bool CommitCandidate(size_t index);

  // One character per candidate, at most 8.
  std::string keys_ = "abcdefgh";
};

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_VSELECT_PROCESSOR_H_
