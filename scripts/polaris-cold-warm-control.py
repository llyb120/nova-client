"""No model calls. Reuse actual model-generated queries from the sanitized real-prefix replay.
Keep both engines, source corpus and maxBytes identical; report cold and resident separately.
"""
import json,os,pathlib,subprocess,time,hashlib,statistics
source=pathlib.Path(os.environ['POLARIS_REPLAY_REPORT']);binary=pathlib.Path(os.environ['POLARIS_REPLAY_BINARY']).resolve();root=pathlib.Path(os.environ['OPERATOR_REPLAY_CORPUS']).resolve();out=pathlib.Path('validation/polaris-control');out.mkdir(parents=True,exist_ok=True)
original=json.loads(source.read_text());queries=[]
for episode in original['runs']:
 if episode['repeat']!=0:continue
 step=next(s for s in episode['steps'] if s.get('tool')=='polaris')
 params={k:v for k,v in step['args'].items() if k in ('task','keywords','files')};params['maxBytes']=12000
 queries.append({'id':episode['id'],'params':params})
assert len(queries)==8
report={'kind':'same-real-query-cold-resident-production-retrieval','modelCalls':0,'sourceReportSha256':hashlib.sha256(source.read_bytes()).hexdigest(),'sourceCorpus':original['corpusCommit'],'binarySha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'queries':queries,'rows':[],'note':'No model answer quality inference. Both engines get the same observed query. Cold starts a fresh process per query; resident keeps one process per engine. Source report files were excluded consistently.'}
def start(mode,label):
 env=os.environ.copy();env['NOVA_DATA_DIR']=str((out/'cache'/label).resolve())
 for name in ('NOVA_POLARIS_EMBEDDING_URL','NOVA_POLARIS_RERANK_URL','NOVA_POLARIS_ENDPOINT','NOVA_POLARIS_API_KEY','NOVA_POLARIS_RERANK'):env.pop(name,None)
 return subprocess.Popen([str(binary)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,env=env)
def ask(p,mode,q,phase,round):
 t=time.perf_counter();p.stdin.write(json.dumps({'root':str(root),'mode':mode,'params':q['params']},ensure_ascii=False)+'\n');p.stdin.flush();line=p.stdout.readline();assert line,'retriever stopped';result=json.loads(line)
 text=result.get('result',{}).get('text','');meta={}
 for ln in text.splitlines():
  if ln.startswith('# retrieval: '):meta=json.loads(ln[len('# retrieval: '):]);break
 row={'query':q['id'],'engine':mode,'phase':phase,'round':round,'wallMs':(time.perf_counter()-t)*1000,'engineMs':result.get('ms'),'ok':result.get('ok'),'indexPartial':meta.get('indexPartial'),'files':meta.get('files'),'changedFiles':meta.get('changedFiles'),'primary':[{k:v for k,v in e.items() if k in ('file','symbol','start','end')} for e in meta.get('evidence',[]) if e.get('relation')=='primary'],'result':result};report['rows'].append(row)
 print(json.dumps({k:v for k,v in row.items() if k not in ('result','primary')},ensure_ascii=False),flush=True)
for q in queries:
 for mode in ('baseline','candidate'):
  p=start(mode,'cold-'+mode+'-'+q['id'])
  try:ask(p,mode,q,'cold',0)
  finally:p.terminate();p.wait(timeout=5)
for mode in ('baseline','candidate'):
 p=start(mode,'resident-'+mode)
 try:
  for round in range(3):
   for q in queries:ask(p,mode,q,'resident',round)
 finally:p.terminate();p.wait(timeout=5)
report['summary']={}
for mode in ('baseline','candidate'):
 for phase in ('cold','resident-warm'):
  rows=[r for r in report['rows'] if r['engine']==mode and (r['phase']=='cold' if phase=='cold' else r['phase']=='resident' and r['round']>0)]
  report['summary'][mode+'-'+phase]={'n':len(rows),'medianMs':statistics.median(r['wallMs'] for r in rows),'meanMs':statistics.mean(r['wallMs'] for r in rows),'errors':sum(not r['ok'] for r in rows),'partial':sum(r['indexPartial'] is True for r in rows)}
(out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2));print(json.dumps(report['summary'],ensure_ascii=False))
