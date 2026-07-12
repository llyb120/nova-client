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
  return commands
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
