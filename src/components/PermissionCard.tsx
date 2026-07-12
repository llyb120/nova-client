import { For, Show } from "solid-js";
import { respondPermission } from "../store";
import type { PermissionRequest } from "../types";
import { agentLabel, stripAnsi } from "../utils";
import { toolIcon } from "./icons";

function inputPreview(raw: unknown): string {
  if (raw == null) return "";
  if (typeof raw === "object") {
    const o = raw as Record<string, unknown>;
    const cmd = o.command ?? o.cmd ?? o.path ?? o.file_path ?? o.url;
    if (typeof cmd === "string") return stripAnsi(cmd);
    try {
      const s = JSON.stringify(raw);
      return stripAnsi(s.length > 200 ? s.slice(0, 200) + "…" : s);
    } catch {
      return "";
    }
  }
  return stripAnsi(String(raw));
}

function buttonClass(kind: string): string {
  if (kind.startsWith("allow")) return "perm-btn allow";
  if (kind.startsWith("reject")) return "perm-btn reject";
  return "perm-btn";
}

export function PermissionCard(props: { req: PermissionRequest }) {
  const preview = () => inputPreview(props.req.toolCall?.rawInput);
  const agent = () => agentLabel(props.req.agentKind ?? "devin");
  return (
    <div class="perm-card">
      <div class="perm-head">
        <span class="perm-icon">{toolIcon(props.req.toolCall?.kind ?? "other")}</span>
        <span class="perm-title">
          {agent()} 请求权限：{props.req.toolCall?.title ?? "执行工具"}
        </span>
      </div>
      <Show when={preview()}>
        <pre class="perm-preview">{preview()}</pre>
      </Show>
      <div class="perm-actions">
        <For each={props.req.options}>
          {(opt) => (
            <button
              class={buttonClass(opt.kind)}
              onClick={() => void respondPermission(props.req.requestKey, opt.optionId)}
            >
              {opt.name}
            </button>
          )}
        </For>
      </div>
    </div>
  );
}
