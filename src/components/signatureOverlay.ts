import { createSignal } from "solid-js";

/**
 * 启动签名进度（0..1）；null 表示未在签名。
 * 非 null 时输入框水印按进度被从左到右「描出」，签完置回 null 完成固化。
 */
export const [signatureProgress, setSignatureProgress] = createSignal<number | null>(null);
