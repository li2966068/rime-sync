// rime-smoke.cc -- headless proof that the installed rime data can deploy and
// that typing "nihao" on the luna_pinyin schema produces the 你好 candidate.
//
// Built and run inside the build container (scripts/ime/container-build.sh) and
// again inside the offline install test. Usage:
//   rime-smoke [shared_data_dir] [user_data_dir] [--no-deploy]
//
// --no-deploy 跳过 start_maintenance，只用 user_data_dir 里现有的 build/ 产物
// 建会话。0.6.15 的「预编译 build 产物」兜底就靠它验证：在没有手机器会成功部署
// 的机器上，产物自身是否够打字。
//
// Exit codes: 0 = pass, 2 = no API, 3 = no session, 4 = candidate missing.
#include <rime_api.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>

static int g_deploy = 0;  // 1 = success, -1 = failure

static void OnNotification(void*, RimeSessionId, const char* type,
                           const char* value) {
  if (type && value && std::strcmp(type, "deploy") == 0) {
    std::printf("[deploy] %s\n", value);
    std::fflush(stdout);
    if (std::strcmp(value, "success") == 0) g_deploy = 1;
    if (std::strcmp(value, "failure") == 0) g_deploy = -1;
  }
}

int main(int argc, char** argv) {
  const char* shared = argc > 1 ? argv[1] : "/usr/share/rime-data";
  const char* user = argc > 2 ? argv[2] : "/tmp/rime-smoke-user";

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
  traits.distribution_code_name = "rime-sync-smoke";
  traits.distribution_version = "1.0";
  traits.app_name = "rime.smoke";

  // 第三个参数：--no-deploy = 不部署，直接加载 user_data_dir/build 里已有的产物。
  bool no_deploy = argc > 3 && std::strcmp(argv[3], "--no-deploy") == 0;

  api->setup(&traits);
  api->set_notification_handler(OnNotification, nullptr);
  api->initialize(&traits);
  if (no_deploy) {
    std::printf("deploy: skipped (--no-deploy: loading prebuilt products only)\n");
  } else {
    api->start_maintenance(1);
    api->join_maintenance_thread();
    std::printf("deploy: %s\n",
                g_deploy == 1 ? "success" : (g_deploy == -1 ? "failure" : "not-reported"));
  }

  RimeSessionId session = api->create_session();
  if (!session) {
    std::printf("FAIL: create_session()\n");
    api->finalize();
    return 3;
  }

  api->simulate_key_sequence(session, "nihao");
  RIME_STRUCT(RimeContext, ctx);
  int found = 0;
  if (!api->get_context(session, &ctx)) {
    std::printf("FAIL: get_context()\n");
  } else {
    std::printf("candidates:");
    for (int i = 0; i < ctx.menu.num_candidates; ++i) {
      const char* text = ctx.menu.candidates[i].text;
      std::printf(" [%d]%s", i, text ? text : "");
      if (text && std::strcmp(text, "你好") == 0) found = 1;
    }
    std::printf("\n");
    api->free_context(&ctx);
  }
  api->destroy_session(session);
  api->finalize();

  if (!found) {
    std::printf("FAIL: candidate 你好 not found\n");
    return 4;
  }
  std::printf("PASS: candidate 你好 found\n");
  return 0;
}
