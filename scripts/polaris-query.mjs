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

export const POLARIS_DESCRIPTION = "代码位置未知或需跨文件分析时调用一次。query/task 可直接提供完整中文行为描述，不要猜造函数名；已知符号用 keywords，已知文件用 files。自然语言内部进行函数级词法/概念召回，显式配置的本地语义模型可补充向量召回和精排，默认不会上传代码。结果提供经过当前源码校验的主体、相关引用、coverage 与 next_reads；CANDIDATES 不是根因已确认，PARTIAL/缺口必须说明。已返回正文不要重复读取；关键证据不足时再按缺口读取，不保证任意问题一次命中。";
