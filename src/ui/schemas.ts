import { api, type SchemaEntry } from "../api";
import { t } from "../i18n";
import { el } from "./dom";
import { errorText, escapeHtml } from "./format";
import { toast } from "./toast";
import { localLog } from "./logs";
import { busy } from "./state";

// 设置页「输入方案」卡片：启用/停用、调整顺序、保存并重新部署。
// 顺序即候选切换顺序：启用的方案按数组顺序写入 patch.schema_list，第一项为默认。

let schemas: SchemaEntry[] = [];
let loaded = false;
// 后端读取失败的原始错误；语言切换后重新包一层文案，所以只存原文。
let loadError: string | null = null;

function enabledList(): SchemaEntry[] {
  return schemas.filter((schema) => schema.enabled);
}

/// 启用的保持用户顺序；停用的按名字排（后端已排好序，这里只兜住切换后的插入位置）。
function sortGroups(): void {
  const enabled = enabledList();
  const disabled = schemas
    .filter((schema) => !schema.enabled)
    .sort(
      (left, right) =>
        left.name.localeCompare(right.name) || left.id.localeCompare(right.id),
    );
  schemas = [...enabled, ...disabled];
}

function sourceLabel(schema: SchemaEntry): string {
  return schema.source === "local" ? t("schemas.source.local") : t("schemas.source.shared");
}

function moveButton(
  id: string,
  direction: "up" | "down",
  enabled: boolean,
  symbol: string,
): string {
  const label = escapeHtml(t(direction === "up" ? "schemas.up" : "schemas.down"));
  return (
    '<button type="button" class="ghost small" data-schema-move="' +
    direction +
    '" data-schema-id="' +
    id +
    '"' +
    (enabled ? "" : " disabled") +
    ' title="' +
    label +
    '" aria-label="' +
    label +
    '">' +
    symbol +
    "</button>"
  );
}

export function renderSchemas(): void {
  const enabled = enabledList();
  el.schemaEmpty.hidden = schemas.length > 0;
  el.schemaEmpty.textContent = loadError
    ? t("schemas.loadFail", { error: loadError })
    : t("schemas.empty");
  el.schemaSave.disabled = busy || enabled.length === 0;

  let position = 0;
  el.schemaList.innerHTML = schemas
    .map((schema) => {
      const id = escapeHtml(schema.id);
      const order = schema.enabled ? position++ : -1;
      const canUp = schema.enabled && order > 0;
      const canDown = schema.enabled && order < enabled.length - 1;
      return (
        '<div class="schema-row' +
        (schema.enabled ? " is-enabled" : "") +
        '">' +
        '<label class="check"><input type="checkbox" data-schema-toggle="' +
        id +
        '"' +
        (schema.enabled ? " checked" : "") +
        ' /><span><span class="schema-name">' +
        escapeHtml(schema.name) +
        "</span><em>" +
        id +
        " · " +
        escapeHtml(sourceLabel(schema)) +
        "</em></span></label>" +
        '<div class="schema-order">' +
        moveButton(id, "up", canUp, "↑") +
        moveButton(id, "down", canDown, "↓") +
        "</div></div>"
      );
    })
    .join("");
}

export async function loadSchemas(force = false): Promise<void> {
  if (loaded && !force) {
    renderSchemas();
    return;
  }
  el.schemaRefresh.disabled = true;
  try {
    schemas = await api.listSchemas();
    loaded = true;
    loadError = null;
    sortGroups();
    el.schemaStatus.textContent = "";
  } catch (error) {
    loaded = false;
    schemas = [];
    loadError = errorText(error);
  } finally {
    el.schemaRefresh.disabled = false;
    renderSchemas();
  }
}

/// 设置页首次打开时才拉取：避免启动时对未安装方案的机器多一次失败请求。
export async function ensureSchemasLoaded(): Promise<void> {
  if (!loaded) {
    await loadSchemas();
  }
}

export function toggleSchema(id: string, enabled: boolean): void {
  const entry = schemas.find((schema) => schema.id === id);
  if (!entry) {
    return;
  }
  entry.enabled = enabled;
  sortGroups();
  renderSchemas();
}

export function moveSchema(id: string, direction: -1 | 1): void {
  const enabled = enabledList();
  const index = enabled.findIndex((schema) => schema.id === id);
  const target = index + direction;
  if (index < 0 || target < 0 || target >= enabled.length) {
    return;
  }
  const reordered = [...enabled];
  [reordered[index], reordered[target]] = [reordered[target], reordered[index]];
  schemas = [...reordered, ...schemas.filter((schema) => !schema.enabled)];
  renderSchemas();
}

export async function saveSchemas(): Promise<void> {
  const ids = enabledList().map((schema) => schema.id);
  if (ids.length === 0) {
    toast(t("schemas.needOne"));
    return;
  }
  el.schemaSave.disabled = true;
  el.schemaStatus.textContent = t("schemas.saving");
  try {
    const result = await api.setSchemas(ids);
    schemas = result.schemas;
    loaded = true;
    loadError = null;
    sortGroups();
    if (result.deploy_error) {
      // 配置已写入（后端写前已备份）：只提示重试部署，列表以磁盘为准重绘。
      const text = t("schemas.deployFail", { error: result.deploy_error });
      el.schemaStatus.textContent = text;
      toast(text);
      localLog(text);
    } else {
      const text = t("schemas.saved");
      el.schemaStatus.textContent = text;
      toast(text);
      localLog(text);
    }
  } catch (error) {
    const text = t("schemas.saveFail", { error: errorText(error) });
    el.schemaStatus.textContent = text;
    toast(text);
    localLog(text);
  } finally {
    renderSchemas();
  }
}
