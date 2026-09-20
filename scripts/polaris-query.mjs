/** Shared JS normalization. The native boundary independently applies the same contract. */
export function normalizePolarisArgs(params = {}) {
  if (!params || typeof params !== 'object' || Array.isArray(params)) throw new TypeError('polaris parameters must be an object');
  const list = (value, max, fold = true) => {
    const seen = new Set();
    return (Array.isArray(value) ? value : typeof value === 'string' ? [value] : [])
      .filter(x => typeof x === 'string').map(x => x.trim())
      .filter(x => x && Buffer.byteLength(x) <= 4096 && !seen.has(fold ? x.toLowerCase() : x) && seen.add(fold ? x.toLowerCase() : x)).slice(0, max);
  };
  const identifier = s => s.length <= 200 && /^[A-Za-z_$][A-Za-z0-9_$:.-]*$/.test(s);
  const query = [...String(params.query ?? '').trim()].slice(0, 1024).join('');
  let task = [...String(params.task ?? '').trim()].slice(0, 1024).join('');
  let keywords = list(params.keywords, 5);
  if (query) {
    if (identifier(query)) { if (!keywords.some(x => x.toLowerCase() === query.toLowerCase())) keywords.push(query); }
    else if (!task) task = query;
  }
  if (!task) task = keywords.filter(x => !identifier(x)).join(' ');
  task = [...task].slice(0, 1024).join('');
  keywords = keywords.filter(identifier).slice(0, 5);
  return { ...params, keywords, task, files: [...new Set(list(params.files, 6, false).map(f => f.replaceAll("\\", "/")))] };
}

export const POLARIS_DESCRIPTION = "代码位置未知或需要跨文件上下文时，先调用一次 Polaris。task/query 提供完整改动目标、对象和约束；不要拆成多轮猜函数名，已知符号或文件分别放 keywords/files。内部按问题筛选声明，返回当前源码的主实现、必要辅助函数、类型/常量及有界调用关系，不等待全仓索引。把返回的原文行段直接作为编辑上下文：足够开始修改时立即修改并验证，不要再 read/grep 已返回范围，也不要为阅读所有候选而反复检索。只有实际阻塞改动的缺失定义或源码已变化时，才针对那个缺口补读；next_reads 是截断或未覆盖的候选清单，不要求全部读取。CANDIDATES 不证明根因，PARTIAL 必须结合任务判断，不能假装缺失代码已验证。默认不调用向量模型、不上传源码。";
