// --setup 请求的两条通道汇合成「要不要开向导、开哪一种」。
//
// 两条通道：
// - 事件 `setup://request`：第二次启动带 --setup 时后端发给已在运行的实例。
//   事件可能早于前端注册监听而丢失。
// - 短命槽位 `cmd_take_setup_slot`：同一次请求在后端留一份短命的内存状态，
//   前端注册完监听后再兜底取一次（只在进程内活着，重启必为空）。
//
// 单独成模块、不掺 DOM：这样「事件丢了也不能丢请求」这条性质
// 能被纯逻辑测试钉住（src/setup_request.test.ts，node --test 跑）。

/** 向导的两种模式：首次运行 / 同步设置。 */
export type WizardMode = "first-run" | "settings";

/**
 * 本次进程带 --setup 启动时的强制向导。
 *
 * @param forced 启动参数里带了 --setup
 */
export function forcedSetupMode(forced: boolean): WizardMode | null {
  return forced ? "settings" : null;
}

/**
 * 短命槽位兜底：注册完监听后取到了请求就开同步设置向导。
 *
 * 已经有向导开着时不再开一次（同一个向导开两遍没有意义，
 * 第二次 openWizard 会把用户刚填的东西重置掉）。
 *
 * @param lateSetup 短命槽位里是否还有 --setup 请求
 * @param wizardOpened 本次启动是否已经开过向导（强制 --setup 或首次运行）
 */
export function lateSetupMode(
  lateSetup: boolean,
  wizardOpened: boolean,
): WizardMode | null {
  if (!lateSetup) {
    return null;
  }
  return wizardOpened ? null : "settings";
}
