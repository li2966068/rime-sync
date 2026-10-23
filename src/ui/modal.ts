import { el } from "./dom";

// 通用确认弹窗。

let modalResolve: ((value: boolean) => void) | null = null;

export function confirmDialog(title: string, body: string): Promise<boolean> {
  el.modalTitle.textContent = title;
  el.modalBody.textContent = body;
  el.modal.hidden = false;
  el.modalConfirm.focus();
  return new Promise((resolve) => {
    modalResolve = resolve;
  });
}

export function settleModal(value: boolean): void {
  el.modal.hidden = true;
  const resolve = modalResolve;
  modalResolve = null;
  resolve?.(value);
}
