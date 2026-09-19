"""Three production engines, equal context budgets, no preparatory indexing or answer-model calls.
Cold means a fresh process and empty Nova cache, NOT flushed OS caches. Read deficits
are a labelled lower-bound proxy, never a measured LLM follow-up count.
"""
import argparse, hashlib, importlib.util, json, os, pathlib, statistics, tempfile, time
P=pathlib.Path
spec=importlib.util.spec_from_file_location('ab',P('scripts/polaris-ab.py'));ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
p=argparse.ArgumentParser();p.add_argument('--previous',type=P,required=True);p.add_argument('--candidate',type=P,required=True);p.add_argument('--corpus',type=P,required=True);p.add_argument('--replay-corpus',type=P);p.add_argument('--replay-report',type=P);p.add_argument('--out',type=P,default=P('validation/fastcontext'));args=p.parse_args();out=args.out;out.mkdir(parents=True,exist_ok=True)
cases=json.loads(P('bench/polaris-ab/cases.json').read_text())['cases']
for c in cases:c.update(group='regression',params={'query':c['query'],'maxBytes':12000},root=str(args.corpus.resolve()))
def gold(file,symbol,*needles):return {'file':file,'symbol':symbol,'needles':list(needles)}
# Nonempty support labels are fixed before this run. They test connected evidence,
# not just an entry point or a mention in output metadata.
closure=[
 {'id':'closure-update-proxy','query':'检查更新遇到禁止访问，判断重试条件后怎样真正切换无代理客户端再发送请求？','primary':[gold('src-tauri/src/updater.rs','request_update_release','should_retry_update_without_proxy','response')],'support':[gold('src-tauri/src/updater.rs','should_retry_update_without_proxy','FORBIDDEN','proxy_configured')]},
 {'id':'closure-restore-window','query':'升级后重新启动，怎样从恢复标记还原窗口并处理隐藏、最小化和焦点？','primary':[gold('src-tauri/src/updater.rs','restore_window_on_launch','take_restore_window','apply_window_state')],'support':[gold('src-tauri/src/updater.rs','apply_window_state','ws.visible','ws.minimized'),gold('src-tauri/src/updater.rs','take_restore_window','marker.thread_id','remove_file')]},
 {'id':'closure-save-window','query':'窗口拖动后怎样延迟保存位置，并防止旧的延迟任务覆盖新的窗口布局？','primary':[gold('src-tauri/src/updater.rs','remember_window_layout','WINDOW_LAYOUT_REVISION','save_window_layout')],'support':[gold('src-tauri/src/updater.rs','save_window_layout','window-layout.json','GetWindowPlacement')]},
 {'id':'closure-desktop-keys','query':'桌面快捷键怎样解析组合按键并发送，按键发送失败后如何释放？','primary':[gold('src-tauri/src/jianlai.rs','input','release_error','Direction::Release')],'support':[gold('src-tauri/src/jianlai.rs','keys','组合键最多4键')]},
]
for c in closure:
 c.update(group='closure',kind='natural',split='new-contract',params={'task':c['query'],'maxBytes':12000},root=str(args.corpus.resolve()));cases.append(c)
if args.replay_corpus and args.replay_report:
 replay=json.loads(args.replay_report.read_text())
 labels={'18be7005-4': ['co_changed_files','fast_context_run'],'719d5639-17':['fast_context','fast_context_run'],'e09fd327-8':['fast_context_run','build_index','fast_context_no_index']}
 for episode in replay['runs']:
  if episode['repeat']!=0:continue
  step=next(s for s in episode['steps'] if s.get('tool')=='polaris');params={k:v for k,v in step['args'].items() if k in ('task','keywords','files')};params['maxBytes']=12000
  symbols=next((v for k,v in labels.items() if episode['id'].startswith(k)),[])
  cases.append({'id':episode['id'],'group':'replay-entry','kind':'natural','split':'replay','root':str(args.replay_corpus.resolve()),'params':params,'primary':[gold('src-tauri/src/nova_tools_native/context.rs',s) for s in symbols],'support':[],'unscored':not bool(symbols)})
# Validate every needle against frozen source, outside the retrieval corpus.
for case in cases:
 for g in case['primary']+case['support']:
  text=(P(case['root'])/g['file']).read_text();assert all(n in text for n in g['needles']),(case['id'],g)
(out/'labels-before-run.json').write_text(json.dumps(cases,ensure_ascii=False,indent=2))
env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')}
engines={'original':(args.previous,'baseline'),'pr11':(args.previous,'candidate'),'demand':(args.candidate,'candidate')}
report={'answerModelCalls':0,'budgetBytes':12000,'cases':cases,'rows':[],'binaries':{k:hashlib.sha256(v[0].read_bytes()).hexdigest() for k,v in engines.items()},'method':__doc__,'status':'running'}
def save():(out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
def measure(case,arm,engine,phase,round,start=None):
 response=engine.ask({'root':case['root'],'mode':engines[arm][1],'params':case['params']})
 wall=(time.perf_counter()-start)*1000 if start else response['wallMs']
 text=response.get('result',{}).get('text','');sections=ab.sections(text)
 metric=ab.assess(case,text)
 missing=[g for g in case['primary']+case['support'] if not any(ab.matched(s,g) for s in sections)]
 # primary labels are alternatives for replay-entry; closure primary and supports are required.
 if case['group']=='replay-entry' and metric['coreBody']:missing=[]
 metric.update(requiredUnits=len(case['primary'])+len(case['support']),missingUnits=len(missing),additionalFileReadsLowerBound=len({g['file'] for g in missing}),allRequiredEvidence=(not missing and bool(case['primary'])))
 meta={}
 for line in text.splitlines():
  if line.startswith('# retrieval: '):meta=json.loads(line[len('# retrieval: '):]);break
 row={'id':case['id'],'group':case['group'],'kind':case['kind'],'arm':arm,'phase':phase,'round':round,'ok':response['ok'],'wallMs':wall,'engineMs':response['ms'],'metrics':metric,'retrieval':meta,'unscored':case.get('unscored',False)}
 if not response['ok']:row['error']=response.get('error')
 report['rows'].append(row);target=out/'raw'/case['id'];target.mkdir(parents=True,exist_ok=True);(target/f'{arm}-{phase}-{round}.txt').write_text(text);save()
 print(json.dumps({k:row[k] for k in ['id','arm','phase','wallMs','metrics']},ensure_ascii=False),flush=True)
with tempfile.TemporaryDirectory(prefix='polaris-query-first-') as cache:
 # Each unique query starts without a process, AST index, query cache or warm-up.
 for i,case in enumerate(cases):
  arms=list(engines);offset=i%len(arms);arms=arms[offset:]+arms[:offset]
  for arm in arms:
   start=time.perf_counter();engine=ab.Engine(engines[arm][0],{**env,'NOVA_DATA_DIR':str(P(cache)/f'cold-{i}-{arm}')},out/f'cold-{i}-{arm}.log')
   try:measure(case,arm,engine,'cold',0,start)
   finally:engine.close()
 # Distinct-query first pass remains separate from repeated-query warm measurements.
 live={a:ab.Engine(b,{**env,'NOVA_DATA_DIR':str(P(cache)/a)},out/f'{a}-resident.log') for a,(b,_) in engines.items()}
 try:
  for round in range(3):
   for i,case in enumerate(cases if round%2==0 else list(reversed(cases))):
    arms=list(engines);offset=(round+i)%3;arms=arms[offset:]+arms[:offset]
    for arm in arms:measure(case,arm,live[arm],'resident-first' if round==0 else 'warm',round)
 finally:
  for e in live.values():e.close()
summary={}
for group in ['regression','closure','replay-entry']:
 summary[group]={}
 for arm in engines:
  summary[group][arm]={}
  for phase in ['cold','resident-first','warm']:
   rows=[r for r in report['rows'] if r['group']==group and r['arm']==arm and r['phase']==phase and r['kind']=='natural'];scored=[r for r in rows if not r['unscored']]
   if not rows:continue
   times=sorted(r['wallMs'] for r in rows)
   summary[group][arm][phase]={'samples':len(rows),'scored':len(scored),'p50Ms':statistics.median(times),'p95Ms':times[min(len(times)-1,int(len(times)*.95))],'errors':sum(not r['ok'] for r in rows),'top1':sum(r['metrics']['top1'] for r in scored),'coreBody':sum(r['metrics']['coreBody'] for r in scored),'allRequired':sum(r['metrics']['allRequiredEvidence'] for r in scored),'readDeficit':sum(r['metrics']['additionalFileReadsLowerBound'] for r in scored),'medianParsedFiles':statistics.median(r['retrieval'].get('demand',{}).get('parsedFiles',r['retrieval'].get('files',0)) for r in rows),'medianBytes':statistics.median(r['metrics']['bytes'] for r in rows)}
report['summary']=summary;report['status']='completed';save();print(json.dumps(summary,ensure_ascii=False,indent=2))
# Correctness failures fail the run. Quality comparisons remain visible, not tuned away.
assert all(r['ok'] for r in report['rows']), 'one or more production queries failed'
assert all(r['metrics']['bytes']<=12000 for r in report['rows'] if r['arm']=='demand'),'context budget exceeded'
