// rime-vmode-smoke.cc -- 用真正部署过的 data 方案验证 v 模式与数字小键盘。
//
// 与 rime-smoke.cc 的分工：那个证明「装好的数据能部署、能出候选」；这个证明
// data 方案补丁在引擎里真的生效：
//   * v 1 2 3 4 + a  -> 壹仟贰佰叁拾肆元整，且上屏文本后面不能多出一个 a
//   * v KP_1..KP_4 + a -> 同上（小键盘被 numpad 归一化成 ASCII 数字）
//   * v KP_1 KP_Decimal KP_5 + space -> 壹元伍角整（KP_Decimal -> 句点）
//   * 0.6.19：KP_Decimal 在无组字时永远上屏 ASCII 句点 0x2E（中文态也不是「。」），
//     英文态与主键盘 '.' 一样交给客户端；正在组字时仍作为 '.' 交回引擎；
//     主键盘 '.' 中文态保持「。」。
//   * 0.6.20（punctuator 深合并）：中文态 `\\` 直接上屏「、」且不弹候选列表、
//     Shift+; 直接上屏「：」；`,` `;` `'` `[` `]` 行为不变、`/fh` 仍可用；
//     英文态 `\\` 与 `;` 仍交给客户端（引擎不提交任何文本）。
//     期望值来自 third_party/rime-data/rime-prelude/symbols.yaml。
//
// 用法：rime-vmode-smoke <shared_data_dir> <user_data_dir> [--no-deploy]
//      --no-deploy 直接用 user_data_dir/build 里已有的产物。
// 退出码：0 通过 / 2 没有 API / 3 建不出会话 / 4 v 模式错 / 5 小键盘错

#include <rime_api.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

int g_deploy = 0;
int g_failures = 0;

void OnNotification(void*, RimeSessionId, const char* type, const char* value) {
  if (type && value && std::strcmp(type, "deploy") == 0) {
    std::printf("[deploy] %s\n", value);
    std::fflush(stdout);
    if (std::strcmp(value, "success") == 0) g_deploy = 1;
    if (std::strcmp(value, "failure") == 0) g_deploy = -1;
  }
}

void Check(bool condition, const char* what, const std::string& detail = "") {
  if (condition) {
    std::printf("ok   %s\n", what);
    return;
  }
  std::printf("FAIL %s%s%s\n", what, detail.empty() ? "" : " -> ",
              detail.c_str());
  ++g_failures;
}

// 取走并清空当前会话的提交文本。
std::string TakeCommit(RimeApi* api, RimeSessionId session) {
  std::string text;
  RIME_STRUCT(RimeCommit, commit);
  if (api->get_commit(session, &commit)) {
    if (commit.text)
      text += commit.text;
    api->free_commit(&commit);
  }
  return text;
}

std::string Input(RimeApi* api, RimeSessionId session) {
  const char* input = api->get_input(session);
  return input ? input : "";
}

// 把上屏文本渲染成十六进制，便于在日志里看清是 '.'（2E）还是「。」（E3 80 82）。
std::string BytesOf(const std::string& text) {
  char buf[8];
  std::string out;
  for (unsigned char c : text) {
    std::snprintf(buf, sizeof(buf), "%02X ", c);
    out += buf;
  }
  if (!out.empty())
    out.pop_back();
  return "[" + out + "]";
}

void CheckText(const std::string& actual,
               const char* expected,
               const char* what) {
  Check(actual == expected, what,
        actual == expected ? std::string()
                           : (actual + " " + BytesOf(actual)));
}

// 当前会话有没有候选列表（用来断言标点直出而不是弹菜单）。
bool HasCandidates(RimeApi* api, RimeSessionId session) {
  RimeCandidateListIterator iterator{};  // 这个结构没有 data_size
  if (!api->candidate_list_begin(session, &iterator))
    return false;
  const bool has = api->candidate_list_next(&iterator);
  api->candidate_list_end(&iterator);
  return has;
}

// 取当前会话的候选列表文本（Shift 备选菜单靠它断言）。
// librime 的迭代器语义（rime_api_impl.h:373-410）：begin 把 index 置为 -1，
// 每次 next 前进一格并返回该格的候选，所以「begin 之后先 next 一次」拿到的
// 就是第一个候选。
std::vector<std::string> Candidates(RimeApi* api, RimeSessionId session) {
  std::vector<std::string> out;
  RimeCandidateListIterator iterator{};
  if (!api->candidate_list_begin(session, &iterator))
    return out;
  while (api->candidate_list_next(&iterator)) {
    if (iterator.candidate.text)
      out.push_back(iterator.candidate.text);
  }
  api->candidate_list_end(&iterator);
  return out;
}

// 带修饰键的按键。mask 用 librime 自己的 kShiftMask（key_table.h:15）。
// 返回引擎是否接管了这个键。
Bool PressWithMask(RimeApi* api, RimeSessionId session, int keycode,
                    int mask) {
  return api->process_key(session, keycode, mask);
}

void Press(RimeApi* api, RimeSessionId session, int keycode) {
  api->process_key(session, keycode, 0);
}

void PressAll(RimeApi* api, RimeSessionId session, const int* keys, size_t n) {
  for (size_t i = 0; i < n; ++i)
    Press(api, session, keys[i]);
}

// 清空会话。不用 Escape：express_editor 的 Escape 是「回退上一段」，
// 在没有候选的输入（例如 v12.）上不保证清干净。
void Reset(RimeApi* api, RimeSessionId session) {
  api->clear_composition(session);
  TakeCommit(api, session);
}

const int kMainDigits[] = {'1', '2', '3', '4'};
// Weasel 与 X11 用同一套 keysym：KP_0..KP_9 = 0xffb0..0xffb9, KP_Decimal = 0xffae
const int kNumpadDigits[] = {0xffb1, 0xffb2, 0xffb3, 0xffb4};

}  // namespace

int main(int argc, char** argv) {
  const char* shared = argc > 1 ? argv[1] : "/usr/share/rime-data";
  const char* user = argc > 2 ? argv[2] : "/tmp/rime-vmode-user";
  // --no-deploy 可以出现在任何位置；方案名是第一个非开关参数。
  bool no_deploy = false;
  for (int i = 1; i < argc; ++i) {
    if (std::strcmp(argv[i], "--no-deploy") == 0)
      no_deploy = true;
  }
  // 方案名可覆盖：容器里跑的是 luna_pinyin_simp（rime-sync 的保留方案，
  // 用户目录里摆 packaging/rime/schemas/luna_pinyin_simp.custom.yaml 才有
  // rimesync_vnum / rimesync_punct / rimesync_vselect / numpad 这几个处理器）。
  const char* schema = argc > 3 ? argv[3] : "luna_pinyin_simp";

  RimeApi* api = rime_get_api();
  if (!api) {
    std::printf("FAIL: rime_get_api() returned NULL\n");
    return 2;
  }
  std::printf("librime %s, shared=%s, user=%s\n", api->get_version(), shared,
              user);

  RIME_STRUCT(RimeTraits, traits);
  traits.shared_data_dir = shared;
  traits.user_data_dir = user;
  traits.distribution_name = "Rime";
  traits.distribution_code_name = "rime-sync-vmode";
  traits.distribution_version = "1.0";
  traits.app_name = "rime.vmode";
  api->setup(&traits);
  api->set_notification_handler(OnNotification, nullptr);
  api->initialize(&traits);

  if (no_deploy) {
    std::printf("deploy: skipped (--no-deploy)\n");
  } else {
    api->start_maintenance(True);
    api->join_maintenance_thread();
    std::printf("deploy: %s\n",
                g_deploy == 1 ? "success"
                              : (g_deploy == -1 ? "failure" : "not-reported"));
    if (g_deploy == -1) {
      api->finalize();
      return 3;
    }
  }

  RimeSessionId session = api->create_session();
  if (!session) {
    std::printf("FAIL: create_session()\n");
    api->finalize();
    return 3;
  }
  if (!api->select_schema(session, schema)) {
    std::printf("FAIL: select_schema(%s)\n", schema);
    api->destroy_session(session);
    api->finalize();
    return 3;
  }

  // 1. 主键盘：v 1 2 3 4 + a
  Reset(api, session);
  Press(api, session, 'v');
  PressAll(api, session, kMainDigits, 4);
  Check(Input(api, session) == "v1234", "main keyboard: input is v1234",
        Input(api, session));
  Press(api, session, 'a');
  const std::string main_commit = TakeCommit(api, session);
  Check(main_commit == u8"壹仟贰佰叁拾肆元整",
        "main keyboard: a commits the upper amount", main_commit);
  Check(main_commit.find('a') == std::string::npos,
        "main keyboard: no trailing a", main_commit);
  Check(Input(api, session).empty(),
        "main keyboard: the key was consumed, the input is empty",
        Input(api, session));

  // 2. 小键盘：v KP_1..KP_4 + a
  Reset(api, session);
  Press(api, session, 'v');
  PressAll(api, session, kNumpadDigits, 4);
  Check(Input(api, session) == "v1234", "numpad: KP_1..KP_4 becomes 1234",
        Input(api, session));
  Press(api, session, 'a');
  const std::string numpad_commit = TakeCommit(api, session);
  Check(numpad_commit == u8"壹仟贰佰叁拾肆元整",
        "numpad: a commits the upper amount", numpad_commit);
  Check(numpad_commit.find('a') == std::string::npos,
        "numpad: no trailing a", numpad_commit);
  Check(numpad_commit == main_commit, "numpad and main keyboard agree",
        numpad_commit);
  Check(Input(api, session).empty(), "numpad: the input is empty",
        Input(api, session));

  // 3. KP_Decimal 当作句点：v 1 KP_Decimal 5 + space
  Reset(api, session);
  Press(api, session, 'v');
  Press(api, session, 0xffb1);  // KP_1
  Press(api, session, 0xffae);  // KP_Decimal
  Press(api, session, 0xffb5);  // KP_5
  Check(Input(api, session) == "v1.5", "numpad: KP_Decimal becomes a period",
        Input(api, session));
  Press(api, session, 0x20);  // space
  const std::string decimal_commit = TakeCommit(api, session);
  Check(decimal_commit == u8"壹元伍角整",
        "numpad: decimal amount commits", decimal_commit);

  // 4. 普通拼音不受影响（numpad 对所有输入生效，但不能破坏字母输入）
  Reset(api, session);
  Press(api, session, 'n');
  Press(api, session, 'i');
  Check(Input(api, session) == "ni", "pinyin: letters still work",
        Input(api, session));
  Reset(api, session);

  // 4b. 0.6.20：中文态两个符号直出（data 方案的 punctuator/half_shape 深合并）
  api->set_option(session, "ascii_mode", False);
  Reset(api, session);
  Press(api, session, '\\');
  const std::string backslash_commit = TakeCommit(api, session);
  CheckText(backslash_commit, u8"、",
            "chinese: backslash commits 、 directly");
  Check(!HasCandidates(api, session),
        "chinese: backslash shows no candidate menu");
  Check(Input(api, session).empty(),
        "chinese: backslash leaves no composition", Input(api, session));

  Reset(api, session);
  Press(api, session, ':');
  const std::string colon_commit = TakeCommit(api, session);
  CheckText(colon_commit, u8"：", "chinese: Shift+; commits ： directly");
  Check(!HasCandidates(api, session),
        "chinese: colon shows no candidate menu");
  Check(Input(api, session).empty(), "chinese: colon leaves no composition",
        Input(api, session));

  // 4c. 其余标点保持原样（期望值来自
  //     third_party/rime-data/rime-prelude/symbols.yaml 的 half_shape）
  Reset(api, session);
  Press(api, session, ',');
  CheckText(TakeCommit(api, session), u8"，", "chinese: ',' still commits ，");

  Reset(api, session);
  Press(api, session, ';');
  CheckText(TakeCommit(api, session), u8"；", "chinese: ';' still commits ；");

  Reset(api, session);
  Press(api, session, '\'');
  CheckText(TakeCommit(api, session), u8"‘",
            "chinese: single quote still commits the opening quote");

  Reset(api, session);
  Press(api, session, '[');
  Press(api, session, 0x20);
  CheckText(TakeCommit(api, session), u8"「",
            "chinese: '[' still commits its first candidate");

  Reset(api, session);
  Press(api, session, ']');
  Press(api, session, 0x20);
  CheckText(TakeCommit(api, session), u8"」",
            "chinese: ']' still commits its first candidate");

  // /fh 符号表（recognizer punct + symbols preset）仍然可用
  Reset(api, session);
  Press(api, session, '/');
  Press(api, session, 'f');
  Press(api, session, 'h');
  Check(Input(api, session) == "/fh", "chinese: /fh types the symbol prefix",
        Input(api, session));
  Press(api, session, 0x20);
  CheckText(TakeCommit(api, session), u8"©",
            "chinese: /fh still commits its first symbol");

  // 5. 0.6.19：中文态、没有组字时，KP_Decimal 直接上屏 ASCII 句点。
  api->set_option(session, "ascii_mode", False);
  Reset(api, session);
  Press(api, session, 0xffae);
  const std::string kp_period = TakeCommit(api, session);
  CheckText(kp_period, ".", "chinese: KP_Decimal commits an ASCII period");
  Check(kp_period.size() == 1 &&
            static_cast<unsigned char>(kp_period[0]) == 0x2e,
        "chinese: the commit is exactly one byte 0x2E", BytesOf(kp_period));
  Check(kp_period != u8"。", "chinese: KP_Decimal does not commit 。",
        BytesOf(kp_period));
  Check(Input(api, session).empty(),
        "chinese: KP_Decimal leaves no composition", Input(api, session));

  // 6. 英文/ASCII 态：引擎把键交给客户端（与主键盘 '.' 完全一致），由客户端
  //    插入字符；IME 既不提交中文标点，也不留下任何组合文本。
  api->set_option(session, "ascii_mode", True);
  const Bool kp_handled = api->process_key(session, 0xffae, 0);
  const std::string kp_ascii_commit = TakeCommit(api, session);
  Check(!kp_handled, "ascii: KP_Decimal is passed to the client",
        std::string("process_key=") + (kp_handled ? "true" : "false"));
  Check(kp_ascii_commit.empty(), "ascii: KP_Decimal commits no IME text",
        BytesOf(kp_ascii_commit));
  const Bool dot_handled = api->process_key(session, '.', 0);
  const std::string dot_ascii_commit = TakeCommit(api, session);
  Check(dot_handled == kp_handled,
        "ascii: KP_Decimal matches the main keyboard '.'",
        std::string("KP=") + (kp_handled ? "true" : "false") + " main=" +
            (dot_handled ? "true" : "false"));
  Check(dot_ascii_commit.empty(),
        "ascii: main '.' commits no IME text either",
        BytesOf(dot_ascii_commit));

  // 6b. 英文态：\\ 与 ; 仍交给客户端（引擎不提交任何文本）
  Reset(api, session);
  const Bool backslash_ascii_handled = api->process_key(session, '\\', 0);
  Check(!backslash_ascii_handled, "ascii: backslash is passed to the client",
        std::string("process_key=") +
            (backslash_ascii_handled ? "true" : "false"));
  Check(TakeCommit(api, session).empty(),
        "ascii: backslash commits no IME text");
  const Bool semicolon_ascii_handled = api->process_key(session, ';', 0);
  Check(!semicolon_ascii_handled, "ascii: ';' is passed to the client",
        std::string("process_key=") +
            (semicolon_ascii_handled ? "true" : "false"));
  Check(TakeCommit(api, session).empty(), "ascii: ';' commits no IME text");
  api->set_option(session, "ascii_mode", False);

  // 7. v 模式：v12.34 + a —— 小数点参与编码，不被上屏。
  Reset(api, session);
  Press(api, session, 'v');
  Press(api, session, 0xffb1);
  Press(api, session, 0xffb2);
  Press(api, session, 0xffae);
  Press(api, session, 0xffb3);
  Press(api, session, 0xffb4);
  Check(Input(api, session) == "v12.34",
        "v-mode: KP_Decimal joins the vnum encoding", Input(api, session));
  Press(api, session, 'a');
  const std::string v1234_commit = TakeCommit(api, session);
  CheckText(v1234_commit, u8"壹拾贰元叁角肆分",
            "v-mode: v12.34 + a commits the amount");
  Check(v1234_commit.find('.') == std::string::npos &&
            v1234_commit.find(u8"。") == std::string::npos,
        "v-mode: the period itself is not committed", v1234_commit);
  Check(Input(api, session).empty(), "v-mode: input is empty after a",
        Input(api, session));

  // 8. v 模式边界：v12. + a —— 现有转换规则拒绝空小数（小数点后必须至少
  //    一位数字），所以不产生金额，也不崩。
  Reset(api, session);
  Press(api, session, 'v');
  Press(api, session, 0xffb1);
  Press(api, session, 0xffb2);
  Press(api, session, 0xffae);
  Check(Input(api, session) == "v12.",
        "v-mode boundary: trailing period stays in the input",
        Input(api, session));
  api->process_key(session, 'a', 0);
  const std::string boundary_commit = TakeCommit(api, session);
  Check(boundary_commit.empty(),
        "v-mode boundary: no amount is committed for v12.",
        BytesOf(boundary_commit));
  Check(!Input(api, session).empty(),
        "v-mode boundary: the composition is still alive",
        Input(api, session));
  std::printf("     v12. + a: input=[%s] commit=%s\n",
              Input(api, session).c_str(), BytesOf(boundary_commit).c_str());

  // 9. 普通拼音组字中按 KP_Decimal：与主键盘 '.' 完全一致，不清空组字。
  Reset(api, session);
  Press(api, session, 'n');
  Press(api, session, 'i');
  Check(Input(api, session) == "ni", "pinyin: input is ni",
        Input(api, session));
  const Bool kp_composing = api->process_key(session, 0xffae, 0);
  const std::string kp_pinyin_input = Input(api, session);
  const std::string kp_pinyin_commit = TakeCommit(api, session);
  Check(kp_composing, "pinyin: KP_Decimal is accepted while composing");
  Check(!(kp_pinyin_input.empty() && kp_pinyin_commit.empty()),
        "pinyin: KP_Decimal is not silently swallowed",
        "input=" + kp_pinyin_input + " commit=" + kp_pinyin_commit);
  Reset(api, session);
  Press(api, session, 'n');
  Press(api, session, 'i');
  const Bool dot_composing = api->process_key(session, '.', 0);
  const std::string dot_pinyin_input = Input(api, session);
  const std::string dot_pinyin_commit = TakeCommit(api, session);
  Check(dot_composing == kp_composing,
        "pinyin: KP_Decimal matches main '.' handling");
  Check(dot_pinyin_input == kp_pinyin_input,
        "pinyin: KP_Decimal matches main '.' input",
        "KP=" + kp_pinyin_input + " main=" + dot_pinyin_input);
  Check(dot_pinyin_commit == kp_pinyin_commit,
        "pinyin: KP_Decimal matches main '.' commit",
        "KP=" + kp_pinyin_commit + " main=" + dot_pinyin_commit);
  std::printf("     pinyin 'ni' + KP_Decimal: input=[%s] commit=[%s]\n",
              kp_pinyin_input.c_str(), kp_pinyin_commit.c_str());

  // 10. 防回归：主键盘 '.' 在中文态仍然是「。」。
  Reset(api, session);
  Press(api, session, '.');
  const std::string dot_commit = TakeCommit(api, session);
  CheckText(dot_commit, u8"。", "chinese: main '.' still commits 。");
  Check(dot_commit != ".", "chinese: main '.' is not the numpad rule",
        BytesOf(dot_commit));

  // 11. Shift 备选菜单（rimesync_punct 的 alternatives）。
  //     语义见 third_party/librime/plugins/rimesync-extra/punct_processor.cc：
  //     表按「字符」索引；Shift 时给出「这个键所属按键的备选」——菜单 = 该条目
  //     的 alternatives，**含 Shift 孪生（第一个）、不含本键的 main 符号**。
  //     librime 送来的 keycode 已经是移位后的 keysym（X11 约定，注释见
  //     punct_processor.cc 头部），所以 Shift+\\ 的 keycode 是 '|' (0x7C)，
  //     不是 '\\' (0x5C)；BaseKeyOf('|') = '\\' 才查得到表里那一项。
  //     '/' 故意不在表里，所以 Shift+/ 不弹菜单、直接上屏「？」。
  const int kShiftMask = 1 << 0;  // librime/src/rime/key_table.h:15
  struct ShiftCase {
    int keycode;
    const char* name;
    const char* base;  // 本键的 main 符号，必须不在菜单里
    std::vector<const char*> expected;
  };
  const std::vector<ShiftCase> kShiftCases = {
      {'|', "Shift+backslash", u8"、", {u8"·", u8"＼", u8"｜"}},
      {':', "Shift+semicolon", u8"；", {u8"：", u8"…"}},
      {'<', "Shift+comma", u8"，", {u8"《", u8"〈", u8"＜"}},
      {'>', "Shift+period", u8"。", {u8"》", u8"〉", u8"．"}},
      {'{', "Shift+lbracket", u8"「", {u8"『", u8"［"}},
  };
  for (const ShiftCase& c : kShiftCases) {
    Reset(api, session);
    const Bool handled = PressWithMask(api, session, c.keycode, kShiftMask) != 0;
    const std::vector<std::string> menu = Candidates(api, session);
    std::string joined;
    for (size_t i = 0; i < menu.size(); ++i) {
      if (i)
        joined += ",";
      joined += menu[i];
    }
    Check(handled, "shift alternatives: the key is intercepted",
          std::string("process_key=") + (handled ? "true" : "false"));
    Check(menu.size() == c.expected.size(),
          "shift alternatives: the menu has the expected size", joined);
    bool content_ok = menu.size() == c.expected.size();
    for (size_t i = 0; content_ok && i < menu.size(); ++i) {
      if (menu[i] != c.expected[i])
        content_ok = false;
    }
    Check(content_ok, "shift alternatives: the menu content matches", joined);
    bool has_base = false;
    for (const std::string& item : menu) {
      if (item == c.base)
        has_base = true;
    }
    Check(!has_base, "shift alternatives: the menu excludes the base symbol",
          joined + " base=" + c.base);
    Check(!menu.empty() && menu[0] == c.expected[0],
          "shift alternatives: the first item is the Shift twin", joined);
    std::printf("     %s: menu=[%s]\n", c.name, joined.c_str());
  }

  api->destroy_session(session);
  api->finalize();

  if (g_failures != 0) {
    std::printf("\n%d check(s) failed\n", g_failures);
    return 4;
  }
  std::printf("\nPASS: v-mode and numpad behave as expected\n");
  return 0;
}
