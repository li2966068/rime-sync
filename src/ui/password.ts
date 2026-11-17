import { el } from "./dom";
import { t } from "../i18n";
import { draftPassword, PASSWORD_MASK, shouldShowMask } from "./password_mask";

// 密码框的状态机：后端里已经存着密码，但前端**不持有**它。
//
// 为什么用掩码而不是留空：留空看起来像「密码被清掉了」，用户会紧张，
// 甚至重新输一遍（而降级模式下重新输也存不进去）。掩码明确表达
// 「密码还在，留空表示沿用已保存的密码」—— 于是用户只改地址时，
// 密码不会被抹掉。

let stored = false; // 后端里已经存着密码
let edited = false; // 用户在本轮里手动改过密码框

/** 后端说「密码已设置」时调用：密码框显示成掩码。 */
export function setPasswordStored(value: boolean): void {
  stored = value;
  applyPasswordMask();
}

/** 用户手动动了密码框：掩码失效，输入框里的值就是他自己的。 */
export function markPasswordEdited(): void {
  edited = true;
  el.pass.dataset.masked = "0";
}

/** 提交用的密码：掩码 ⇒ 空串（后端理解为「沿用已保存的密码」）。 */
export function passwordDraftValue(): string {
  return draftPassword(el.pass.value, el.pass.dataset.masked === "1");
}

/** 后端回填配置后调用：清掉输入框里的明文，再按「是否已存密码」决定显示掩码。 */
export function resetPasswordField(value: string): void {
  edited = false;
  el.pass.dataset.masked = "0";
  el.pass.value = value;
  applyPasswordMask();
}

/** 按「后端是否已存密码」刷新密码框的显示。 */
export function applyPasswordMask(): void {
  if (shouldShowMask(stored, edited, el.pass.value)) {
    el.pass.value = PASSWORD_MASK;
    el.pass.dataset.masked = "1";
    el.pass.setAttribute("placeholder", t("conn.secretSaved"));
    return;
  }
  if (!stored && el.pass.dataset.masked === "1") {
    el.pass.value = "";
    el.pass.dataset.masked = "0";
  }
}
