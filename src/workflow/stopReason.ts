/**
 * 轮次结束原因（stopReason）归一化：纯函数，便于脱离 Solid/Tauri 单测。
 *
 * ACP 是 camelCase 协议（响应字段即 `stopReason`），部分实现会把枚举名
 * END_TURN / MAX_TURN_REQUESTS 原样透传；而内部（工作流推进、未读标记、
 * 通知文案）一律按 snake_case 分流。此前直接全等比较，一旦对端给出
 * camelCase，一次正常收尾就会被判成「异常收尾」走 suspendWorkflow 挂起，
 * 整条工作流链永久留在室女座，且没有任何界面提示。
 */
export function canonicalStopReason(reason: string | null | undefined): string {
  return (reason ?? "")
    .trim()
    .replace(/[-\s]+/g, "_")
    // 只在「小写/数字 → 大写」处插分隔符，再整体转小写：camelCase（endTurn）
    // 与常量名（END_TURN）都能收敛到内部 snake_case，且不会拆出多余下划线。
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .toLowerCase()
    .replace(/_+/g, "_");
}

/** 回合是否算正常收尾——只有正常收尾才推进工作流到下一节点。 */
export function isNormalStop(reason: string | null | undefined): boolean {
  const canonical = canonicalStopReason(reason);
  return canonical === "end_turn" || canonical === "max_turn_requests";
}

/** 回合是否由用户主动中止——中止视为放弃流程，整条链移出室女座。 */
export function isManualStop(reason: string | null | undefined): boolean {
  const canonical = canonicalStopReason(reason);
  return canonical === "cancelled" || canonical === "force_cancelled";
}
