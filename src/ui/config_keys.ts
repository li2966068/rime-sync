// 配置键的双语显示名：界面里展示 *.custom.yaml 的 patch 键时统一走这里。
// 键名本身不变（写回配置时仍用原始键），只负责换成人能读的「中文（English）」。

export interface KeyLabel {
  zh: string;
  en: string;
}

/// 键名表按「去掉 patch. 前缀」的裸键索引，例如 menu/page_size。
export const CONFIG_KEY_LABELS: Record<string, KeyLabel> = {
  schema_list: { zh: "输入方案列表", en: "Schema list" },

  "menu/page_size": { zh: "每页候选数", en: "Candidates per page" },
  "menu/alternative_select_keys": { zh: "选词键", en: "Selection keys" },
  "menu/alternative_select_labels": { zh: "选词标签", en: "Selection labels" },

  "style/horizontal": { zh: "横向候选", en: "Horizontal candidates" },
  "style/color_scheme": { zh: "配色方案", en: "Color scheme" },
  "style/font_point": { zh: "字号", en: "Font size" },
  "style/candidate_list_layout": { zh: "候选排列", en: "Candidate layout" },
  "style/font_face": { zh: "字体", en: "Font family" },
  "style/inline_preedit": { zh: "行内预编辑", en: "Inline preedit" },

  "ascii_composer/switch_key": { zh: "中英切换键", en: "Script switch key" },
  "ascii_composer/good_old_caps_lock": { zh: "Caps Lock 大写行为", en: "Caps Lock behavior" },

  "punctuator/full_shape": { zh: "全角标点", en: "Full-width punctuation" },
  "punctuator/half_shape": { zh: "半角标点", en: "Half-width punctuation" },

  "key_binder/bindings": { zh: "快捷键绑定", en: "Key bindings" },
  "key_binder/import_preset": { zh: "预设按键方案", en: "Preset key bindings" },

  "switches/ascii_mode": { zh: "中英切换开关", en: "ASCII mode switch" },
  "switches/full_shape": { zh: "全角/半角开关", en: "Full-shape switch" },
  "switches/emoji": { zh: "Emoji 开关", en: "Emoji switch" },
};

const PATCH_PREFIX = "patch.";

/// 冲突报告里的键带 patch. 前缀（patch.menu/page_size）；查表用裸键。
export function bareKey(key: string): string {
  const trimmed = key.trim();
  return trimmed.startsWith(PATCH_PREFIX) ? trimmed.slice(PATCH_PREFIX.length) : trimmed;
}

/// 已知键返回「中文（English）」；未知键原样返回，避免丢掉后端新加的键。
export function labelForKey(key: string): string {
  const label = CONFIG_KEY_LABELS[bareKey(key)];
  return label ? label.zh + "（" + label.en + "）" : key;
}

/// 报告行形如 "default.custom.yaml: patch.menu/page_size, patch.style/font_point"：
/// 只把 patch 键换成双语标签，文件名与其它说明（如「远端在传输中被改动」）保持原样。
export function labelConflictLine(line: string): string {
  return line.replace(/patch\.[A-Za-z0-9_/-]+/g, (token) => labelForKey(token));
}
