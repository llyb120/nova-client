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

async function oneShot(client, request) {
  switch (request.action) {
    case "providers": {
      const providers = (await client.provider.list()).data;
      const connected = new Set(providers.connected);
      return {
        all: providers.all
          .filter((provider) => connected.has(provider.id))
          .map((provider) => ({
            id: provider.id,
            name: provider.name,
            models: Object.fromEntries(
              Object.entries(provider.models).map(([id, model]) => [
                id,
                {
                  name: model.name,
                  variants: Object.keys(model.variants ?? {}),
                  capabilities: {
                    attachment: model.capabilities.attachment,
                    input: {
                      image: model.capabilities.input.image,
                      pdf: model.capabilities.input.pdf,
                    },
                  },
                },
              ]),
            ),
          })),
      };
    }
    case "commands":
      return (await client.command.list()).data.map((command) => ({
        name: command.name,
        description: command.description ?? "",
      }));
    case "title": {
      const sessionId = await ensureSession(client);
      const body = { parts: [{ type: "text", text: request.prompt }] };
      if (request.model) body.model = request.model;
      if (request.variant) body.variant = request.variant;
      const result = await client.session.prompt({ sessionID: sessionId, ...body });
      if (result.error) throw new Error(JSON.stringify(result.error));
      return result.data.parts
        .filter((part) => part.type === "text")
        .map((part) => part.text)
        .join("");
    }
    case "fork": {
      const result = await client.session.fork({
        sessionID: request.sessionId,
        messageID: request.position,
      });
      if (result.error) throw new Error(JSON.stringify(result.error));
      return result.data.id;
    }
    default:
      throw new Error(`Unknown action: ${request.action}`);
  }
}

async function ensureSession(client, sessionId) {
  if (sessionId) {
    const existing = await client.session.get({ sessionID: sessionId });
    if (!existing.error) return sessionId;
  }
  const created = await client.session.create();
  if (created.error) throw new Error(JSON.stringify(created.error));
  return created.data.id;
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

function promptEventState(event, sessionId, started) {
  const properties = event.properties ?? {};
  if (properties.sessionID !== sessionId) return { started, done: false };
  const status = event.type === "session.status" ? properties.status?.type : undefined;
  if (event.type === "session.idle" || status === "idle") {
    return { started, done: started };
  }
  const activity = status === "busy"
    || status === "retry"
    || [
      "message.updated",
      "message.part.updated",
      "todo.updated",
      "permission.asked",
      "permission.v2.asked",
      "session.error",
    ].includes(event.type);
  return { started: started || activity, done: false };
}

function startPrompt(client, sessionId, request) {
  if (request.command) {
    return client.session.command({
      sessionID: sessionId,
      command: request.command,
      arguments: request.arguments ?? "",
      parts: request.parts.filter((part) => part.type === "file"),
      model: request.model
        ? `${request.model.providerID}/${request.model.modelID}`
        : undefined,
      agent: request.agent,
      variant: request.variant,
    });
  }
  const body = {};
  if (request.model) body.model = request.model;
  if (request.agent) body.agent = request.agent;
  if (request.variant) body.variant = request.variant;
  return client.session.promptAsync({
    sessionID: sessionId,
    ...body,
    parts: request.parts,
  });
}

async function runPrompt(client, lines, request) {
  const sessionId = await ensureSession(client, request.sessionId);
  const subscription = await client.event.subscribe();
  send({ type: "ready", sessionId });

  let cancelled = false;
  let checkpointUserItemId = request.userItemId;
  const reportStarted = (started) => {
    started.then((result) => {
      if (result.error && !cancelled) send({ type: "error", error: JSON.stringify(result.error) });
    }).catch((error) => {
      if (!cancelled) send({ type: "error", error: error instanceof Error ? error.message : String(error) });
    });
  };
  const input = (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "permission") {
        const result = await client.permission.reply({
          requestID: command.requestId,
          reply: command.reply,
        });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else if (command.action === "cancel") {
        cancelled = true;
        await client.session.abort({ sessionID: sessionId });
      } else if (command.action === "prompt") {
        checkpointUserItemId = command.userItemId;
        reportStarted(startPrompt(client, sessionId, {
          ...command,
          model: command.model ?? request.model,
          agent: command.agent ?? request.agent,
          variant: command.variant ?? request.variant,
        }));
      }
    }
  })();

  reportStarted(startPrompt(client, sessionId, request));

  const assistantMessages = new Set();
  const pendingParts = new Map();
  let promptStarted = false;
  for await (const event of subscription.stream) {
    const properties = event.properties ?? {};
    if (properties.sessionID !== sessionId) continue;
    const eventState = promptEventState(event, sessionId, promptStarted);
    promptStarted = eventState.started;
    if (event.type === "message.updated") {
      if (properties.info?.role === "assistant") {
        assistantMessages.add(properties.info.id);
        for (const part of pendingParts.get(properties.info.id)?.values() ?? []) send({ type: "part", part });
        pendingParts.delete(properties.info.id);
      }
      continue;
    }
    if (event.type === "message.part.updated") {
      const part = properties.part;
      if (assistantMessages.has(part?.messageID)) {
        send({ type: "part", part });
      } else if (part?.messageID && part?.id) {
        const parts = pendingParts.get(part.messageID) ?? new Map();
        parts.set(part.id, part);
        pendingParts.set(part.messageID, parts);
      }
      continue;
    }
    if (event.type === "todo.updated") {
      send({ type: "part", part: todoPart(sessionId, properties.todos ?? []) });
      continue;
    }
    if (event.type === "permission.asked" || event.type === "permission.v2.asked") {
      const reply = automaticPermissionReply(request.mode);
      if (reply) {
        const result = await client.permission.reply({ requestID: properties.id, reply });
        if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
      } else {
        send({ type: "permission", permission: properties });
      }
      continue;
    }
    if (event.type === "session.error") {
      if (cancelled) break;
      send({ type: "error", error: JSON.stringify(properties.error ?? "OpenCode session error") });
      break;
    }
    if (eventState.done) {
      const position = [...assistantMessages].at(-1);
      if (position) send({ type: "checkpoint", sessionId, position, userItemId: checkpointUserItemId });
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
    if (request.action === "prompt") {
      await runPrompt(client, lines, request);
    } else {
      send({ ok: true, data: await oneShot(client, request) });
    }
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
    opencode?.server.close();
  }
}

if (process.env.NOVA_OPENCODE_BRIDGE_TEST !== "1") void main();

export { automaticPermissionReply, promptEventState, startPrompt, todoPart };
