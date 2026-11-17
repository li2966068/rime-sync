// 密码框的纯逻辑（不碰 DOM，方便单测）。

/** 密码已存在后端时的占位掩码（前端永远拿不到真实密码）。 */
export const PASSWORD_MASK = "••••••••";

/**
 * 提交用的密码：掩码 ⇒ 空串（后端理解为「沿用已保存的密码」）。
 *
 * 为什么：界面上不该出现真密码，用户改个地址不该把密码抹掉；
 * 后端拿到空密码时的语义是「沿用已保存的那个」，而不是「用户清空了密码」。
 */
export function draftPassword(value: string, masked: boolean): string {
  return masked && value === PASSWORD_MASK ? "" : value;
}

/**
 * 该不该显示掩码：后端已存着密码、用户这轮没手动改过、输入框现在是空的。
 */
export function shouldShowMask(stored: boolean, edited: boolean, current: string): boolean {
  return stored && !edited && current === "";
}
