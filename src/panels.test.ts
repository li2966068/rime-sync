// 页面名白名单的纯逻辑测试。
//
// 跑法：node --experimental-strip-types --test src/panels.test.ts
// （Node 22 自带 node:test，无需任何测试框架依赖；CI 的 frontend job 会跑这一条。）

import assert from "node:assert/strict";
import test from "node:test";

import { isKnownPanel, PANEL_NAMES } from "./panels.ts";

test("五个页面名都在白名单内", () => {
  for (const name of PANEL_NAMES) {
    assert.equal(isKnownPanel(name), true, `${name} 必须是合法页面名`);
  }
});

test("白名单恰好是这五个页面（不多不少）", () => {
  assert.deepEqual(
    [...PANEL_NAMES].sort(),
    ["conflicts", "connection", "devices", "settings", "sync"],
  );
});

test("未知页面名一律拒绝", () => {
  // 外部参数不能决定前端切到哪个视图：只认白名单。
  for (const name of ["evil", "connection ", "Connection", "panels", "panel-sync"]) {
    assert.equal(isKnownPanel(name), false, `${name} 必须被拒绝`);
  }
});

test("空值与空串一律拒绝", () => {
  assert.equal(isKnownPanel(""), false);
  assert.equal(isKnownPanel(null), false);
  assert.equal(isKnownPanel(undefined), false);
});
