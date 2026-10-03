"""Same eight real replay queries, three retrieval arms, no answer-model calls.
The four original tasks have no independent root-cause gold: this measures source
entry-point/body coverage, NOT diagnosis correctness or task completion.
"""
import hashlib, importlib.util, json, os, pathlib, platform, re, secrets, socket, statistics, subprocess, sys, tempfile, time, urllib.request
from huggingface_hub import snapshot_download
P=pathlib.Path
out=P('validation/real-vector');out.mkdir(parents=True,exist_ok=True)
spec=importlib.util.spec_from_file_location('ab',P('scripts/polaris-ab.py'));ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
source=P(os.environ['POLARIS_REPLAY_REPORT']);binary=P(os.environ['POLARIS_REPLAY_BINARY']).resolve();root=P(os.environ['OPERATOR_REPLAY_CORPUS']).resolve()
original=json.loads(source.read_text());queries=[]
for e in original['runs']:
    if e['repeat']!=0:continue
    step=next(s for s in e['steps'] if s.get('tool')=='polaris')
    params={k:v for k,v in step['args'].items() if k in ('task','keywords','files')};params['maxBytes']=12000
    queries.append({'id':e['id'],'caseId':e['caseId'],'params':params})
assert len(queries)==8 and len({q['caseId'] for q in queries})==4
assert subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip()=='22e3cf0fb55537ca6c93b68126c2c737794d783d'
assert hashlib.sha256(binary.read_bytes()).hexdigest()=='ef1896343d43926775c4c28923ffd0e981584069b327b750b04c8144502e6537'
# Freeze labels before any new-arm retrieval. These are navigation targets, not
# root-cause labels. The ambiguous Lyra regression has no scored gold.
labels={
 '18be7005-4': {'file':'src-tauri/src/nova_tools_native/context.rs','symbols':['co_changed_files','fast_context_run']},
 '719d5639-17': {'file':'src-tauri/src/nova_tools_native/context.rs','symbols':['fast_context','fast_context_run']},
 'e09fd327-8': {'file':'src-tauri/src/nova_tools_native/context.rs','symbols':['fast_context_run','build_index','fast_context_no_index']},
}
for label in labels.values():
    text=(root/label['file']).read_text()
    for symbol in label['symbols']:assert re.search(r'\bfn\s+'+re.escape(symbol)+r'\s*\(',text),symbol
report={'kind':'real-query-local-vector-three-arm','answerModelCalls':0,'queries':queries,'labels':labels,'independentTasks':4,'scoredNavigationTasks':3,'sourceCommit':original['corpusCommit'],'sourceReportSha256':hashlib.sha256(source.read_bytes()).hexdigest(),'binarySha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'platform':platform.platform(),'rows':[],'method':'All arms receive identical task/keywords/files/maxBytes=12000. Three rotating-order rounds, first pass separate, warm rounds 1/2. Real learned E5 vectors, no reranker. Labels fixed from visible source symbols before this run; previous outputs have been seen, not blind. Coverage is entry-point/body retrieval, not root-cause accuracy. Ambiguous Lyra task unscored. No API credentials, no source upload.'}
(out/'labels-before-run.json').write_text(json.dumps(labels,ensure_ascii=False,indent=2))
def save(): (out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
def health(url,token):return json.load(urllib.request.urlopen(urllib.request.Request(url+'/health',headers={'Authorization':'Bearer '+token}),timeout=3))
def quality(q,text):
    label=next((v for k,v in labels.items() if q['id'].startswith(k)),None)
    if label is None:return {'scored':False,'reason':'No independent root-cause or navigation gold for ambiguous Lyra slowdown'}
    sections=ab.sections(text);primary=[s for s in sections if s['relation'] in ('primary','baseline-file')]
    def found(s):return s['file']==label['file'] and any(re.search(r'\bfn\s+'+re.escape(name)+r'\s*\(',s['body']) for name in label['symbols'])
    rank=next((i+1 for i,s in enumerate(primary) if found(s)),None)
    return {'scored':True,'top1Entry':rank==1,'entryBody':any(found(s) for s in sections),'rank':rank,'primaryFiles':[s['file'] for s in primary]}
processes={};worker=None;log=None
try:
    env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')}
    repo='intfloat/multilingual-e5-small';revision='614241f622f53c4eeff9890bdc4f31cfecc418b3'
    started=time.perf_counter();model=P(snapshot_download(repo,revision=revision,allow_patterns=['*.json','*.safetensors','*.model','vocab.txt'],ignore_patterns=['onnx/*','openvino/*','*.bin'],max_workers=2))
    weights=list(model.glob('*.safetensors'));assert weights
    report['model']={'repo':repo,'revision':revision,'weights':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in weights},'downloadMs':(time.perf_counter()-started)*1000,'pooling':'masked mean, normalized, query/passage prefix','maxTokens':256}
    token=secrets.token_urlsafe(32)
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    url=f'http://127.0.0.1:{port}';identity='e5:'+revision+'|meanpool256-v1'
    sem={**env,'NOVA_POLARIS_SEMANTIC_URL':url,'NOVA_POLARIS_SEMANTIC_MODEL':identity,'NOVA_POLARIS_SEMANTIC_TOKEN':token,'NOVA_POLARIS_RERANK':'0'}
    log=(out/'worker.log').open('w');worker=subprocess.Popen([sys.executable,'scripts/polaris-semantic-server.py','--model-path',str(model),'--identity',identity,'--port',str(port)],env=sem,stdout=log,stderr=log)
    for n in range(180):
        if worker.poll() is not None:raise RuntimeError('local vector worker exited; inspect worker.log')
        try:report['workerInitial']=health(url,token);break
        except Exception:time.sleep(1)
    else:raise RuntimeError('local worker startup timeout')
    cache=P(tempfile.mkdtemp(prefix='polaris-real-vector-'))
    for arm in ['old','lexical','vector']:
        processes[arm]=ab.Engine(binary,{**(sem if arm=='vector' else env),'NOVA_DATA_DIR':str(cache/arm)},out/(arm+'.log'))
    started=time.perf_counter();prep=processes['vector'].ask({'root':str(root),'mode':'prepare'},timeout=1500)
    report['vectorPreparation']={'wallMs':(time.perf_counter()-started)*1000,'result':prep};save()
    if not prep.get('ok'):raise RuntimeError('learned index preparation failed')
    for round in range(3):
        for i,q in enumerate(queries if round%2==0 else list(reversed(queries))):
            arms=['old','lexical','vector'];offset=(round+i)%3;arms=arms[offset:]+arms[:offset]
            for arm in arms:
                result=processes[arm].ask({'root':str(root),'mode':'baseline' if arm=='old' else 'candidate','params':q['params']})
                text=result.get('result',{}).get('text','');meta={}
                for ln in text.splitlines():
                    if ln.startswith('# retrieval: '):meta=json.loads(ln[len('# retrieval: '):]);break
                row={'id':q['id'],'caseId':q['caseId'],'round':round,'arm':arm,'wallMs':result.get('wallMs'),'engineMs':result.get('ms'),'ok':result.get('ok'),'quality':quality(q,text),'retrieval':meta,'result':result}
                report['rows'].append(row);save()
                if not result.get('ok'):raise RuntimeError('retrieval failed: '+str(result.get('error')))
                if arm=='vector' and not (meta.get('backend')=='hybrid' and meta.get('semanticReady',0)>0 and meta.get('semanticReady')==meta.get('semanticTotal') and meta.get('indexPartial') is False):
                    raise RuntimeError('vector arm silently fell back or was incomplete: '+str(meta))
                print(json.dumps({k:row[k] for k in ['id','round','arm','wallMs','quality']},ensure_ascii=False),flush=True)
    report['summary']={}
    for arm in processes:
        first=[r for r in report['rows'] if r['arm']==arm and r['round']==0];warm=[r for r in report['rows'] if r['arm']==arm and r['round']>0];scored=[r for r in first if r['quality']['scored']]
        report['summary'][arm]={'uniqueQueries':8,'scoredQueries':len(scored),'top1Entry':sum(r['quality']['top1Entry'] for r in scored),'entryBody':sum(r['quality']['entryBody'] for r in scored),'warmSamples':len(warm),'warmMedianMs':statistics.median(r['wallMs'] for r in warm),'warmP95Ms':sorted(r['wallMs'] for r in warm)[-1],'firstQueryMs':first[0]['wallMs'],'firstPassMedianMs':statistics.median(r['wallMs'] for r in first)}
    report['workerFinal']=health(url,token);report['status']='completed';save();print(json.dumps(report['summary'],ensure_ascii=False,indent=2))
except Exception as error:
    report['status']='failed';report['error']=str(error);save();raise
finally:
    for process in processes.values():process.close()
    if worker:
        worker.terminate()
        try:worker.wait(10)
        except subprocess.TimeoutExpired:worker.kill();worker.wait()
    if log:log.close()
