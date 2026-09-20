"""Replace only Polaris tool descriptions; never modify Reasonix or its policies."""
from pathlib import Path
import hashlib,re,json
text='代码位置未知或需要跨文件上下文时，先调用一次 Polaris。task/query 提供完整改动目标、对象和约束；不要拆成多轮猜函数名，已知符号或文件分别放 keywords/files。内部按问题筛选声明，返回当前源码的主实现、必要辅助函数、类型/常量及有界调用关系，不等待全仓索引。把返回的原文行段直接作为编辑上下文：足够开始修改时立即修改并验证，不要再 read/grep 已返回范围，也不要为阅读所有候选而反复检索。只有实际阻塞改动的缺失定义或源码已变化时，才针对那个缺口补读；next_reads 是截断或未覆盖的候选清单，不要求全部读取。CANDIDATES 不证明根因，PARTIAL 必须结合任务判断，不能假装缺失代码已验证。默认不调用向量模型、不上传源码。'
for name,sha,pattern in [
 ('scripts/polaris-query.mjs','68ae4b7692ca873813e79d5e06aff51a8d95ac19',r'export const POLARIS_DESCRIPTION = .*?;\s*$'),
 ('scripts/ctx-core.mjs','1cb5ce9617458cb715bf4b5203c0b21d423c5196',r'export const POLARIS_DESCRIPTION =\s*.*?;\s*$'),
 ('src-tauri/src/lyra/tools.rs','4b2c3a98b4b6b19a5e6bf0621149560ad1c8ba91',r'const POLARIS_DESCRIPTION: &str = .*?;'),
]:
 p=Path(name);b=p.read_bytes().replace(b'\r\n',b'\n');assert hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()==sha,name
 s=b.decode();prefix='const POLARIS_DESCRIPTION: &str = ' if name.endswith('.rs') else 'export const POLARIS_DESCRIPTION = '
 s,n=re.subn(pattern,lambda _:prefix+json.dumps(text,ensure_ascii=False)+';\n',s,flags=re.S);assert n==1,name;p.write_text(s)
print('Updated only three Polaris descriptions. No prompt governance or Reasonix changes.')
