"""Deterministic scaling control, not an answer-accuracy benchmark.
Compare identical source and budgets with no prepared index. Noise is generated,
not sampled from real repositories. All timings include actual production engine work.
"""
import argparse, hashlib, importlib.util, json, os, pathlib, platform, statistics, tempfile, time
P=pathlib.Path
spec=importlib.util.spec_from_file_location('ab',P(__file__).with_name('polaris-ab.py'));ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
p=argparse.ArgumentParser();p.add_argument('--previous',type=P,required=True);p.add_argument('--candidate',type=P,required=True);p.add_argument('--out',type=P,default=P('validation/scale'));args=p.parse_args();args.out.mkdir(parents=True,exist_ok=True)
env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')}
source={
 'src/api.ts':"import { collectLedger } from './ledger';\nexport function readLedger(raw: string[]) { return collectLedger(raw); }\n",
 'src/ledger.ts':"import { parseRow } from './row';\n// Read ledger entries, validate records and compute sum\nexport function collectLedger(raw: string[]) {\n const rows = raw.map(parseRow).filter(row => row.valid);\n const total = rows.reduce((sum, row) => sum + row.amount, 0);\n return { rows, total };\n}\n",
 'src/row.ts':"export function parseRow(raw: string) {\n const [id, value] = raw.split(',');\n const amount = Number(value);\n return { id, amount, valid: Boolean(id) && Number.isFinite(amount) };\n}\n",
}
params={'task':'read ledger entries validate records compute sum','maxBytes':12000}
gold=[{'file':'src/ledger.ts','symbol':'collectLedger','needles':['raw.map(parseRow)','sum + row.amount']},{'file':'src/row.ts','symbol':'parseRow','needles':['Number.isFinite(amount)']},{'file':'src/api.ts','symbol':'readLedger','needles':['collectLedger(raw)']}]
report={'method':__doc__,'answerModelCalls':0,'platform':platform.platform(),'labels':gold,'params':params,'rows':[],'binaryHashes':{k:hashlib.sha256(v.read_bytes()).hexdigest() for k,v in [('pr11',args.previous),('demand',args.candidate)]}}
def save():(args.out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
def ask(engine,root,arm,size,phase,started=None):
 result=engine.ask({'root':str(root),'mode':'candidate','params':params});text=result.get('result',{}).get('text','');sections=ab.sections(text)
 missing=[g for g in gold if not any(ab.matched(s,g) for s in sections)]
 meta={}
 for line in text.splitlines():
  if line.startswith('# retrieval: '):meta=json.loads(line[len('# retrieval: '):]);break
 row={'arm':arm,'files':size+3,'phase':phase,'ok':result['ok'],'wallMs':(time.perf_counter()-started)*1000 if started else result['wallMs'],'engineMs':result['ms'],'allRequired':not missing,'missing':[g['symbol'] for g in missing],'retrieval':meta}
 report['rows'].append(row);(args.out/f'{size}-{arm}-{phase}.txt').write_text(text);save();return text
with tempfile.TemporaryDirectory(prefix='polaris-scale-') as tmp:
 root=P(tmp)/'corpus';root.mkdir()
 for file,text in source.items():
  path=root/file;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(text)
 for target in [100,1000,6000]:
  for i in range(0 if target==100 else {1000:100,6000:1000}[target],target):
   file=root/'noise'/f'group{i//100:02d}'/f'feature{i:04d}.ts';file.parent.mkdir(parents=True,exist_ok=True)
   file.write_text(''.join(f'export function feature_{i}_{j}(value: number) {{\n const offset = {j};\n return value + offset;\n}}\n' for j in range(8)))
  for arm,binary in [('pr11',args.previous),('demand',args.candidate)]:
   started=time.perf_counter();engine=ab.Engine(binary.resolve(),{**env,'NOVA_DATA_DIR':str(P(tmp)/f'{arm}-{target}')},args.out/f'{target}-{arm}.log')
   try:
    ask(engine,root,arm,target,'cold',started)
    for j in range(3):ask(engine,root,arm,target,f'warm{j}')
    if arm=='demand':
     file=root/'src/ledger.ts';stamp=file.stat();file.write_text(source['src/ledger.ts'].replace('const total','const fresh').replace('{ rows, total }','{ rows, fresh }'));os.utime(file,ns=(stamp.st_atime_ns,stamp.st_mtime_ns))
     text=ask(engine,root,arm,target,'same-mtime-edit');assert 'const fresh' in text and 'const total' not in text,'stale source emitted'
     file.write_text(source['src/ledger.ts'])
   finally:engine.close()
report['summary']={}
for size in [103,1003,6003]:
 report['summary'][str(size)]={}
 for arm in ['pr11','demand']:
  rows=[r for r in report['rows'] if r['files']==size and r['arm']==arm];warm=[r['wallMs'] for r in rows if r['phase'].startswith('warm')]
  report['summary'][str(size)][arm]={'coldMs':rows[0]['wallMs'],'warmMedianMs':statistics.median(warm),'completeResponses':sum(r['allRequired'] for r in rows),'samples':len(rows),'coldParsedFiles':rows[0]['retrieval'].get('demand',{}).get('parsedFiles',rows[0]['retrieval'].get('files'))}
report['status']='completed';save();print(json.dumps(report['summary'],indent=2))
assert all(r['ok'] for r in report['rows'])
assert all(r['allRequired'] for r in report['rows'] if r['arm']=='demand'), 'missing required scale-test evidence'
