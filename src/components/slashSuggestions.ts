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
      name: "plan",
      description: "先出实施计划（少追问），仍在 Build 下发送",
      kind: "Nova",
      input: "/plan ",
    },
    {
      name: "easy",
      description: "显而易见的小修改：不运行或编写测试，仅做基本编译校验",
      kind: "Nova",
      input: "/easy ",
    },
    {
      name: "stage",
      description: "用轻量模型开启引用当前会话的 Stage：前置内容先续跑当前会话",
      kind: "Nova",
      input: "/stage ",
    },
    {
      name: "hard",
      description: "让当前 Agent 设计简洁工作流并立即执行",
      kind: "Nova",
      input: "/hard ",
    },
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
    {
      name: "run",
      description: "运行一个已配置的工作流：/run 工作流名 目标",
      kind: "Nova",
      input: "/run ",
    },
    {
      name: "train",
      description: "立即用大熊座配置的模型训练一次经验库",
      kind: "Nova",
      input: "/train",
    },
    {
      name: "setup",
      description: "把一个模型 / provider 接入 Lyra：/setup 模型名",
      kind: "Nova",
      input: "/setup ",
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
