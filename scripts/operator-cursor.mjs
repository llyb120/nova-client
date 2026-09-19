// A tool-less single-decision executor. No Reasonix, parent history, user/project
// MCP configuration or native Task tool is loaded into this agent.

export async function cursorDecision(request, sdk, parseModel) {
  if (typeof request.model !== 'string' || !request.model.trim() || request.model === '__cursor_auto__') {
    throw new Error('Operator needs a resolved parent model; Auto cannot guarantee the same model');
  }
  sdk ??= (await import('@cursor/sdk')).Agent;
  parseModel ??= (await import('./cursor-bridge-common.mjs')).modelSelection;
  const model = parseModel(request.model);
  const agent = await sdk.create({
    apiKey: process.env.CURSOR_API_KEY,
    model,
    systemPrompt: request.system,
    tools: [],
    disallowedTools: ['mcp', 'task'],
    mcpServers: {},
    local: { cwd: request.cwd, settingSources: [], customTools: {}, enableAgentRetries: false },
  });
  try {
    const run = await agent.send({ text: JSON.stringify(request.context), images: request.images ?? [] });
    const result = await run.wait();
    if (result.status === 'error') throw new Error(result.error?.message || 'Cursor decision failed');
    if (!result.result || result.result.length > 48_000) throw new Error('Missing or oversized Cursor decision');
    return result.result;
  } finally {
    await agent.close();
  }
}

export async function runOperatorDecision() {
  const { createInterface } = await import('node:readline');
  const lines = createInterface({ input: process.stdin });
  try {
    for await (const line of lines) {
      try {
        const request = JSON.parse(line);
        if (request.action !== 'operator_decide') throw new Error('Expected operator_decide');
        const text = await cursorDecision(request);
        process.stdout.write(`${JSON.stringify({ operatorDecision: true, ok: true, text })}\n`);
      } catch (error) {
        process.stdout.write(`${JSON.stringify({ operatorDecision: true, ok: false, error: error.message })}\n`);
      }
      break;
    }
  } finally { lines.close(); }
}
