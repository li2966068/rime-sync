import { en } from "./en";
import { zh } from "./zh";

export type Lang = "zh" | "en";
export type MessageKey = keyof typeof zh;

const STORAGE_KEY = "rime-sync.lang";
const dictionaries: Record<Lang, Record<MessageKey, string>> = { zh, en };

function readLang(): Lang {
  try {
    return localStorage.getItem(STORAGE_KEY) === "en" ? "en" : "zh";
  } catch {
    return "zh";
  }
}

let lang: Lang = readLang();

export function getLang(): Lang {
  return lang;
}

export function setLang(next: Lang): void {
  lang = next;
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // localStorage 可能被禁用：内存里的选择仍然生效。
  }
  document.documentElement.lang = next === "zh" ? "zh-CN" : "en";
}

export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const template = dictionaries[lang][key] ?? zh[key] ?? key;
  if (!params) {
    return template;
  }
  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

function setText(node: Element, key: string | undefined): void {
  if (!key) {
    return;
  }
  node.textContent = t(key as MessageKey);
}

export function applyI18n(root: ParentNode = document): void {
  root.querySelectorAll<HTMLElement>("[data-i18n]").forEach((node) => setText(node, node.dataset.i18n));
  root.querySelectorAll<HTMLElement>("[data-i18n-placeholder]").forEach((node) => {
    const key = node.dataset.i18nPlaceholder;
    if (key) {
      node.setAttribute("placeholder", t(key as MessageKey));
    }
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-title]").forEach((node) => {
    const key = node.dataset.i18nTitle;
    if (key) {
      node.setAttribute("title", t(key as MessageKey));
    }
  });
  root.querySelectorAll<HTMLElement>("[data-i18n-aria-label]").forEach((node) => {
    const key = node.dataset.i18nAriaLabel;
    if (key) {
      node.setAttribute("aria-label", t(key as MessageKey));
    }
  });
  document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
}

export function presetLabel(id: string): string {
  return t(("preset." + id) as MessageKey);
}

export function presetHint(id: string): string {
  return t(("presetHint." + id) as MessageKey);
}
