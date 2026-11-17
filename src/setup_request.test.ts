// --setup 请求兜底逻辑的纯逻辑测试。
//
// 跑法：node --experimental-strip-types --test src/setup_request.test.ts
// （Node 22 自带 node:test，无需任何测试框架依赖；CI 的 frontend job 会跑 src/*.test.ts。）

import assert from "node:assert/strict";
import test from "node:test";

import { forcedSetupMode, lateSetupMode } from "./setup_request.ts";

test("本次进程带 --setup 启动：强制进同步设置向导", () => {
  assert.equal(forcedSetupMode(true), "settings");
});

test("没带 --setup 启动：不开强制向导", () => {
  assert.equal(forcedSetupMode(false), null);
});

test("短命槽位里有请求：兜底开一次同步设置向导", () => {
  // 事件早于前端注册监听而丢失时，靠这一条把请求捞回来。
  assert.equal(lateSetupMode(true, false), "settings");
});

test("短命槽位是空的：不重复开向导", () => {
  assert.equal(lateSetupMode(false, false), null);
  assert.equal(lateSetupMode(false, true), null);
});

test("已经开过向导时，短命槽位不再开一次", () => {
  // 重复 openWizard 会把用户刚填的表单重置掉，所以只在没开过向导时才补开。
  assert.equal(lateSetupMode(true, true), null);
});
