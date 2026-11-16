// 页面名白名单：外部（启动参数、单实例转发、托盘动作）能指定的页面只有这五个。
//
// 单独成模块、不掺 DOM：这样「外部参数不能决定前端切到哪个视图」这条安全性质
// 能被纯逻辑测试钉住（src/panels.test.ts，node --test 跑，零测试框架依赖）。
//
// 名字必须与 index.html 里各 tab 的 data-panel 一致。

export const PANEL_NAMES = [
  "connection",
  "sync",
  "conflicts",
  "devices",
  "settings",
] as const;

export type PanelName = (typeof PANEL_NAMES)[number];

/** 页面名是否在白名单内。null/undefined/空串/未知名字一律为 false。 */
export function isKnownPanel(name: string | null | undefined): name is PanelName {
  if (!name) {
    return false;
  }
  return (PANEL_NAMES as readonly string[]).includes(name);
}
