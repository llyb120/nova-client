import { mkdir, readFile, realpath, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";
import { Type } from "typebox";

const execFileAsync = promisify(execFile);
const projectIdentityCache = new Map();
let storeCache;
const entryTermsCache = new WeakMap();

async function projectIdentity(cwd) {
  const requested = resolve(String(cwd || process.cwd()));
  let pending = projectIdentityCache.get(requested);
  if (!pending) {
    pending = (async () => {
      let root = requested;
      try {
        const { stdout } = await execFileAsync("git", ["-C", root, "rev-parse", "--path-format=absolute", "--git-common-dir"], { windowsHide: true });
        const common = resolve(root, stdout.trim());
        if (basename(common) === ".git") root = dirname(common);
      } catch {
        root = await realpath(root).catch(() => root);
      }
      let key = root.replaceAll("\\", "/").replace(/\/$/, "");
      if (process.platform === "win32") key = key.toLowerCase();
      return { key, root };
    })();
    projectIdentityCache.set(requested, pending);
  }
  try {
    return await pending;
  } catch (error) {
    projectIdentityCache.delete(requested);
    throw error;
  }
}

function novaRoot(env = process.env) {
  return env.NOVA_DATA_DIR || join(homedir(), ".nova");
}

function terms(text) {
  const lower = String(text ?? "").toLowerCase();
  const result = new Set(lower.split(/[^\p{L}\p{N}]+/u).filter(Boolean));
  const cjk = [...lower].filter((char) => /[\u3400-\u9fff]/u.test(char));
  for (let index = 0; index + 1 < cjk.length; index += 1) result.add(cjk[index] + cjk[index + 1]);
  return result;
}

function relevance(query, entry) {
  if (!query.size) return 0;
  let haystack = entryTermsCache.get(entry);
  if (!haystack) {
    haystack = terms([entry.trigger, entry.action, entry.avoid, ...(entry.scope ?? [])].join(" "));
    entryTermsCache.set(entry, haystack);
  }
  let hits = 0;
  for (const term of query) if (haystack.has(term)) hits += 1;
  return hits / query.size;
}

async function loadStore() {
  const path = join(novaRoot(), "experience_memory.json");
  const metadata = await stat(path).catch(() => null);
  if (!metadata) {
    storeCache = { path, mtimeMs: 0, size: 0, store: {} };
    return storeCache.store;
  }
  if (storeCache?.path === path && storeCache.mtimeMs === metadata.mtimeMs && storeCache.size === metadata.size) {
    return storeCache.store;
  }
  const text = await readFile(path, "utf8").catch(() => "{}");
  let store;
  try { store = JSON.parse(text); } catch { store = {}; }
  storeCache = { path, mtimeMs: metadata.mtimeMs, size: metadata.size, store };
  return store;
}

function textResult(value) {
  return { content: [{ type: "text", text: JSON.stringify(value, null, 2) }] };
}

async function loadTrainedMemory(params) {
  const query = terms(params?.query);
  const cwd = String(params?.cwd ?? process.cwd());
  const limit = Math.max(1, Math.min(12, Number(params?.limit) || 8));
  const store = await loadStore();
  const { key, root } = await projectIdentity(cwd);
  const universal = (store.universalExperiences ?? store.universal_experiences ?? [])
    .filter((entry) => (entry.knowledgeScope ?? entry.knowledge_scope) === "universal");
  const project = (store.projects?.[key]?.experiences ?? [])
    .filter((entry) => (entry.knowledgeScope ?? entry.knowledge_scope ?? "project") !== "universal");
  const rows = [...universal, ...project]
    .filter((entry) => entry.status !== "quarantined")
    .map((entry) => ({ entry, relevance: relevance(query, entry) }))
    .filter((row) => row.relevance > 0)
    .map((row) => ({ ...row, score: 0.55 * row.relevance + 0.25 * (Number(row.entry.utility) || 0) + 0.20 * (Number(row.entry.confidence) || 0) }))
    .sort((a, b) => b.score - a.score);
  const selectedExperts = [];
  for (const row of rows) {
    if (!selectedExperts.includes(row.entry.expertId ?? row.entry.expert_id)) selectedExperts.push(row.entry.expertId ?? row.entry.expert_id);
    if (selectedExperts.length >= 3) break;
  }
  const counts = new Map();
  const experiences = [];
  for (const { entry } of rows) {
    const expertId = entry.expertId ?? entry.expert_id;
    if (!selectedExperts.includes(expertId) || (counts.get(expertId) ?? 0) >= 3) continue;
    counts.set(expertId, (counts.get(expertId) ?? 0) + 1);
    experiences.push({
      id: entry.id, expertId, kind: entry.kind ?? "experience",
      knowledgeScope: entry.knowledgeScope ?? entry.knowledge_scope ?? "project", trigger: entry.trigger,
      action: entry.action, avoid: entry.avoid ?? "", scope: entry.scope ?? [],
      confidence: entry.confidence ?? 0, utility: entry.utility ?? 0,
    });
    if (experiences.length >= limit) break;
  }
  return textResult({
    activatedExperts: selectedExperts,
    projectRoot: root,
    experiences,
    instruction: "knowledgeScope=universal 的知识可跨项目使用；knowledgeScope=project 的知识只适用于当前 projectRoot。memory 是可参考事实，rule 是强约束，experience 仅在触发条件匹配时参考；当前会话事实始终优先。",
  });
}

export async function appendTrainedKnowledge(codeText, params, cwd = process.cwd()) {
  if (process.env.NOVA_EXPERIENCE_TOOLS !== "1") return String(codeText ?? "");
  const query = String(params?.task ?? "").trim()
    || (Array.isArray(params?.keywords) ? params.keywords.join(" ") : String(params?.keywords ?? "")).trim();
  if (!query) return String(codeText ?? "");
  const result = await loadTrainedMemory({ query, limit: 8, cwd });
  const memory = JSON.parse(result.content[0].text);
  if (!memory.experiences?.length) return String(codeText ?? "");
  const rendered = memory.experiences.map((item) =>
    `- [${item.knowledgeScope === "universal" ? "泛用" : "项目独有"}/${item.kind}] id=${item.id} 条件/上下文：${item.trigger}\n  内容：${item.action}`).join("\n");
  return `${String(codeText ?? "")}\n\n# TRAINED KNOWLEDGE\nprojectRoot=${memory.projectRoot}\nactivatedExperts=${JSON.stringify(memory.activatedExperts)}\n${rendered}\n# FEEDBACK REQUIRED\n最终回复前调用 feedback_memory；采用并验证用 ±1，未采用或无法验证用 0。`;
}


async function feedbackMemory(params, cwd = process.cwd()) {
  const experienceIds = Array.isArray(params?.experienceIds) ? params.experienceIds.map(String).filter(Boolean) : [];
  const reward = Math.max(-1, Math.min(1, Number(params?.reward) || 0));
  if (!experienceIds.length && reward !== 0) throw new Error("feedback_memory 非零反馈必须提供 experienceIds");
  const inbox = join(novaRoot(), "experience-feedback-inbox");
  await mkdir(inbox, { recursive: true });
  const payload = { cwd, experienceIds, reward, note: String(params?.note ?? ""), createdAt: Date.now(), source: "vega" };
  await writeFile(join(inbox, `${Date.now()}-${randomUUID()}.json`), JSON.stringify(payload), "utf8");
  return textResult({ queued: true, updated: experienceIds.length, reward });
}

export function createExperienceTools(currentRoot = () => process.cwd()) {
  if (process.env.NOVA_EXPERIENCE_TOOLS !== "1") return [];
  return [{
    name: "feedback_memory",
    description: "闭环反馈 fast_context 本轮返回且实际使用的训练知识；非零反馈必须提供采用的条目 id。",
    parameters: Type.Object({
      experienceIds: Type.Array(Type.String(), { description: "实际影响本轮决策的知识 id" }),
      reward: Type.Number({ minimum: -1, maximum: 1, description: "-1 明确有害，0 无法判断或未采用，1 明确有效" }),
      note: Type.String({ description: "成功、失败、不采用或无法验证的依据" }),
    }),
    async execute(_id, params) { return feedbackMemory(params, currentRoot()); },
  }];
}
