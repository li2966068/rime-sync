// rime-vmode-smoke.cc -- 用真正部署过的 data 方案验证 v 模式与数字小键盘。
//
// 与 rime-smoke.cc 的分工：那个证明「装好的数据能部署、能出候选」；这个证明
// 0.6.18 的 data 方案补丁在引擎里真的生效：
//   * v 1 2 3 4 + a  -> 壹仟贰佰叁拾肆元整，且上屏文本后面不能多出一个 a
//   * v KP_1..KP_4 + a -> 同上（小键盘被 numpad 归一化成 ASCII 数字）
//   * v KP_1 KP_Decimal KP_5 + space -> 壹元伍角整（KP_Decimal -> 句点）
//
// 用法：rime-vmode-smoke <shared_data_dir> <user_data_dir> [--no-deploy]
//      --no-deploy 直接用 user_data_dir/build 里已有的产物。
// 退出码：0 通过 / 2 没有 API / 3 建不出会话 / 4 v 模式错 / 5 小键盘错

#include <rime_api.h>

#include <cstdio>
#include <cstring>
#include <string>

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

void Press(RimeApi* api, RimeSessionId session, int keycode) {
  api->process_key(session, keycode, 0);
}

void PressAll(RimeApi* api, RimeSessionId session, const int* keys, size_t n) {
  for (size_t i = 0; i < n; ++i)
    Press(api, session, keys[i]);
}

void Reset(RimeApi* api, RimeSessionId session) {
  Press(api, session, 0xff1b);  // Escape
  TakeCommit(api, session);
}

const int kMainDigits[] = {'1', '2', '3', '4'};
// Weasel 与 X11 用同一套 keysym：KP_0..KP_9 = 0xffb0..0xffb9, KP_Decimal = 0xffae
const int kNumpadDigits[] = {0xffb1, 0xffb2, 0xffb3, 0xffb4};

}  // namespace

int main(int argc, char** argv) {
  const char* shared = argc > 1 ? argv[1] : "/usr/share/rime-data";
  const char* user = argc > 2 ? argv[2] : "/tmp/rime-vmode-user";
  const bool no_deploy = argc > 3 && std::strcmp(argv[3], "--no-deploy") == 0;

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
  if (!api->select_schema(session, "luna_pinyin")) {
    std::printf("FAIL: select_schema(luna_pinyin)\n");
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

  api->destroy_session(session);
  api->finalize();

  if (g_failures != 0) {
    std::printf("\n%d check(s) failed\n", g_failures);
    return 4;
  }
  std::printf("\nPASS: v-mode and numpad behave as expected\n");
  return 0;
}
