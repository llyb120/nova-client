"""Frozen-corpus A/B for exact production Rust retrieval, with optional real local models.
Gold cases live OUTSIDE the indexed checkout. Do not point --corpus at the PR working tree.
"""
from __future__ import annotations
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import re
import secrets
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
from threading import Thread
import time
import urllib.request

BASE = '3da28d30f1adfd0da3b993813a47aa3fff20fdeb'

class Engine:
    def __init__(self, binary: Path, env: dict, log: Path):
        self.log = log.open('w', encoding='utf-8')
        self.p = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True, encoding='utf-8', env=env, bufsize=1)
        self.responses = queue.Queue()
        def reader():
            for line in self.p.stdout:
                try: self.responses.put(json.loads(line))
                except ValueError: self.responses.put({'ok':False,'error':'non-JSON harness output','raw':line,'ms':0})
            self.responses.put({'ok':False,'error':'harness exited','ms':0})
        Thread(target=reader,daemon=True).start()
    def ask(self, request: dict, timeout=90):
        start=time.perf_counter()
        self.p.stdin.write(json.dumps(request, ensure_ascii=False)+'\n');self.p.stdin.flush()
        try: result=self.responses.get(timeout=timeout)
        except queue.Empty:
            self.p.kill();raise RuntimeError('harness timed out')
        result['wallMs']=(time.perf_counter()-start)*1000
        return result
    def close(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:self.p.wait(5)
            except subprocess.TimeoutExpired:self.p.kill();self.p.wait()
        self.log.close()

def old_normalize(query):
    return {'query':query,'keywords':[] if ' ' in query else [query], 'task':query if ' ' in query else '', 'files':[]}

def sections(text):
    """Ignore metadata, signatures/impact tables and next_reads. Only emitted source bodies count."""
    result=[];current=None;collect=False
    for line in text.splitlines():
        if line.startswith('### '):
            m=re.match(r'^### (.+?):(\d+)-(\d+) \[([^ ]+) (.+)\]$',line)
            if m:
                current={'file':m[1], 'relation':m[4], 'symbol':m[5], 'body':[]};result.append(current);collect=False
            else:
                m=re.match(r'^### (.+?) \(\d+L\)(.*)$',line)
                current={'file':m[1], 'relation':'baseline-file','symbol':None,'body':[]} if m else None
                if current:result.append(current)
                collect=bool(current and ' FULL' in line)
            continue
        if current is None:continue
        if current['relation']=='baseline-file':
            if line.startswith('@@ '):collect=True;continue
            if line.startswith(('~ ','## ','# ')):collect=False
            if collect:current['body'].append(line)
        else:
            m=re.match(r'^\d+: (.*)$',line)
            if m:current['body'].append(m[1])
    for section in result:section['body']='\n'.join(section['body'])
    return result

def matched(section,gold):
    if section['file']!=gold['file']:return False
    # Mentioning a function in another function is NOT its implementation.
    name=re.escape(gold['symbol'])
    definition=re.compile(r'(?:\bfn\s+|\bfunction\s+)' + name + r'\s*(?:<[^>]*>)?\s*\(|(?:\bconst\s+|\blet\s+)' + name + r'\s*=')
    if not definition.search(section['body']):return False
    body=re.sub(r'\s+','',section['body'])
    return all(re.sub(r'\s+','',needle) in body for needle in gold['needles'])

def assess(case,text):
    parsed=sections(text)
    primary=[s for s in parsed if s['relation'] in ('primary','baseline-file')]
    relevant=lambda s:any(matched(s,g) for g in case['primary'])
    rank=next((i+1 for i,s in enumerate(primary) if relevant(s)),None)
    found=any(relevant(s) for s in parsed)
    supports=all(any(matched(s,g) for s in parsed) for g in case['support'])
    return {'top1':rank==1,'top3':rank is not None and rank<=3,'coreBody':found,'evidenceSufficientProxy':found and supports,
            'rank':rank,'bodyFiles':[s['file'] for s in primary], 'bytes':len(text.encode()),
            'annotatedFileFraction':sum(s['file'] in {g['file'] for g in case['primary']+case['support']} for s in primary)/max(1,len(primary)),
            'correctAbstention':not parsed if not case['primary'] else None}

def download_models(out:Path):
    # Explicit setup downloads public pinned weights, never uploads source.
    from huggingface_hub import snapshot_download
    config=[];start=time.perf_counter()
    for repo,sha in [('intfloat/multilingual-e5-small','614241f622f53c4eeff9890bdc4f31cfecc418b3'),('cross-encoder/mmarco-mMiniLMv2-L12-H384-v1','1427fd652930e4ba29e8149678df786c240d8825')]:
        location=Path(snapshot_download(repo,revision=sha,allow_patterns=['*.json','*.safetensors','*.model','vocab.txt'],ignore_patterns=['onnx/*','openvino/*','*.bin'],max_workers=2))
        weights=sorted(location.glob('*.safetensors'))
        if not weights:raise RuntimeError('safe tensor weights are required')
        hashes={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in weights}
        config.append({'repo':repo,'revision':sha,'path':str(location),'weights':hashes})
    data={'models':config,'downloadMs':(time.perf_counter()-start)*1000,'maxTokens':256}
    (out/'models.json').write_text(json.dumps(data,indent=2))
    return data

def request(url,token,path='/health'):
    return json.load(urllib.request.urlopen(urllib.request.Request(url+path,headers={'Authorization':'Bearer '+token}),timeout=2))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--corpus',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--cases',type=Path,default=Path('bench/polaris-ab/cases.json'))
    parser.add_argument('--models',action='store_true',help='Explicitly download public models and evaluate local learned retrieval')
    parser.add_argument('--rounds',type=int,default=4)
    parser.add_argument('--split',choices=['dev','all'],default='all')
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=True)
    if not 2<=args.rounds<=10:parser.error('rounds must be 2..10')
    corpus=args.corpus.resolve();binary=args.binary.resolve();labels=json.loads(args.cases.read_text());cases=labels['cases']
    if args.split=='dev':cases=[c for c in cases if c['split']=='dev']
    if corpus==Path.cwd().resolve() or args.cases.resolve().is_relative_to(corpus):raise RuntimeError('labels must be outside corpus')
    revision=subprocess.check_output(['git','-C',str(corpus),'rev-parse','HEAD'],text=True).strip()
    if revision!=BASE or revision!=labels['baseline']:raise RuntimeError('corpus differs from frozen baseline')
    if subprocess.check_output(['git','-C',str(corpus),'status','--porcelain'],text=True).strip():raise RuntimeError('corpus is dirty')
    actual_context=hashlib.sha1(b'blob '+str((corpus/'src-tauri/src/nova_tools_native/context.rs').stat().st_size).encode()+b'\0'+(corpus/'src-tauri/src/nova_tools_native/context.rs').read_bytes()).hexdigest()
    if actual_context!='1ff6acaa423698df1b111287c68ca701b4ca8ae0':raise RuntimeError('baseline engine mismatch')
    for case in cases:
        for gold in case['primary']+case['support']:
            body=re.sub(r'\s+','',(corpus/gold['file']).read_text())
            if not all(re.sub(r'\s+','',n) in body for n in gold['needles']):raise RuntimeError('invalid label '+case['id'])
    env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')}
    processes={};model_process=None;model_log=None;raw=[]
    cache_root=Path(tempfile.mkdtemp(prefix='polaris-ab-cache-'))
    report={'baseline':BASE,'binarySha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'casesSha256':hashlib.sha256(args.cases.read_bytes()).hexdigest(),
      'candidateRevision':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
      'platform':platform.platform(),'python':sys.version,'cpuCount':os.cpu_count(),'rounds':args.rounds,'firstPass':[],'runs':raw,
      'method':'Same clean release corpus, labels external, release production modules. Rotating arm order; round 0 separate first pass, rounds 1..N warm. Disk/page caches not flushed; not strict OS-cold latency. Top1 ranks primary code bodies/files, never metadata. Hand labels incomplete: file fraction is not a universal precision/noise judgment; empty support labels measure core-body coverage, not whole-task completion.'}
    try:
        for arm in ['A_query','A_task','B_lexical']:
            processes[arm]=Engine(binary,{**env,'NOVA_DATA_DIR':str(cache_root/arm)},out/(arm+'.log'))
        if args.models:
            models=download_models(out);report['models']=models
            token=secrets.token_urlsafe(32)
            with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
            url=f'http://127.0.0.1:{port}'
            identity='e5:'+models['models'][0]['revision']+'|rerank:'+models['models'][1]['revision']+'|meanpool256-v1'
            semantic_env={**env,'NOVA_POLARIS_SEMANTIC_URL':url,'NOVA_POLARIS_SEMANTIC_MODEL':identity,'NOVA_POLARIS_SEMANTIC_TOKEN':token}
            model_log=(out/'model-worker.log').open('w')
            model_process=subprocess.Popen([sys.executable,'scripts/polaris-semantic-server.py','--model-path',models['models'][0]['path'],'--rerank-path',models['models'][1]['path'],'--identity',identity,'--port',str(port)],env=semantic_env,stdout=model_log,stderr=model_log)
            for _ in range(180):
                if model_process.poll() is not None:raise RuntimeError('model worker did not start; see model-worker.log')
                try:report['worker']=request(url,token);break
                except Exception:time.sleep(1)
            else:raise RuntimeError('model startup timeout')
            for arm in ['C_semantic','D_rerank']:
                processes[arm]=Engine(binary,{**semantic_env,'NOVA_DATA_DIR':str(cache_root/'semantic'),'NOVA_POLARIS_RERANK':'1' if arm=='D_rerank' else '0'},out/(arm+'.log'))
                prep=processes[arm].ask({'root':str(corpus),'mode':'prepare'},timeout=1500)
                report[arm+'Preparation']=prep
                if not prep['ok']:raise RuntimeError('learned indexing did not complete: '+str(prep))
        for round in range(args.rounds):
            for i,case in enumerate(cases):
                arms=list(processes);offset=(i+round)%len(arms);arms=arms[offset:]+arms[:offset]
                if round%2:arms.reverse()
                for arm in arms:
                    if arm=='A_query':params=old_normalize(case['query'])
                    elif arm=='A_task':params={'task':case['query']} if case['kind']=='natural' else old_normalize(case['query'])
                    else:params={'query':case['query']}
                    response=processes[arm].ask({'root':str(corpus),'mode':'baseline' if arm.startswith('A_') else 'candidate','params':params})
                    text=response.get('result',{}).get('text','')
                    item={'round':round,'id':case['id'],'split':case['split'],'arm':arm,'ok':response['ok'],'ms':response['ms'],'wallMs':response['wallMs'],'metrics':assess(case,text)}
                    if not response['ok']:item['error']=response.get('error')
                    meta=next((s[len('# retrieval: '):] for s in text.splitlines() if s.startswith('# retrieval: ')),None)
                    if meta:item['retrieval']=json.loads(meta)
                    if arm in ['C_semantic','D_rerank'] and case['kind']=='natural':
                        item['learnedReady']=item.get('retrieval',{}).get('backend')=='hybrid'
                        item['rerankUsed']=item.get('retrieval',{}).get('reranked',False)
                    target=out/'raw'/case['id'];target.mkdir(parents=True,exist_ok=True);(target/f'{arm}-{round}.txt').write_text(text,encoding='utf-8')
                    (report['firstPass'] if round==0 else raw).append(item)
                    (out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
                    print(f'{arm} {round} {case["id"]}: {item["metrics"]["rank"]} {item["ms"]:.1f}ms',flush=True)
        summary={}
        for split in ['dev','heldout','control','negative','all-natural']:
            summary[split]={}
            for arm in processes:
                items=[r for r in raw if r['arm']==arm and (r['split'] in ['dev','heldout'] if split=='all-natural' else r['split']==split)]
                if not items:continue
                values=sorted(r['ms'] for r in items)
                fields=['top1','top3','coreBody','evidenceSufficientProxy','annotatedFileFraction','bytes']
                summary[split][arm]={key:statistics.mean(float(r['metrics'][key]) for r in items) for key in fields}
                summary[split][arm].update(samples=len(items),p50Ms=statistics.median(values),p95Ms=values[min(len(values)-1,int(len(values)*.95))],errors=sum(not r['ok'] for r in items))
                if split=='negative':summary[split][arm]['abstention']=statistics.mean(float(r['metrics']['correctAbstention']) for r in items)
        report['summary']=summary
        report['backendCounts']={arm:dict(Counter(r.get('retrieval',{}).get('backend','exact-legacy') for r in raw if r['arm']==arm)) for arm in processes}
        report['rerankCounts']={arm:sum(r.get('rerankUsed',False) for r in raw if r['arm']==arm) for arm in processes}
        if args.models:report['workerFinal']=request(url,token)
        report['sourceUnchanged']=not subprocess.check_output(['git','-C',str(corpus),'status','--porcelain'],text=True).strip()
        if not report['sourceUnchanged']:raise RuntimeError('benchmark modified corpus')
        (out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
        print(json.dumps(summary,ensure_ascii=False,indent=2),flush=True)
    finally:
        for p in processes.values():p.close()
        if model_process:
            model_process.terminate()
            try:model_process.wait(10)
            except subprocess.TimeoutExpired:model_process.kill();model_process.wait()
        if model_log:model_log.close()
        shutil.rmtree(cache_root)

if __name__=='__main__':main()
