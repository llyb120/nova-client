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
    { name: "generate-image", description: "生成新图，可参考附图的内容或风格：/generate-image 图片描述", kind: "Nova", input: "/generate-image " },
    { name: "edit-image", description: "编辑原图，保留未要求改动的内容：/edit-image 修改要求", kind: "Nova", input: "/edit-image " },
    { name: "setup-image", description: "配置通用生图 API 地址、Token 和图片模型", kind: "Nova", input: "/setup-image " },
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
    ...(agentKind === "lyra" ? [
      {
        name: "browser",
        description: "进入持续的 Playwright 前端调试模式：/browser 网址和任务",
        kind: "Nova",
        input: "/browser ",
      },
      {
        name: "browser-exit",
        description: "退出 Playwright 前端调试模式",
        kind: "Nova",
        input: "/browser-exit",
      },
    ] : []),
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
