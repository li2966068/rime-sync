// 方案裁剪预演的「纯逻辑」部分：把后端返回的预演结果整理成人能看懂的清单。
//
// 单独成模块、不掺 DOM：这样「预演要说清哪三件事」这条性质能被纯逻辑测试钉住
// （src/ui/prune_view.test.ts，node --test 跑）。

/** 后端预演结果里我们用到的部分（字段名与 Rust 侧 serde 一致）。 */
export interface PrunePreviewInput {
  token: string;
  files: string[];
  schema_lists: {
    path: string;
    after: string;
    dropped: number;
    removed_schemas: string[];
  }[];
  affected_schemas: string[];
  has_changes: boolean;
  disabled: boolean;
  warnings: string[];
}

/** 翻译函数（注入进来，便于测试不依赖 DOM 与 localStorage）。 */
export type Translate = (key: string, params?: Record<string, string | number>) => string;

/** 一个 schema_list 的展示块。 */
export interface SchemaListBlock {
  path: string;
  /** 改写后的完整内容。 */
  after: string;
  /** 被删掉的方案 id（人话层面）。 */
  removed: string[];
}

/** 预演结果的展示模型。 */
export interface PruneView {
  /** 一句话结论。 */
  headline: string;
  /** 待删文件清单。 */
  fileLines: string[];
  /** 删完之后 schema_list 长什么样。 */
  schemaListBlocks: SchemaListBlock[];
  /** 会因此在输入法里消失的方案。 */
  affected: string[];
  /** 告警（探测失败等）。 */
  warnings: string[];
  /** 能不能点「确认删除」：没东西可删或已关闭时不能。 */
  canConfirm: boolean;
  /** 预演令牌（确认执行时要带回后端）。 */
  token: string;
}

/**
 * 把预演结果整理成展示模型。
 *
 * 三件事必须都在结果里（这是任务硬要求）：
 * 1. 哪些文件会被删；
 * 2. 新的 schema_list 长什么样；
 * 3. 哪些方案会因此在输入法里消失。
 */
export function buildPruneView(preview: PrunePreviewInput, translate: Translate): PruneView {
  const fileLines = preview.files.slice();
  const schemaListBlocks = preview.schema_lists.map((item) => ({
    path: item.path,
    after: item.after,
    removed: item.removed_schemas.slice(),
  }));
  const affected = preview.affected_schemas.slice();

  let headline: string;
  if (preview.disabled) {
    headline = translate("prune.disabled");
  } else if (!preview.has_changes) {
    headline = translate("prune.nothing");
  } else {
    headline = translate("prune.summary", {
      files: preview.files.length,
      schemas: affected.length,
    });
  }

  return {
    headline,
    fileLines,
    schemaListBlocks,
    affected,
    warnings: preview.warnings.slice(),
    canConfirm: preview.has_changes && !preview.disabled && preview.token.length > 0,
    token: preview.token,
  };
}
