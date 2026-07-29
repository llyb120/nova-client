// Parity helper: runs Node-side implementations on shared fixtures and emits JSON.
// Usage:
//   node parity-node.mjs smart-edit <cases.json>
//   node parity-node.mjs strip-frontmatter <SKILL.md>
//   node parity-node.mjs expand-skill <skills-dir> <skill-name> [args]
//   node parity-node.mjs models <config.jsonc>
//   node parity-node.mjs sse-parse <sse-recording.txt>
import { readFile } from "node:fs/promises";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const scriptsDir = resolve(__dirname, "../../../scripts");
const scriptsUrl = pathToFileURL(scriptsDir.endsWith("/") ? scriptsDir : scriptsDir + "/").href;

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

async function smartEdit(casesPath) {
  const { applySmartEdits } = await import(new URL("alkaid-smart-edit.mjs", scriptsUrl).href);
  const cases = JSON.parse(await readFile(casesPath, "utf8"));
  const results = [];
  for (const c of cases) {
    try {
      const result = applySmartEdits(c.content, c.edits, c.path);
      results.push({ id: c.id, ok: true, content: result.content, matches: result.matches });
    } catch (error) {
      results.push({ id: c.id, ok: false, error: error instanceof Error ? error.message : String(error) });
    }
  }
  send(results);
}

async function stripFrontmatter(skillMdPath) {
  // stripSkillFrontmatter is not exported from alkaid-core.mjs, so we inline the
  // exact same logic (verbatim from scripts/alkaid-core.mjs line 603-609).
  const content = await readFile(skillMdPath, "utf8");
  function stripSkillFrontmatter(text) {
    if (!text.startsWith("---")) return text;
    const lines = text.split(/\r?\n/);
    if (lines[0].trim() !== "---") return text;
    const end = lines.slice(1).findIndex((line) => line.trim() === "---");
    return end < 0 ? text : lines.slice(end + 2).join("\n");
  }
  send(stripSkillFrontmatter(content));
}

async function expandSkill(skillsDir, skillName, args) {
  const { expandAlkaidSkillCommand } = await import(new URL("alkaid-core.mjs", scriptsUrl).href);
  const { loadSkillsFromDir } = await import("@earendil-works/pi-coding-agent");
  const result = loadSkillsFromDir({ dir: resolve(skillsDir), source: "user" });
  const skills = result.skills ?? result;
  const text = args ? `/skill:${skillName} ${args}` : `/skill:${skillName}`;
  const expanded = await expandAlkaidSkillCommand(text, skills);
  send(expanded);
}

async function modelsAction(configPath) {
  // Replicate the models action from alkaid-bridge-common.mjs using alkaid-config.mjs.
  const { alkaidModelOptions, defaultAlkaidModel, loadAlkaidConfig } = await import(new URL("alkaid-config.mjs", scriptsUrl).href);
  const root = dirname(resolve(configPath));
  const config = await loadAlkaidConfig({ root, serverConfig: undefined });
  const data = {
    configOptions: [{ id: "model", name: "Model", currentValue: defaultAlkaidModel(config), options: alkaidModelOptions(config) }],
    modes: null,
  };
  send(data);
}

async function sseParse(ssePath) {
  // Direct SSE line parser mirroring the pi-ai SDK's openai-responses event handling
  // (node_modules/@earendil-works/pi-ai/dist/api/openai-responses-shared.js).
  // Accumulates text/reasoning deltas, function calls, and usage — same as the Rust
  // feed_sse_chunk parser. This is a reference implementation for parity comparison.
  const recording = await readFile(ssePath, "utf8");
  const lines = recording.split(/\r?\n/);
  const state = {
    text: "",
    thinking: "",
    toolCalls: [],
    toolArgs: {},
    usage: null,
    stopReason: null,
    error: null,
  };
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || !trimmed.startsWith("data: ")) continue;
    const data = trimmed.slice(6);
    if (data === "[DONE]") continue;
    let event;
    try { event = JSON.parse(data); } catch { continue; }
    const type = event.type ?? "";
    switch (type) {
      case "response.output_text.delta":
        if (typeof event.delta === "string") state.text += event.delta;
        break;
      case "response.reasoning.delta":
      case "response.reasoning_summary_text.delta":
        if (typeof event.delta === "string") state.thinking += event.delta;
        break;
      case "response.output_item.added":
        if (event.item?.type === "function_call") {
          const id = event.item.call_id ?? event.item.id ?? "";
          const name = event.item.name ?? "";
          if (id && !state.toolCalls.some((t) => t.id === id)) {
            state.toolCalls.push({ id, name, arguments: "" });
          }
        }
        break;
      case "response.function_call_arguments.delta": {
        const id = event.item_id ?? event.call_id ?? "";
        if (typeof event.delta === "string") {
          state.toolArgs[id] = (state.toolArgs[id] ?? "") + event.delta;
        }
        break;
      }
      case "response.output_item.done":
        if (event.item?.type === "function_call") {
          const id = event.item.call_id ?? event.item.id ?? "";
          const args = typeof event.item.arguments === "string" ? event.item.arguments : "";
          if (id) {
            const existing = state.toolCalls.find((t) => t.id === id);
            if (existing) {
              if (args) existing.arguments = args;
            } else {
              state.toolCalls.push({ id, name: event.item.name ?? "", arguments: args });
            }
          }
        }
        break;
      case "response.completed":
      case "response.done": {
        const resp = event.response ?? event;
        if (resp.usage) state.usage = resp.usage;
        if (typeof resp.status === "string") state.stopReason = resp.status;
        break;
      }
      case "error":
        state.error = event.message ?? "provider error";
        break;
    }
  }
  // Merge accumulated argument deltas into tool calls.
  for (const tc of state.toolCalls) {
    if (!tc.arguments && state.toolArgs[tc.id]) {
      tc.arguments = state.toolArgs[tc.id];
    }
  }
  send(state);
}

const [action, ...rest] = process.argv.slice(2);
try {
  switch (action) {
    case "smart-edit": await smartEdit(rest[0]); break;
    case "strip-frontmatter": await stripFrontmatter(rest[0]); break;
    case "expand-skill": await expandSkill(rest[0], rest[1], rest[2]); break;
    case "models": await modelsAction(rest[0]); break;
    case "sse-parse": await sseParse(rest[0]); break;
    default: throw new Error(`unknown action: ${action}`);
  }
} catch (error) {
  send({ ok: false, error: error instanceof Error ? error.message : String(error) });
  process.exitCode = 1;
}
