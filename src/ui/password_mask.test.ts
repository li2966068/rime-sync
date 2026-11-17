// 密码框的纯逻辑测试。
//
// 跑法：node --experimental-strip-types --test src/ui/password_mask.test.ts
// （Node 22 自带 node:test，无需任何测试框架依赖。）

import assert from "node:assert/strict";
import test from "node:test";

import { draftPassword, PASSWORD_MASK, shouldShowMask } from "./password_mask.ts";

test("掩码提交为空串：后端按「沿用已保存的密码」处理，不会把密码清掉", () => {
  assert.equal(draftPassword(PASSWORD_MASK, true), "");
});

test("用户自己填的密码原样提交（掩码状态下只要不是掩码本身就算用户输入）", () => {
  assert.equal(draftPassword("my-pass", false), "my-pass");
  assert.equal(draftPassword("my-pass", true), "my-pass");
});

test("后端已存密码、用户没改过、输入框是空的 ⇒ 显示掩码", () => {
  assert.equal(shouldShowMask(true, false, ""), true);
});

test("用户动过密码框 ⇒ 不再盖掩码（否则会把用户刚填的密码冲掉）", () => {
  assert.equal(shouldShowMask(true, true, ""), false);
  assert.equal(shouldShowMask(true, false, "typed"), false);
});

test("后端没有密码 ⇒ 不显示掩码（空框就是「还没设密码」）", () => {
  assert.equal(shouldShowMask(false, false, ""), false);
});
