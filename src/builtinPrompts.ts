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
 * 展开 `/easy <目标>`：用于目标明确、改法直接的小改动。
 * 这是一次性提示词约束，不改变会话模式，也不把 `/easy` 交给后端解析。
 */
export function buildEasyPrompt(goal: string): string {
  return `请以“快速小改动”方式完成下面的任务。这个任务应当是目标明确、影响范围有限、修改方式显而易见的改动；保持改动聚焦，不要借机重构或扩大范围。

执行要求：
- 先读取完成任务所必需的直接相关代码和上下文，然后直接实施最小、清晰的修改；不要为显而易见的细节反复追问或展开复杂方案讨论。
- 不要做复杂的验证或全面回归：修改后只保留项目已有的最低成本编译 / 类型检查（或等价的基本构建校验）。
- 不要运行任何测试，也不要新增、修改或专门编写测试；不要运行 lint、集成测试、端到端测试、性能检查或其他额外验证。这里的“基本编译 / 类型检查”是唯一例外。
- 仍须保证基本的语法、类型和明显的逻辑正确性，检查最终 diff；不要为了省步骤而忽略会直接暴露编译错误的基本检查。
- 如果你发现任务实际上并不简单、需求存在阻塞性不确定，或基本检查之外必须进行复杂验证才能安全修改，请停止扩大执行范围，说明原因并向我确认，不要擅自把复杂任务当作快速小改动完成。

完成后用简短文字说明改了什么，以及执行的基本编译 / 类型检查结果；若未执行检查，说明原因。

任务：
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
      "options": { "baseURL": "https://.../v1", "apiKey": "sk-..." },
      "models": {
        "<modelId>": { "reasoning": true, "limit": { "context": 131072, "output": 8192 } }
      }
    }
  }
}
\`\`\`

## 步骤
1. 先读取现有 ~/.nova/alkaid/config.jsonc；不存在则新建为 \`{ "provider": {} }\`。
2. 确定「${goal}」的接入信息：baseURL、精确模型 id、是否推理模型、上下文/输出窗口、API key。
   - **凡是不能从当前工作区或我给出的信息中确定的参数（尤其是上下文/输出窗口、模型特有能力或配置），必须联网搜索官方文档确认，禁止凭记忆臆测。** 搜索后仍无法确认的，列出待确认项向我询问，不要填入猜测值。
   - 常见示例：DashScope（通义千问，OpenAI 兼容）baseURL 为 https://dashscope.aliyuncs.com/compatible-mode/v1。
3. 在 provider 下新增或更新一个 provider 块；如需设为默认，把顶层 model 更新为 \`<providerId>/<modelId>\`。
4. apiKey 直接明文写入配置文件即可。
5. 完成后告诉我：本次 /setup 轮次结束后 Nova 会自动重载 Vega 配置并刷新模型列表，无需手动重启；如果没有看到新模型，再提示我在设置·模型后端中点击「刷新配置」。

## 验证
改动后若当前工作区就是 Nova 仓库，可冒烟测试：\`npm run alkaid -- --prompt "请只回复 Vega OK"\`；否则提示我等待本次 setup 完成后确认新模型出现在模型列表中。`;
}

/**
 * 展开「时光笔记」：新建一个训练会话，让它自己读取世界线材料文件逐级分析、
 * 必要时反问用户，最后沉淀为一个可复用的 skill。提示词只描述规则，
 * 不内嵌任何世界线内容；具体学什么由用户在会话中自行补充。
 */
export function buildTimeNotesPrompt(
  skillName: string,
  digestPath: string,
  skillsDir: string,
): string {
  return `【时光笔记】${skillName}
请把我这次会话的历史经验沉淀为一个可复用的 skill，skill 名称为 ${skillName}。

## 材料获取
- 材料文件：${digestPath}。请自己用读文件工具阅读，不要让我粘贴内容。
- 材料是本次会话的完整分支历史：同一个任务的不同做法从同一处分叉，各自走向不同结局。
- 材料不足或语义不明时，直接向我提问。

## 分析规则
1. 通读分支树：每条分支从哪里分叉、做了什么、结局如何。
2. 对未标记结局的分支，根据轨迹自行推断成功 / 失败 / 无果；无法判断就问我。
3. 对比分叉点两侧的做法，找出导致成败差异的具体决策。
4. 提炼可复用的经验：成功做法、失败陷阱、硬性规则。
5. 任何拿不准的地方，先提问、等我回答后再继续。

## 产出要求
产出一个 skill 目录，不一定只有一份 SKILL.md：
- 必须有 SKILL.md：YAML frontmatter（name 为 ${skillName}，description 写明何时该用这个 skill）+ Markdown 正文。
- 正文按「成功做法」「失败陷阱」「硬性规则」组织，每条规则注明来源依据。
- 如果经验里有适合固化成脚本、模板或参考资料的部分（比如固定的检查步骤、回退脚本），可以一并写成目录里的辅助文件（如 scripts/、templates/），并在 SKILL.md 里说明何时、怎么使用它们。
- 不要为了凑数而加文件；没有值得固化的脚本就只留 SKILL.md。

写完整个目录结构和所有文件内容后发我确认；我确认后再写入 ${skillsDir}/${skillName}/。
若你没有权限写入该目录，就把全部文件内容留在回复里并告诉我。`;
}
