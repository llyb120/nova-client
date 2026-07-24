import type { AgentKind, SlashCommand } from "../types";
import { agentLabel } from "../utils";

export type SlashSuggestion = {
  id: string;
  title: string;
  detail: string;
  kind: string;
  insertText: string;
};

function commandToSuggestion(agentKind: AgentKind, command: SlashCommand): SlashSuggestion {
  const name = command.name.replace(/^\/+/, "");
  const input = command.input ?? `/${name} `;
  return {
    id: `${agentKind}:command:${name}`,
    title: `/${name}`,
    detail: command.description ?? `${agentLabel(agentKind)} command`,
    kind: command.kind ?? "command",
    insertText: input.endsWith(" ") ? input : `${input} `,
  };
}

export function getSlashSuggestions(
  agentKind: AgentKind,
  commands: SlashCommand[],
  query: string,
): SlashSuggestion[] {
  const builtins: SlashCommand[] = [
    {
      name: "fire",
      description: "分阶段执行，并用独立会话反复验收直到目标达成",
      kind: "Nova",
      input: "/fire ",
    },
    {
      name: "target",
      description: "为 /fire 明确指定验收规则（需与 /fire 一起发送）",
      kind: "Nova",
      input: "/target ",
    },
  ];
  return [...builtins, ...commands]
    .map((c) => commandToSuggestion(agentKind, c))
    .filter((item, index, all) => all.findIndex((x) => x.id === item.id) === index)
    .filter((item) => {
      if (!query) return true;
      const q = query.toLowerCase();
      return (
        item.title.toLowerCase().includes(q) ||
        item.detail.toLowerCase().includes(q) ||
        item.kind.toLowerCase().includes(q)
      );
    });
}
