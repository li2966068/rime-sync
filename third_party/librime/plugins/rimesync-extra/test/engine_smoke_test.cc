// Runtime smoke test for the rimesync-extra components against a real
// librime build.  Not part of the default build; compile it by hand:
//
//   g++ -std=c++17 -I<librime>/src -I<librime>/include -I<build>/src \
//       test/engine_smoke_test.cc -L<build>/lib -lrime -o /tmp/smoke
//   LD_LIBRARY_PATH=<build>/lib /tmp/smoke
//
// Part 1 drives the components directly, part 2 applies a small in-memory
// schema to a real Engine and presses keys through the whole processor chain
// (that is the part that proves the schema wiring: /fh symbols, v-mode).

#include <cstdio>
#include <memory>
#include <sstream>
#include <string>

#include <rime/candidate.h>
#include <rime/common.h>
#include <rime/component.h>
#include <rime/config.h>
#include <rime/context.h>
#include <rime/engine.h>
#include <rime/key_event.h>
#include <rime/key_table.h>
#include <rime/menu.h>
#include <rime/processor.h>
#include <rime/schema.h>
#include <rime/segmentation.h>
#include <rime/setup.h>
#include <rime/ticket.h>
#include <rime/translation.h>
#include <rime/translator.h>

using namespace rime;

namespace {

int g_failures = 0;
int g_checks = 0;

void Check(bool condition, const char* what) {
  ++g_checks;
  if (condition)
    return;
  std::printf("FAIL %s\n", what);
  ++g_failures;
}

void CheckEq(const std::string& actual,
             const std::string& expected,
             const char* what) {
  ++g_checks;
  if (actual == expected)
    return;
  std::printf("FAIL %s\n  got:  [%s]\n  want: [%s]\n", what, actual.c_str(),
              expected.c_str());
  ++g_failures;
}

std::string CandidateText(Context* ctx, size_t index) {
  if (!ctx || ctx->composition().empty())
    return std::string();
  an<Candidate> candidate = ctx->composition().back().GetCandidateAt(index);
  return candidate ? candidate->text() : std::string();
}

void TestTranslator() {
  auto* component = Translator::Require("rimesync_vnum");
  Check(component != nullptr, "translator rimesync_vnum is registered");
  if (!component)
    return;
  std::unique_ptr<Translator> translator(component->Create(Ticket()));
  Check(translator.get() != nullptr, "translator can be created");

  {
    auto translation = translator->Query("v1234.5", Segment(0, 7));
    Check(translation != nullptr, "v1234.5 yields candidates");
    if (translation) {
      CheckEq(translation->Peek()->text(), u8"壹仟贰佰叁拾肆元伍角整",
              "upper candidate of v1234.5");
      translation->Next();
      Check(translation->Peek() != nullptr, "second candidate exists");
      if (translation->Peek())
        CheckEq(translation->Peek()->text(), u8"一千二百三十四点五",
                "lower candidate of v1234.5");
    }
  }
  {
    auto translation = translator->Query("v0.05", Segment(0, 5));
    Check(translation != nullptr, "v0.05 yields candidates");
    if (translation)
      CheckEq(translation->Peek()->text(), u8"零元零伍分", "upper of v0.05");
  }
  Check(!translator->Query("v1.234", Segment(0, 6)),
        "three fraction digits rejected");
  Check(!translator->Query("v1234567890123456789", Segment(0, 20)),
        "19 integer digits rejected");
  Check(!translator->Query("hello", Segment(0, 5)), "letters rejected");
  Check(!translator->Query("1234", Segment(0, 4)), "missing v rejected");
}

void TestProcessor() {
  auto* component = Processor::Require("rimesync_punct");
  Check(component != nullptr, "processor rimesync_punct is registered");
  if (!component)
    return;
  std::unique_ptr<Engine> engine(Engine::Create());
  Check(engine.get() != nullptr, "engine can be created");
  if (!engine)
    return;
  std::unique_ptr<Processor> processor(component->Create(Ticket(engine.get())));
  Context* ctx = engine->context();
  ctx->set_option("ascii_mode", false);

  std::string committed;
  engine->sink().connect([&committed](const std::string& text) {
    committed += text;
  });

  // Direct commit, no composition, no trailing space.
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_backslash, 0)) == kAccepted,
        "backslash accepted");
  CheckEq(committed, u8"、", "backslash commits 、 immediately");
  Check(!ctx->IsComposing(), "backslash leaves no composition");

  // '/' is handed back to Rime's own punctuator (symbol prefix /fh).
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_slash, 0)) == kNoop,
        "slash is not intercepted any more");
  Check(committed.empty(), "slash commits nothing here");
  Check(!ctx->IsComposing(), "slash leaves no composition behind");

  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_period, 0)) == kAccepted,
        "period accepted");
  CheckEq(committed, u8"。", "period commits 。");

  // Shift+4 is '$' with the Shift bit on X11; its base key is not
  // intercepted, so the main symbol is committed directly.
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent('$', kShiftMask)) == kAccepted,
        "shift+4 accepted");
  CheckEq(committed, u8"￥", "shift+4 commits ￥");

  // '?' lost its base key ('/') from the table and is committed directly.
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent('?', kShiftMask)) == kAccepted,
        "shift+slash accepted");
  CheckEq(committed, u8"？", "shift+slash commits ？ directly");

  // Shift+, is '<' with the Shift bit; the ',' alternatives menu opens.
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_less, kShiftMask)) == kAccepted,
        "shift+comma accepted");
  Check(ctx->HasMenu(), "shift+comma opens the alternatives menu");
  CheckEq(CandidateText(ctx, 0), u8"《", "first alternative of comma is 《");
  Check(processor->ProcessKeyEvent(KeyEvent('1', 0)) == kAccepted,
        "digit selects a candidate");
  CheckEq(committed, u8"《", "selecting the first alternative commits 《");
  Check(!ctx->IsComposing(), "menu is gone after selection");

  // Escape cancels the menu without committing.
  committed.clear();
  processor->ProcessKeyEvent(KeyEvent(XK_less, kShiftMask));
  Check(processor->ProcessKeyEvent(KeyEvent(XK_Escape, 0)) == kAccepted,
        "escape accepted while the menu is open");
  Check(!ctx->IsComposing() && committed.empty(), "escape cancels the menu");

  // Letters are never intercepted, and digits without a vnum menu are not
  // ours either.
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
        "letters are left alone");
  Check(committed.empty(), "letters commit nothing");
  Check(processor->ProcessKeyEvent(KeyEvent('1', 0)) == kNoop,
        "digits without a vnum menu are left alone");

  // English mode.
  ctx->set_option("ascii_mode", true);
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_backslash, 0)) == kNoop,
        "ascii mode is left alone");
  Check(committed.empty(), "ascii mode commits nothing");
  ctx->set_option("ascii_mode", false);

  // An active composition is left alone too.
  ctx->set_input("ni");
  committed.clear();
  Check(processor->ProcessKeyEvent(KeyEvent(XK_comma, 0)) == kNoop,
        "composition is left alone");
  ctx->Clear();
}

// A small schema equivalent to the wiring documented in PLUGINS.md.
const char* const kSchemaYaml = R"YAML(
schema:
  schema_id: rimesync_smoke
  name: rimesync smoke
  version: "1"
engine:
  processors:
    - ascii_composer
    - rimesync_punct
    - rimesync_vselect
    - recognizer
    - key_binder
    - speller
    - punctuator
    - selector
    - express_editor
  segmentors:
    - matcher
    - punct_segmentor
    - fallback_segmentor
  translators:
    - punct_translator
    - rimesync_vnum
menu:
  page_size: 5
speller:
  alphabet: "zyxwvutsrqponmlkjihgfedcba"
recognizer:
  patterns:
    punct: '^/([0-9]0?|[A-Za-z]+)$'
    vnum: '^v[0-9]{1,18}(\.[0-9]{0,2})?$'
punctuator:
  half_shape:
    '/': [ '、', '/', '／', '÷' ]
    '\': [ '、', '＼' ]
  symbols:
    '/fh': [ '©', '®' ]
)YAML";

// Builds an engine from kSchemaYaml plus an optional extra config block.
// The engine owns the schema, so the caller only deletes the engine.
Engine* CreateSchemaEngine(const std::string& extra_config, Context** out_ctx) {
  the<Config> config(new Config());
  std::istringstream yaml(std::string(kSchemaYaml) + extra_config);
  Check(config->LoadFromStream(yaml), "smoke schema yaml parses");
  Engine* engine = Engine::Create();
  Check(engine != nullptr, "schema engine can be created");
  if (!engine)
    return nullptr;
  engine->ApplySchema(new Schema("rimesync_smoke", config.release()));
  engine->context()->set_option("ascii_mode", false);
  if (out_ctx)
    *out_ctx = engine->context();
  return engine;
}

void TypeString(Engine* engine, const char* keys, int modifier = 0) {
  for (const char* p = keys; *p; ++p)
    engine->ProcessKey(KeyEvent(*p, modifier));
}

void TestSchemaSession() {
  Context* ctx = nullptr;
  the<Engine> engine(CreateSchemaEngine("", &ctx));
  if (!engine)
    return;
  std::string committed;
  engine->sink().connect([&committed](const std::string& text) {
    committed += text;
  });

  // 1. '/' keeps Rime's behaviour: a 「、」 candidate from punctuation.yaml.
  committed.clear();
  Check(engine->ProcessKey(KeyEvent(XK_slash, 0)), "slash accepted by engine");
  CheckEq(ctx->input(), "/", "slash entered the input");
  Check(ctx->HasMenu(), "slash opens Rime's punct candidate menu");
  CheckEq(CandidateText(ctx, 0), u8"、", "Rime's 、 candidate is intact");
  Check(committed.empty(), "slash does not commit by itself");

  // 2. '/fh' reaches the symbols table instead of being eaten.
  Check(engine->ProcessKey(KeyEvent('f', 0)), "f accepted");
  Check(engine->ProcessKey(KeyEvent('h', 0)), "h accepted");
  CheckEq(ctx->input(), "/fh", "input became /fh");
  Check(ctx->HasMenu(), "/fh opens the symbols menu");
  CheckEq(CandidateText(ctx, 0), u8"©", "/fh first symbol is ©");
  committed.clear();
  Check(engine->ProcessKey(KeyEvent('1', 0)), "1 selects a symbol");
  CheckEq(committed, u8"©", "/fh -> © committed");
  Check(!ctx->IsComposing(), "symbol composition is finished");

  // 3. '\' still commits 、 immediately through the whole chain.
  committed.clear();
  Check(engine->ProcessKey(KeyEvent(XK_backslash, 0)), "backslash accepted");
  CheckEq(committed, u8"、", "backslash still commits 、 immediately");
  Check(!ctx->IsComposing(), "backslash leaves no composition");

  // 4. Shift+, still opens the 《 alternatives menu.
  committed.clear();
  Check(engine->ProcessKey(KeyEvent(XK_less, kShiftMask)),
        "shift+comma accepted by engine");
  Check(ctx->HasMenu(), "shift+comma menu still opens");
  CheckEq(CandidateText(ctx, 0), u8"《", "first alternative is still 《");
  Check(engine->ProcessKey(KeyEvent('1', 0)), "1 selects 《");
  CheckEq(committed, u8"《", "shift+comma -> 《 committed");

  // 5. v-mode through the schema: v 1 2 3 4 . 5 + space.
  //    (Number keys cannot select here: the recognizer processor matches
  //    input + digit and keeps extending the number, which is also what
  //    upstream Rime does with a digit-bearing pattern.  Space/Enter confirm.)
  committed.clear();
  const char* keys = "v1234.5";
  for (const char* p = keys; *p; ++p)
    engine->ProcessKey(KeyEvent(*p, 0));
  CheckEq(ctx->input(), "v1234.5", "v1234.5 entered the input");
  Check(ctx->HasMenu(), "v1234.5 opens the vnum menu");
  CheckEq(CandidateText(ctx, 0), u8"壹仟贰佰叁拾肆元伍角整",
          "vnum upper candidate in the schema");
  CheckEq(CandidateText(ctx, 1), u8"一千二百三十四点五",
          "vnum lower candidate in the schema");
  Check(engine->ProcessKey(KeyEvent(XK_space, 0)), "space confirms the amount");
  CheckEq(committed, u8"壹仟贰佰叁拾肆元伍角整", "v-mode commits the amount");
  Check(!ctx->IsComposing(), "v-mode composition is finished");

  // 6. Second v-mode case, also confirmed with space.
  committed.clear();
  const char* cents = "v0.05";
  for (const char* p = cents; *p; ++p)
    engine->ProcessKey(KeyEvent(*p, 0));
  Check(ctx->HasMenu(), "v0.05 opens the vnum menu");
  Check(engine->ProcessKey(KeyEvent(XK_space, 0)), "space confirms");
  CheckEq(committed, u8"零元零伍分", "space commits the amount");
  Check(!ctx->IsComposing(), "v0.05 composition is finished");

  // 7. rimesync_vselect: a/b/c... pick candidates while the v-mode menu is up.
  committed.clear();
  TypeString(engine.get(), "v1234");
  Check(ctx->HasMenu(), "v1234 opens the vnum menu");
  Check(engine->ProcessKey(KeyEvent('a', 0)), "a is accepted by the chain");
  CheckEq(committed, u8"壹仟贰佰叁拾肆元整", "a commits the upper amount");
  Check(!ctx->IsComposing(), "letter selection finishes the composition");

  committed.clear();
  TypeString(engine.get(), "v1234");
  Check(engine->ProcessKey(KeyEvent('b', 0)), "b is accepted by the chain");
  CheckEq(committed, u8"一千二百三十四", "b commits the lower number");
  Check(!ctx->IsComposing(), "b finishes the composition");

  // 8. Negative cases, checked on the processor itself so that no other
  //    processor can hide what rimesync_vselect does with the key.
  std::unique_ptr<Processor> vselect(
      Processor::Require("rimesync_vselect")->Create(Ticket(engine.get())));
  Check(vselect.get() != nullptr, "processor rimesync_vselect is registered");
  if (vselect) {
    TypeString(engine.get(), "v1234");
    Check(ctx->HasMenu(), "v1234 menu is up for the kNoop checks");
    Check(vselect->ProcessKeyEvent(KeyEvent('c', 0)) == kNoop,
          "out-of-range letter is kNoop");
    CheckEq(ctx->input(), "v1234", "out-of-range letter does not touch input");
    Check(vselect->ProcessKeyEvent(KeyEvent('z', 0)) == kNoop,
          "unknown letter is kNoop");
    CheckEq(ctx->input(), "v1234", "unknown letter does not touch input");
    Check(vselect->ProcessKeyEvent(KeyEvent(XK_space, 0)) == kNoop,
          "space is left to selector/express_editor");
    Check(vselect->ProcessKeyEvent(KeyEvent(XK_Escape, 0)) == kNoop,
          "escape is left alone");
    Check(vselect->ProcessKeyEvent(KeyEvent(XK_Down, 0)) == kNoop,
          "arrows are left alone");
    Check(vselect->ProcessKeyEvent(KeyEvent(XK_Next, 0)) == kNoop,
          "paging keys are left alone");
    CheckEq(ctx->input(), "v1234", "navigation keys do not touch input");

    // English mode.
    ctx->set_option("ascii_mode", true);
    Check(vselect->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
          "ascii mode is left alone");
    ctx->set_option("ascii_mode", false);

    // A menu that does not belong to v-mode ('/' punct menu).
    ctx->Clear();
    engine->ProcessKey(KeyEvent(XK_slash, 0));
    Check(ctx->HasMenu(), "slash menu is up");
    Check(vselect->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
          "punct menu does not take letters");
    CheckEq(ctx->input(), "/", "punct input untouched by vselect");

    // Plain pinyin input.
    ctx->Clear();
    TypeString(engine.get(), "nihao");
    CheckEq(ctx->input(), "nihao", "pinyin input is in the context");
    Check(vselect->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
          "pinyin + letter is kNoop");
    CheckEq(ctx->input(), "nihao", "pinyin input untouched by vselect");

    // No menu at all.
    ctx->Clear();
    Check(vselect->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
          "no menu means no letter selection");
  }
}

// rimesync_vselect/keys overrides the default "abcdefgh".
void TestVselectKeysOverride() {
  Context* ctx = nullptr;
  the<Engine> engine(
      CreateSchemaEngine("\nrimesync_vselect:\n  keys: \"xyz\"\n", &ctx));
  if (!engine)
    return;
  std::string committed;
  engine->sink().connect([&committed](const std::string& text) {
    committed += text;
  });
  TypeString(engine.get(), "v12");
  Check(ctx->HasMenu(), "v12 opens the vnum menu");
  Check(engine->ProcessKey(KeyEvent('x', 0)), "configured key x is accepted");
  CheckEq(committed, u8"壹拾贰元整", "x commits the upper amount");
  Check(!ctx->IsComposing(), "configured key finishes the composition");

  std::unique_ptr<Processor> vselect(
      Processor::Require("rimesync_vselect")->Create(Ticket(engine.get())));
  if (vselect) {
    TypeString(engine.get(), "v12");
    Check(vselect->ProcessKeyEvent(KeyEvent('a', 0)) == kNoop,
          "default key a is disabled by the override");
    CheckEq(ctx->input(), "v12", "input untouched");
  }
}

}  // namespace

int main() {
  LoadModules(kDefaultModules);
  TestTranslator();
  TestProcessor();
  TestSchemaSession();
  TestVselectKeysOverride();
  if (g_failures != 0) {
    std::printf("\n%d check(s) failed\n", g_failures);
    return 1;
  }
  std::printf("all %d rimesync-extra runtime checks passed\n", g_checks);
  return 0;
}
