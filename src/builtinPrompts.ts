/**
 * Nova 内置「通用指令」的提示词模板。
 *
 * 与 per-agent 的 skill（每个后端各有独立 skills 目录，需逐个配置）不同，这些模板在
 * 客户端层展开：Nova 把用户的斜杠命令翻译成一段自包含的提示词（内嵌目标配置的
 * schema 与操作步骤），再投递给当前会话的 agent 后端，因此对任意后端通用。
 */

/**
 * 展开 `/plan <目标>`：在 Build 模式下静默注入「先出计划、少追问」指令。
 * 不再切入 Plan 模式；只是把斜杠命令翻译成一段自包含提示词。
 */
export function buildPlanPrompt(goal: string): string {
  return `请针对以下目标给出完整实施计划。直接规划并写出可执行步骤，不要先就方案细节反复追问澄清；有不确定处请在计划中写明假设后继续。

目标：
${goal}`;
}

/**
 * 展开 `/setup <目标>`：指导 agent 把一个模型 / provider 接入 Vega（alkaid 后端）。
 * 提示词内嵌 ~/.nova/alkaid/config.jsonc 的真实结构与约束，agent 照此编辑即可。
 */
export function buildIntegrateModelPrompt(goal: string): string {
  return `请把模型「${goal}」接入 Vega（alkaid 后端）。

## 目标配置文件
~/.nova/alkaid/config.jsonc —— OpenCode 风格的 JSONC，允许注释与尾逗号（加载器会自动剥离）。

## 配置结构
\`\`\`jsonc
{
  "model": "<providerId>/<modelId>",        // 默认选中的模型
  "provider": {
    "<providerId>": {
      "npm": "@ai-sdk/openai-compatible",   // Anthropic 用含 anthropic 的包；Google 用含 google 的包；api 可省略（由 npm 推导）
      "options": { "baseURL": "https://.../v1", "apiKey": "{env:环境变量名}" },
      "models": {
        "<modelId>": { "reasoning": true, "limit": { "context": 131072, "output": 8192 } }
      }
    }
  }
}
\`\`\`

## 步骤
1. 先读取现有 ~/.nova/alkaid/config.jsonc；不存在则新建为 \`{ "provider": {} }\`。
2. 确定「${goal}」的接入信息：baseURL、精确模型 id、是否推理模型、上下文/输出窗口、API key 的环境变量名。
   - **凡是不能从当前工作区或我给出的信息中确定的参数（尤其是上下文/输出窗口、模型特有能力或配置），必须联网搜索官方文档确认，禁止凭记忆臆测。** 搜索后仍无法确认的，列出待确认项向我询问，不要填入猜测值。
   - 常见示例：DashScope（通义千问，OpenAI 兼容）baseURL 为 https://dashscope.aliyuncs.com/compatible-mode/v1，环境变量 DASHSCOPE_API_KEY。
3. 在 provider 下新增或更新一个 provider 块；如需设为默认，把顶层 model 更新为 \`<providerId>/<modelId>\`。
4. apiKey 必须写成 \`{env:环境变量名}\`，严禁写入明文密钥。
5. 完成后告诉我：需在启动 Nova 的 shell 注入该环境变量（Windows PowerShell 用 \`$env:变量名="..."\`），并在桌面端重启 Vega 以重新探测模型列表。

## 验证
改动后若当前工作区就是 Nova 仓库，可冒烟测试：\`npm run alkaid -- --prompt "请只回复 Vega OK"\`；否则提示我在桌面端重启 Vega 后确认新模型出现在模型列表。`;
}
