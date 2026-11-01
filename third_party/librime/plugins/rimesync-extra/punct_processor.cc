// rimesync-extra: Chinese punctuation processor.
//
// Notes on key codes: librime delivers printable keys as X11 keysyms, so a
// shifted key already arrives as its shifted character (Shift+, is '<').  The
// table is therefore indexed by character, and a Shift modifier means "give me
// the alternatives of the key this character belongs to".  When that base key
// is not intercepted (e.g. '$' from Shift+4) the character's own main symbol is
// committed, which is what the table says for '\$'.
// Set rimesync_punct/shift_alternatives: false to always commit the main
// symbol and skip the alternatives menu.
//
// '/' is not intercepted on purpose (see DefaultTable()).

#include "punct_processor.h"

#include <string>
#include <utility>

#include <rime/candidate.h>
#include <rime/composition.h>
#include <rime/config.h>
#include <rime/context.h>
#include <rime/engine.h>
#include <rime/key_event.h>
#include <rime/key_table.h>
#include <rime/menu.h>
#include <rime/schema.h>
#include <rime/segmentation.h>
#include <rime/translation.h>

namespace rimesync {

using namespace rime;

namespace {

const char kAltTag[] = "rimesync_punct_alt";
const char kComponentName[] = "rimesync_punct";

// Maps a shifted symbol back to the ASCII key it is produced from on a US
// layout.  Unmapped characters are their own base.
char BaseKeyOf(char ch) {
  switch (ch) {
    case '<': return ',';
    case '>': return '.';
    case '?': return '/';
    case ':': return ';';
    case '{': return '[';
    case '}': return ']';
    case '|': return '\\';
    case '~': return '`';
    case '!': return '1';
    case '@': return '2';
    case '#': return '3';
    case '$': return '4';
    case '%': return '5';
    case '^': return '6';
    case '&': return '7';
    case '*': return '8';
    case '(': return '9';
    case ')': return '0';
    case '_': return '-';
    case '+': return '=';
    case '"': return '\'';
    default: return ch;
  }
}

bool IsMenuNavigationKey(int keycode) {
  switch (keycode) {
    case XK_Up:
    case XK_Down:
    case XK_Left:
    case XK_Right:
    case XK_Prior:
    case XK_Next:
    case XK_Home:
    case XK_End:
      return true;
    default:
      return false;
  }
}

std::map<char, PunctEntry> DefaultTable() {
  std::map<char, PunctEntry> table;
  // Key             main      alternatives (first one is the Shift twin)
  table['\\'] = PunctEntry{u8"、", {u8"·", u8"＼", u8"｜"}};
  // '/' is deliberately absent: it is Rime's symbol prefix (/fh, /dn, ...)
  // and Rime's own punctuator already offers 「、」 as a candidate for it.
  // The key still works: '/' is pushed by punctuator, and '?' (Shift+/) is
  // committed directly by the entry below.
  table[';'] = PunctEntry{u8"；", {u8"：", u8"…"}};
  table[','] = PunctEntry{u8"，", {u8"《", u8"〈", u8"＜"}};
  table['.'] = PunctEntry{u8"。", {u8"》", u8"〉", u8"．"}};
  table['['] = PunctEntry{u8"「", {u8"『", u8"［"}};
  table[']'] = PunctEntry{u8"」", {u8"』", u8"］"}};
  table['<'] = PunctEntry{u8"《", {u8"〈", u8"＜"}};
  table['>'] = PunctEntry{u8"》", {u8"〉", u8"＞"}};
  table['{'] = PunctEntry{u8"『", {u8"｛"}};
  table['}'] = PunctEntry{u8"』", {u8"｝"}};
  table['|'] = PunctEntry{u8"·", {u8"｜"}};
  table['%'] = PunctEntry{u8"％", {u8"‰", u8"%"}};
  table['$'] = PunctEntry{u8"￥", {u8"＄", u8"€"}};
  table['*'] = PunctEntry{u8"×", {u8"＊", u8"·"}};
  table['~'] = PunctEntry{u8"～", {u8"〜", u8"~"}};
  table['!'] = PunctEntry{u8"！", {u8"¡", u8"!"}};
  table['?'] = PunctEntry{u8"？", {u8"?"}};
  table[':'] = PunctEntry{u8"：", {u8"∶", u8"︰"}};
  return table;
}

}  // namespace

PunctProcessor::PunctProcessor(const Ticket& ticket) : Processor(ticket) {
  table_ = DefaultTable();
  LoadConfig();
}

void PunctProcessor::LoadConfig() {
  Schema* schema = engine_ ? engine_->schema() : nullptr;
  Config* config = schema ? schema->config() : nullptr;
  if (!config)
    return;
  bool shift_alternatives = true;
  config->GetBool("rimesync_punct/shift_alternatives", &shift_alternatives);
  shift_alternatives_ = shift_alternatives;

  an<ConfigMap> keys = config->GetMap("rimesync_punct/keys");
  if (!keys)
    return;
  for (auto it = keys->begin(); it != keys->end(); ++it) {
    const string& key = it->first;
    if (key.size() != 1)
      continue;
    std::vector<string> texts;
    if (an<ConfigValue> value = As<ConfigValue>(it->second)) {
      texts.push_back(value->str());
    } else if (an<ConfigList> list = As<ConfigList>(it->second)) {
      for (size_t i = 0; i < list->size(); ++i) {
        if (an<ConfigValue> item = list->GetValueAt(i))
          texts.push_back(item->str());
      }
    }
    if (texts.empty())
      continue;
    PunctEntry entry;
    entry.main = texts.front();
    entry.alternatives.assign(texts.begin() + 1, texts.end());
    table_[key[0]] = entry;
  }
}

bool PunctProcessor::AltMenuActive() const {
  Context* ctx = engine_ ? engine_->context() : nullptr;
  if (!ctx || ctx->composition().empty())
    return false;
  return ctx->composition().back().HasTag(kAltTag);
}

bool PunctProcessor::ShowAlternatives(Context* ctx,
                                      char base,
                                      const PunctEntry& entry) {
  if (entry.alternatives.empty())
    return false;
  auto translation = New<FifoTranslation>();
  for (const string& text : entry.alternatives) {
    translation->Append(
        New<SimpleCandidate>(kAltTag, 0, 1, text));
  }
  an<Menu> menu = New<Menu>();
  menu->AddTranslation(translation);
  menu->Prepare(entry.alternatives.size());

  // set_input() rebuilds the composition through the engine, so install the
  // hand-made segment afterwards: it only lives until the next composition
  // update, which is exactly the lifetime of the alternatives menu.
  ctx->set_input(string(1, base));
  Composition composition;
  Segment segment(0, 1);
  segment.status = Segment::kGuess;
  segment.tags.insert(kAltTag);
  segment.menu = menu;
  composition.push_back(segment);
  ctx->set_composition(std::move(composition));
  return true;
}

bool PunctProcessor::CommitAlternative(size_t index) {
  Context* ctx = engine_ ? engine_->context() : nullptr;
  if (!ctx || ctx->composition().empty())
    return false;
  an<Candidate> candidate = ctx->composition().back().GetCandidateAt(index);
  if (!candidate)
    return false;
  const string text = candidate->text();
  ctx->Clear();
  engine_->CommitText(text);
  return true;
}

ProcessResult PunctProcessor::HandleAltMenuKey(const KeyEvent& key_event) {
  Context* ctx = engine_->context();
  const int keycode = key_event.keycode();
  if (keycode == XK_Escape) {
    ctx->Clear();
    return kAccepted;
  }
  if (keycode == XK_space || keycode == XK_Return || keycode == XK_KP_Enter) {
    size_t index = 0;
    if (!ctx->composition().empty())
      index = ctx->composition().back().selected_index;
    CommitAlternative(index);
    return kAccepted;
  }
  if (keycode >= '1' && keycode <= '9') {
    CommitAlternative(static_cast<size_t>(keycode - '1'));
    return kAccepted;
  }
  if (keycode == '0') {
    CommitAlternative(9);
    return kAccepted;
  }
  if (IsMenuNavigationKey(keycode)) {
    // Leave paging and highlighting to selector/navigator.
    return kNoop;
  }
  // Any other key cancels the menu and keeps its own meaning.
  ctx->Clear();
  return kNoop;
}

ProcessResult PunctProcessor::ProcessKeyEvent(const KeyEvent& key_event) {
  if (key_event.release() || key_event.ctrl() || key_event.alt() ||
      key_event.super())
    return kNoop;
  Context* ctx = engine_ ? engine_->context() : nullptr;
  if (!ctx)
    return kNoop;

  if (AltMenuActive())
    return HandleAltMenuKey(key_event);

  // Chinese mode only, and never while a composition is being typed.
  if (ctx->get_option("ascii_mode") || ctx->get_option("ascii_punct"))
    return kNoop;
  if (ctx->IsComposing())
    return kNoop;

  const int keycode = key_event.keycode();
  if (keycode <= 0x20 || keycode >= 0x7f)
    return kNoop;
  const char ch = static_cast<char>(keycode);

  if (key_event.shift() && shift_alternatives_) {
    const char base = BaseKeyOf(ch);
    auto it = table_.find(base);
    if (it != table_.end() && ShowAlternatives(ctx, base, it->second))
      return kAccepted;
  }

  auto it = table_.find(ch);
  if (it == table_.end())
    return kNoop;
  engine_->CommitText(it->second.main);
  return kAccepted;
}

}  // namespace rimesync
