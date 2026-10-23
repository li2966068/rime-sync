import { el } from "./dom";

// 右下角轻提示。

let toastTimer = 0;

export function toast(message: string): void {
  el.toast.textContent = message;
  el.toast.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    el.toast.hidden = true;
  }, 2600);
}
