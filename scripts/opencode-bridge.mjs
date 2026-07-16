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

async function runPrompt(client, lines, request) {
  const sessionId = await ensureSession(client, request.sessionId);
  const subscription = await client.event.subscribe();
  send({ type: "ready", sessionId });

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
        await client.session.abort({ sessionID: sessionId });
      }
    }
  })();

  const body = {};
  if (request.model) body.model = request.model;
  if (request.agent) body.agent = request.agent;
  if (request.variant) body.variant = request.variant;
  let started;
  if (request.command) {
    started = client.session.command({
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
  } else {
    started = client.session.promptAsync({
      sessionID: sessionId,
      ...body,
      parts: request.parts,
    });
  }
  started.then((result) => {
    if (result.error) send({ type: "error", error: JSON.stringify(result.error) });
  }).catch((error) => send({ type: "error", error: error instanceof Error ? error.message : String(error) }));

  const assistantMessages = new Set();
  for await (const event of subscription.stream) {
    const properties = event.properties ?? {};
    if (properties.sessionID !== sessionId) continue;
    if (event.type === "message.updated") {
      if (properties.info?.role === "assistant") assistantMessages.add(properties.info.id);
      continue;
    }
    if (event.type === "message.part.updated") {
      if (assistantMessages.has(properties.part?.messageID)) send({ type: "part", part: properties.part });
      continue;
    }
    if (event.type === "permission.asked" || event.type === "permission.v2.asked") {
      send({ type: "permission", permission: properties });
      continue;
    }
    if (event.type === "session.error") {
      send({ type: "error", error: JSON.stringify(properties.error ?? "OpenCode session error") });
      break;
    }
    if (event.type === "session.idle") {
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

void main();
