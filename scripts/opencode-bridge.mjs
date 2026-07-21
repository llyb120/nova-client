import { createInterface } from "node:readline";
import { createOpencode } from "@opencode-ai/sdk/v2";

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

async function readRequest(lines) {
  const { value, done } = await lines[Symbol.asyncIterator]().next();
  if (done) throw new Error("Missing request");
  return JSON.parse(value);
}

function requireData(result) {
  if (result.error) throw new Error(JSON.stringify(result.error));
  return result.data?.data ?? result.data;
}

function modelRef(model, variant) {
  if (!model) return undefined;
  return { id: model.modelID, providerID: model.providerID, ...(variant ? { variant } : {}) };
}

function promptInput(parts) {
  const text = parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("\n");
  const files = parts
    .filter((part) => part.type === "file")
    .map((part) => ({
      uri: part.url,
      ...(part.filename ? { name: part.filename } : {}),
    }));
  return { text, ...(files.length ? { files } : {}) };
}

async function listModels(client) {
  const [providersResult, modelsResult] = await Promise.all([
    client.v2.provider.list(),
    client.v2.model.list(),
  ]);
  const providers = requireData(providersResult) ?? [];
  const models = requireData(modelsResult) ?? [];
  const byProvider = new Map(providers.map((provider) => [provider.id, {
    id: provider.id,
    name: provider.name,
    models: {},
  }]));
  for (const model of models) {
    const provider = byProvider.get(model.providerID);
    if (!provider || model.enabled === false) continue;
    const inputs = model.capabilities?.input ?? [];
    provider.models[model.id] = {
      name: model.name,
      variants: (model.variants ?? []).map((variant) => variant.id),
      capabilities: {
        attachment: inputs.includes("image") || inputs.includes("pdf"),
        input: {
          image: inputs.includes("image"),
          pdf: inputs.includes("pdf"),
        },
      },
    };
  }
  return { all: [...byProvider.values()].filter((provider) => Object.keys(provider.models).length) };
}

async function configureSession(client, sessionId, request) {
  if (request.agent) requireData(await client.v2.session.switchAgent({ sessionID: sessionId, agent: request.agent }));
  const model = modelRef(request.model, request.variant);
  if (model) requireData(await client.v2.session.switchModel({ sessionID: sessionId, model }));
}

async function expandCommand(client, request) {
  const commands = requireData(await client.v2.command.list()) ?? [];
  const command = commands.find((candidate) => candidate.name === request.command);
  if (!command) throw new Error(`Unknown OpenCode command: /${request.command}`);
  const args = request.arguments ?? "";
  const template = command.template ?? "";
  const text = template.includes("$ARGUMENTS")
    ? template.replaceAll("$ARGUMENTS", args)
    : [template, args].filter(Boolean).join("\n\n");
  return {
    ...request,
    agent: request.agent ?? command.agent,
    model: request.model ?? (command.model ? {
      providerID: command.model.providerID,
      modelID: command.model.id,
    } : undefined),
    variant: request.variant ?? command.model?.variant,
    parts: [{ type: "text", text }, ...(request.parts ?? []).filter((part) => part.type === "file")],
  };
}

async function oneShot(client, request) {
  switch (request.action) {
    case "providers":
      return listModels(client);
    case "commands":
      return (requireData(await client.v2.command.list()) ?? []).map((command) => ({
        name: command.name,
        description: command.description ?? "",
      }));
    case "title": {
      const sessionId = await ensureSession(client, undefined, request);
      requireData(await client.v2.session.prompt({
        sessionID: sessionId,
        prompt: { text: request.prompt },
      }));
      requireData(await client.v2.session.wait({ sessionID: sessionId }));
      const messages = requireData(await client.v2.session.messages({ sessionID: sessionId, order: "desc" })) ?? [];
      const assistant = messages.find((message) => message.type === "assistant");
      return (assistant?.content ?? [])
        .filter((part) => part.type === "text")
        .map((part) => part.text)
        .join("");
    }
    case "fork":
      throw new Error("OpenCode v2 does not provide session fork; replay retained context instead");
    default:
      throw new Error(`Unknown action: ${request.action}`);
  }
}

async function ensureSession(client, sessionId, request = {}) {
  if (sessionId) {
    const existing = await client.v2.session.get({ sessionID: sessionId });
    if (!existing.error) return sessionId;
  }
  const created = await client.v2.session.create({
    location: { directory: process.cwd() },
    ...(request.agent ? { agent: request.agent } : {}),
    ...(request.model ? { model: modelRef(request.model, request.variant) } : {}),
  });
  return requireData(created).id;
}

function automaticPermissionReply(mode) {
  return mode === "build" ? "always" : undefined;
}

function todoPart(sessionId, todos) {
  const id = `nova-todo-${sessionId}`;
  return {
    id,
    sessionID: sessionId,
    messageID: id,
    type: "tool",
    callID: id,
    tool: "todowrite",
    state: { status: "completed", input: { todos } },
  };
}

function eventProperties(event) {
  return event.properties ?? event.data ?? {};
}

function todoPlan(todos) {
  return todos
    .map((todo) => ({ content: todo.content?.trim() ?? "", status: todo.status ?? "pending" }))
    .filter((todo) => todo.content);
}

function promptEventState(event, sessionId, started) {
  const properties = eventProperties(event);
  if (properties.sessionID !== sessionId) return { started, done: false };
  const status = event.type === "session.status" ? properties.status?.type : undefined;
  if (event.type === "session.idle" || status === "idle") return { started, done: started };
  const activity = status === "busy"
    || status === "retry"
    || event.type.startsWith("session.next.")
    || ["todo.updated", "permission.v2.asked", "session.error"].includes(event.type);
  return { started: started || activity, done: false };
}

async function sessionIsIdle(client, sessionId) {
  try {
    const active = requireData(await client.v2.session.active()) ?? {};
    return active[sessionId]?.type !== "running";
  } catch {
    return true;
  }
}

function steerPrompt(client, sessionId, parts) {
  return client.v2.session.prompt({ sessionID: sessionId, prompt: promptInput(parts), delivery: "steer" });
}

function createPromptTracker(reportError) {
  const pending = new Set();
  return {
    start(started) {
      const tracked = Promise.resolve(started)
        .then((result) => {
          if (result.error) reportError(JSON.stringify(result.error));
        }, (error) => reportError(error instanceof Error ? error.message : String(error)))
        .finally(() => pending.delete(tracked));
      pending.add(tracked);
    },
    async wait() {
      while (pending.size) await Promise.all([...pending]);
    },
  };
}

async function startPrompt(client, sessionId, request) {
  const expanded = request.command ? await expandCommand(client, request) : request;
  await configureSession(client, sessionId, expanded);
  return client.v2.session.prompt({
    sessionID: sessionId,
    prompt: promptInput(expanded.parts),
    delivery: expanded.delivery === "steer" ? "steer" : "queue",
  });
}

function toolOutput(properties) {
  return properties.result ?? properties.content ?? properties.structured;
}

function applyV2Event(event, parts) {
  const properties = eventProperties(event);
  const base = { sessionID: properties.sessionID, messageID: properties.assistantMessageID };
  switch (event.type) {
    case "session.next.text.started": {
      const part = { ...base, id: properties.textID, type: "text", text: "" };
      parts.set(part.id, part);
      return part;
    }
    case "session.next.text.delta": {
      const part = parts.get(properties.textID) ?? { ...base, id: properties.textID, type: "text", text: "" };
      part.text += properties.delta ?? "";
      parts.set(part.id, part);
      return part;
    }
    case "session.next.text.ended": {
      const part = parts.get(properties.textID) ?? { ...base, id: properties.textID, type: "text", text: "" };
      part.text = properties.text ?? part.text;
      parts.set(part.id, part);
      return part;
    }
    case "session.next.reasoning.started": {
      const part = { ...base, id: properties.reasoningID, type: "reasoning", text: "" };
      parts.set(part.id, part);
      return part;
    }
    case "session.next.reasoning.delta": {
      const part = parts.get(properties.reasoningID) ?? { ...base, id: properties.reasoningID, type: "reasoning", text: "" };
      part.text += properties.delta ?? "";
      parts.set(part.id, part);
      return part;
    }
    case "session.next.reasoning.ended": {
      const part = parts.get(properties.reasoningID) ?? { ...base, id: properties.reasoningID, type: "reasoning", text: "" };
      part.text = properties.text ?? part.text;
      parts.set(part.id, part);
      return part;
    }
    case "session.next.tool.input.started": {
      const part = { ...base, id: properties.callID, callID: properties.callID, type: "tool", tool: properties.name, state: { status: "pending", input: {} } };
      parts.set(part.id, part);
      return part;
    }
    case "session.next.tool.called": {
      const part = parts.get(properties.callID) ?? { ...base, id: properties.callID, callID: properties.callID, type: "tool" };
      Object.assign(part, { tool: properties.tool, state: { status: "running", input: properties.input ?? {} } });
      parts.set(part.id, part);
      return part;
    }
    case "session.next.tool.progress": {
      const part = parts.get(properties.callID);
      if (!part) return undefined;
      part.state = { ...part.state, status: "running", output: toolOutput(properties) };
      return part;
    }
    case "session.next.tool.success": {
      const part = parts.get(properties.callID);
      if (!part) return undefined;
      part.state = { ...part.state, status: "completed", output: toolOutput(properties) };
      return part;
    }
    case "session.next.tool.failed": {
      const part = parts.get(properties.callID);
      if (!part) return undefined;
      part.state = { ...part.state, status: "error", error: properties.error };
      return part;
    }
    default:
      return undefined;
  }
}

async function runPrompt(client, lines, request) {
  const sessionId = await ensureSession(client, request.sessionId, request);
  const subscription = await client.v2.event.subscribe();
  send({ type: "ready", sessionId });

  let cancelled = false;
  let checkpointUserItemId = request.userItemId;
  let lastAssistantMessageId;
  const parts = new Map();
  const prompts = createPromptTracker((error) => { if (!cancelled) send({ type: "error", error }); });
  const input = (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "permission") {
        const result = await client.v2.session.permission.reply({
          sessionID: sessionId,
          requestID: command.requestId,
          reply: command.reply,
        });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else if (command.action === "cancel") {
        cancelled = true;
        await client.v2.session.interrupt({ sessionID: sessionId });
      } else if (command.action === "prompt") {
        checkpointUserItemId = command.userItemId;
        prompts.start(startPrompt(client, sessionId, {
          ...command,
          model: command.model ?? request.model,
          agent: command.agent ?? request.agent,
          variant: command.variant ?? request.variant,
        }));
      }
    }
  })();

  prompts.start(startPrompt(client, sessionId, request));
  let promptStarted = false;
  for await (const event of subscription.stream) {
    const properties = eventProperties(event);
    if (properties.sessionID !== sessionId) continue;
    const eventState = promptEventState(event, sessionId, promptStarted);
    promptStarted = eventState.started;
    if (properties.assistantMessageID) lastAssistantMessageId = properties.assistantMessageID;
    const part = applyV2Event(event, parts);
    if (part) send({ type: "part", part });
    if (event.type === "todo.updated") {
      const todos = properties.todos ?? [];
      send({ type: "part", part: todoPart(sessionId, todos) });
      send({ type: "plan", plan: todoPlan(todos) });
    } else if (event.type === "permission.v2.asked") {
      const reply = automaticPermissionReply(request.mode);
      if (reply) {
        const result = await client.v2.session.permission.reply({
          sessionID: sessionId,
          requestID: properties.id,
          reply,
        });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else {
        send({ type: "permission", permission: properties });
      }
    } else if (event.type === "session.error") {
      if (cancelled) break;
      send({ type: "error", error: JSON.stringify(properties.error ?? "OpenCode session error") });
      break;
    }
    if (eventState.done) {
      await prompts.wait();
      if (!await sessionIsIdle(client, sessionId)) continue;
      if (lastAssistantMessageId) send({ type: "checkpoint", sessionId, position: lastAssistantMessageId, userItemId: checkpointUserItemId });
      send({ type: "done" });
      break;
    }
  }
  void input;
}

async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let opencode;
  try {
    const request = await readRequest(lines);
    opencode = await createOpencode({ hostname: "127.0.0.1", port: 0, timeout: 10_000 });
    const { client } = opencode;
    if (request.action === "prompt") await runPrompt(client, lines, request);
    else send({ ok: true, data: await oneShot(client, request) });
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
    opencode?.server.close();
  }
}

if (process.env.NOVA_OPENCODE_BRIDGE_TEST !== "1") void main();

export { applyV2Event, automaticPermissionReply, createPromptTracker, ensureSession, eventProperties, listModels, promptEventState, sessionIsIdle, startPrompt, steerPrompt, todoPart, todoPlan };
