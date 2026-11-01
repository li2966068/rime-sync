// rimesync-extra: letter selection for the v-mode candidate menu.

#include "vselect_processor.h"

#include <string>

#include <rime/candidate.h>
#include <rime/composition.h>
#include <rime/config.h>
#include <rime/context.h>
#include <rime/engine.h>
#include <rime/key_event.h>
#include <rime/segmentation.h>
#include <rime/schema.h>

namespace rimesync {

using namespace rime;

namespace {

const size_t kMaxSelectKeys = 8;

// ^[vV][0-9.]*$
bool IsVmodeInput(const string& input) {
  if (input.empty() || (input[0] != 'v' && input[0] != 'V'))
    return false;
  for (size_t i = 1; i < input.size(); ++i) {
    const char ch = input[i];
    if (!((ch >= '0' && ch <= '9') || ch == '.'))
      return false;
  }
  return true;
}

}  // namespace

VselectProcessor::VselectProcessor(const Ticket& ticket) : Processor(ticket) {
  LoadConfig();
}

void VselectProcessor::LoadConfig() {
  Schema* schema = engine_ ? engine_->schema() : nullptr;
  Config* config = schema ? schema->config() : nullptr;
  if (!config)
    return;
  string keys;
  if (config->GetString("rimesync_vselect/keys", &keys) && !keys.empty())
    keys_ = keys;
}

bool VselectProcessor::VmodeMenuActive() const {
  Context* ctx = engine_ ? engine_->context() : nullptr;
  if (!ctx || !ctx->HasMenu())
    return false;
  return IsVmodeInput(ctx->input());
}

bool VselectProcessor::CommitCandidate(size_t index) {
  Context* ctx = engine_->context();
  if (!ctx || ctx->composition().empty())
    return false;
  an<Candidate> candidate = ctx->composition().back().GetCandidateAt(index);
  if (!candidate)  // out of range: leave the key to the other processors
    return false;
  const string text = candidate->text();
  ctx->Clear();
  engine_->CommitText(text);
  return true;
}

ProcessResult VselectProcessor::ProcessKeyEvent(const KeyEvent& key_event) {
  if (key_event.release() || key_event.ctrl() || key_event.alt() ||
      key_event.super())
    return kNoop;
  Context* ctx = engine_ ? engine_->context() : nullptr;
  if (!ctx)
    return kNoop;
  if (ctx->get_option("ascii_mode"))
    return kNoop;
  const int keycode = key_event.keycode();
  if (keycode < 0x20 || keycode >= 0x7f)
    return kNoop;
  const char ch = static_cast<char>(keycode);
  const size_t pos = keys_.find(ch);
  if (pos == string::npos || pos >= kMaxSelectKeys)
    return kNoop;
  if (!VmodeMenuActive())
    return kNoop;
  if (!CommitCandidate(pos))
    return kNoop;
  return kAccepted;
}

}  // namespace rimesync
