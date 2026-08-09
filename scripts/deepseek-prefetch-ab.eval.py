#!/usr/bin/env python3
import json,os,shutil,subprocess,tempfile,time
from pathlib import Path
REPO=Path('/home/agentuser/projects/nova-client'); BIN=REPO/'src-tauri/target/debug/nova'; SRC=Path.home()/'.nova/alkaid'; MODEL='opencode/deepseek-v4-flash/variant/high'
CASES=[
('P1','用且只用一次 fast_context，keywords 取 fast_context_run、source_cache_contains、prefetch_model，分析新的预取模型与 source cache 链路。只基于工具输出回答，不再调用其它工具。'),
('P2','用且只用一次 fast_context，keywords 取 PendingPrefetchTrace、record_prefetch_demand、prefetch_update_pair，分析预取在线反馈如何形成未来三次调用标签。只基于工具输出回答，不再调用其它工具。'),
('P3','用且只用一次 fast_context，keywords 取 SOURCE_LOADING、Condvar、source，分析 single-flight 如何避免预取与需求双读。只基于工具输出回答，不再调用其它工具。')]
def root(arm):
 r=Path(tempfile.mkdtemp(prefix='deepseek-prefetch-ab-'));a=r/'alkaid';a.mkdir();shutil.copy2(SRC/'config.jsonc',a/'config.jsonc');return r
def run(arm,prefetch):
 r=root(arm);env=os.environ.copy();env['NOVA_DATA_DIR']=str(r);env['NOVA_CTX_PREFETCH']='1' if prefetch else '0';env['NOVA_CONTEXT_LEARNING']='1';env['LYRA_SPECULATE']='off';rows=[]
 try:
  for i,(cid,prompt) in enumerate(CASES):
   req={'action':'prompt','cwd':str(REPO),'mode':'plan','model':MODEL,'sessionId':f'prefetch-{arm}-{i}','parts':[{'type':'text','text':prompt}]};t=time.time();p=subprocess.Popen([str(BIN),'lyra'],cwd=REPO,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);out,err=p.communicate(json.dumps(req,ensure_ascii=False)+'\n',timeout=600);events=[]
   for line in out.splitlines():
    try:events.append(json.loads(line))
    except:pass
   done=next((e for e in reversed(events) if e.get('type')=='done'),{});items=[e.get('item',{}) for e in events if e.get('type')=='item'];tools={x.get('id'):x for x in items if x.get('tool')};rows.append({'id':cid,'wallMs':round((time.time()-t)*1000),'usage':done.get('usage'),'tools':[x.get('tool') for x in tools.values()],'rounds':sum(e.get('type')=='timing' and e.get('phase')=='provider_turn' for e in events),'error':next((e for e in events if e.get('ok') is False),None)})
 finally:shutil.rmtree(r,ignore_errors=True)
 return rows
A=run('A-off',False);B=run('B-on',True)
def total(rows):
 z={k:0 for k in ['wallMs','input','output','cacheRead','cacheWrite','tools','rounds']}
 for x in rows:
  z['wallMs']+=x['wallMs'];z['tools']+=len(x['tools']);z['rounds']+=x['rounds'];u=x.get('usage') or {}
  for k in ['input','output','cacheRead','cacheWrite']:z[k]+=u.get(k,0)
 return z
rep={'model':MODEL,'cases':[{**{'id':CASES[i][0]},'A':A[i],'B':B[i]} for i in range(len(CASES))],'A':total(A),'B':total(B)};rep['delta']={k:rep['B'][k]-rep['A'][k] for k in rep['A']};p=REPO/'scripts/deepseek-prefetch-ab.report.json';p.write_text(json.dumps(rep,ensure_ascii=False,indent=2));print(json.dumps({'report':str(p),**rep},ensure_ascii=False,indent=2))
