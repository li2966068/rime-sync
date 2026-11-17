// 方案裁剪预演的纯逻辑测试。
//
// 跑法：node --experimental-strip-types --test src/ui/prune_view.test.ts
// （Node 22 自带 node:test，无需任何测试框架依赖；
//  CI 的 frontend job 用 find src -name '*.test.ts' 收集全部测试。）

import assert from "node:assert/strict";
import test from "node:test";

import { buildPruneView, type PrunePreviewInput } from "./prune_view.ts";

/** 造一份预演结果。 */
function preview(overrides: Partial<PrunePreviewInput> = {}): PrunePreviewInput {
  return {
    token: "prune-0123456789abcdef",
    files: ["/rime/user/terra_pinyin.custom.yaml"],
    schema_lists: [
      {
        path: "/rime/user/default.custom.yaml",
        after: "patch:\n  schema_list:\n    - {schema: luna_pinyin_simp}\n",
        dropped: 1,
        removed_schemas: ["terra_pinyin"],
      },
    ],
    affected_schemas: ["terra_pinyin"],
    has_changes: true,
    disabled: false,
    warnings: [],
    ...overrides,
  };
}

/** 假翻译：直接把 key 与参数打出来，便于断言「用了哪些文案」。 */
const translate = (key: string, params?: Record<string, string | number>): string =>
  params ? `${key}:${JSON.stringify(params)}` : key;

test("预演结果要说清三件事：删哪些文件、schema_list 变成什么样、哪些方案会消失", () => {
  const view = buildPruneView(preview(), translate);
  // 1) 待删文件
  assert.deepEqual(view.fileLines, ["/rime/user/terra_pinyin.custom.yaml"]);
  // 2) 删除后的 schema_list
  assert.equal(view.schemaListBlocks.length, 1);
  assert.equal(view.schemaListBlocks[0].path, "/rime/user/default.custom.yaml");
  assert.match(view.schemaListBlocks[0].after, /luna_pinyin_simp/);
  assert.deepEqual(view.schemaListBlocks[0].removed, ["terra_pinyin"]);
  // 3) 会消失的方案
  assert.deepEqual(view.affected, ["terra_pinyin"]);
});

test("有东西要删时才能点确认", () => {
  assert.equal(buildPruneView(preview(), translate).canConfirm, true);
  // 没什么可删 ⇒ 不能确认（点下去也没有意义）。
  const empty = buildPruneView(
    preview({ files: [], schema_lists: [], affected_schemas: [], has_changes: false }),
    translate,
  );
  assert.equal(empty.canConfirm, false);
  // 被环境开关关掉 ⇒ 不能确认。
  assert.equal(buildPruneView(preview({ disabled: true }), translate).canConfirm, false);
  // 令牌丢了 ⇒ 不能确认（后端会拒）。
  assert.equal(buildPruneView(preview({ token: "" }), translate).canConfirm, false);
});

test("结论行区分「没东西可删」与「被关闭」", () => {
  const empty = buildPruneView(
    preview({ files: [], schema_lists: [], affected_schemas: [], has_changes: false }),
    translate,
  );
  assert.equal(empty.headline, "prune.nothing");
  assert.equal(buildPruneView(preview({ disabled: true }), translate).headline, "prune.disabled");
  assert.equal(
    buildPruneView(preview(), translate).headline,
    'prune.summary:{"files":1,"schemas":1}',
  );
});

test("告警原样带出来（用户目录探测失败等）", () => {
  const view = buildPruneView(
    preview({ warnings: ["未能探测 Rime 用户目录：xxx"] }),
    translate,
  );
  assert.deepEqual(view.warnings, ["未能探测 Rime 用户目录：xxx"]);
});
