"""Original fastcontext / pre-fix one-shot / patched source-text compatibility.
Synthetic format reproductions of the screenshot query, NOT the user's business repository.
Frozen existing retrieval labels are reused unchanged, outside the searched corpus.
"""
import argparse,hashlib,importlib.util,json,os,pathlib,statistics,tempfile,time
P=pathlib.Path
spec=importlib.util.spec_from_file_location('ab',P('scripts/polaris-ab.py'));ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
p=argparse.ArgumentParser();p.add_argument('--accepted',type=P,required=True);p.add_argument('--candidate',type=P,required=True);p.add_argument('--corpus',type=P,required=True);p.add_argument('--out',type=P,default=P('validation/text-recall'));a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
previous=(a.accepted/'bin/previous').resolve();before=(a.accepted/'bin/candidate').resolve();candidate=a.candidate.resolve()
assert hashlib.sha256(before.read_bytes()).hexdigest()=='35c4d33d93988b3b7d953dbf456f8cb073c398ddbbb6eb54e877bcd0caf6d969'
assert hashlib.sha256(previous.read_bytes()).hexdigest()=='ef1896343d43926775c4c28923ffd0e981584069b327b750b04c8144502e6537'
assert hashlib.sha256(candidate.read_bytes()).digest()!=hashlib.sha256(before.read_bytes()).digest()
for b in (previous,before,candidate):b.chmod(b.stat().st_mode|0o111)
query='查下小游戏榜单的sql'
sql='-- 小游戏榜单\nSELECT game_id, SUM(score) AS total_score\nFROM mini_game_scores\nGROUP BY game_id\nORDER BY total_score DESC\nLIMIT 100;\n'
fixtures={
 'plain-sql':{'queries/report.sql':sql},
 'markdown-sql':{'docs/ranking.md':'# 小游戏榜单\n\n```sql\n'+sql+'```\n'},
 'xml-mapper':{'mapper/GameMapper.xml':'<mapper namespace="MiniGameMapper">\n<!-- 小游戏榜单 -->\n<select id="ranking">\n'+sql+'</select>\n</mapper>\n'},
 'typescript-string':{'query.ts':'// 小游戏榜单\nexport const BOARD_SQL = `'+sql+'`;\nexport function ping() { return 1; }\n'},
 'python-string':{'query.py':'# 小游戏榜单\nBOARD_SQL = """'+sql+'"""\ndef ping():\n    return 1\n'},
 'java-string':{'Query.java':'class Query {\n// 小游戏榜单\npublic static final String SQL = "SELECT game_id FROM mini_game_scores ORDER BY score DESC";\npublic int ping() { return 1; }\n}\n'},
 'yaml-query':{'queries.yml':'# 小游戏榜单\nboard: |\n'+''.join('  '+l+'\n' for l in sql.splitlines())},
 'json-query':{'queries.json':json.dumps({'name':'小游戏榜单','sql':sql},ensure_ascii=False,indent=2)},
 'properties-query':{'queries.properties':'# 小游戏榜单\nquery=SELECT game_id FROM mini_game_scores ORDER BY score DESC\n'},
 'late-markdown':{'docs/query.md':'\n'.join('unrelated paragraph '+str(i) for i in range(900))+'\n# 小游戏榜单\n```sql\n'+sql+'```\n'},
 'mixed-distractor':{'docs/query.md':'# 小游戏榜单\n```sql\n'+sql+'```\n','runner.ts':'export function sql(){return "SELECT 1";}\n'},
 'path-only':{'mini_game_ranking.sql':'SELECT game_id FROM mini_game_scores ORDER BY score DESC LIMIT 100;\n'},
}
report={'method':__doc__,'userQuery':query,'answerModelCalls':0,'budgetBytes':12000,'fixtureRows':[],'existingRows':[],'binaries':{'original':hashlib.sha256(previous.read_bytes()).hexdigest(),'before':hashlib.sha256(before.read_bytes()).hexdigest(),'after':hashlib.sha256(candidate.read_bytes()).hexdigest()}}
root=a.out/'corpora';root.mkdir(exist_ok=True)
def save():(a.out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf-8')
engines={'original':(previous,'baseline'),'before':(before,'candidate'),'after':(candidate,'candidate')}
env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')}
def engine(arm,cache,tag):return ab.Engine(engines[arm][0],{**env,'NOVA_DATA_DIR':str(cache/tag)},a.out/(tag+'.log'))
def run(e,arm,corpus,params):
    r=e.ask({'root':str(corpus.resolve()),'mode':engines[arm][1],'params':params});text=r.get('result',{}).get('text','');return r,text
with tempfile.TemporaryDirectory(prefix='polaris-text-recall-') as tmp:
 cache=P(tmp)
 for id,files in fixtures.items():
  corpus=root/id;corpus.mkdir(exist_ok=True)
  for file,text in files.items():
   path=corpus/file;path.parent.mkdir(parents=True,exist_ok=True);path.write_text(text,encoding='utf-8')
  for form in ['task','query','keywords']:
   params={form:query if form!='keywords' else ['小游戏','榜单','sql'],'maxBytes':12000}
   for arm in engines:
    request=dict(params)
    if arm=='original' and form=='query':request['task']=request.pop('query')
    e=engine(arm,cache,f'{id}-{form}-{arm}')
    try:r,text=run(e,arm,corpus,request)
    finally:e.close()
    bodies='\n'.join(s['body'] for s in ab.sections(text));ok=r.get('ok') and 'FROM mini_game_scores' in bodies
    raw=a.out/'raw'/id;raw.mkdir(parents=True,exist_ok=True);(raw/f'{form}-{arm}.txt').write_text(text,encoding='utf-8')
    report['fixtureRows'].append({'id':id,'form':form,'arm':arm,'ok':bool(ok),'ms':r.get('ms'),'bytes':len(text.encode()),'error':r.get('error')});save()
 # Reuse previous fixed labels, not new labels chosen for this implementation.
 labels=json.loads((a.accepted/'fastcontext/labels-before-run.json').read_text(encoding='utf-8'))
 labels=[c for c in labels if c['group'] in ['regression','closure']]
 for arm in ['before','after']:
  e=engine(arm,cache,'existing-'+arm)
  try:
   for round in range(2):
    for c in labels:
     r,text=run(e,arm,a.corpus,c['params']);sections=ab.sections(text);metrics=ab.assess(c,text)
     missing=[g for g in c['primary']+c['support'] if not any(ab.matched(s,g) for s in sections)]
     metrics['allRequired']=not missing and bool(c['primary'])
     report['existingRows'].append({'id':c['id'],'group':c['group'],'kind':c['kind'],'arm':arm,'round':round,'ms':r.get('ms'),'ok':r.get('ok'),'metrics':metrics})
     raw=a.out/'existing-raw'/c['id'];raw.mkdir(parents=True,exist_ok=True);(raw/f'{arm}-{round}.txt').write_text(text,encoding='utf-8');save()
  finally:e.close()
rows=report['fixtureRows'];report['formatSummary']={arm:{'hits':sum(r['ok'] for r in rows if r['arm']==arm),'total':sum(r['arm']==arm for r in rows),'p50Ms':statistics.median(r['ms'] for r in rows if r['arm']==arm)}for arm in engines}
report['existingSummary']={}
for arm in ['before','after']:
 r=[r for r in report['existingRows'] if r['arm']==arm and r['round']==0 and r['group']=='regression' and r['kind']=='natural']
 warm=[r for r in report['existingRows'] if r['arm']==arm and r['round']==1 and r['group']=='regression' and r['kind']=='natural']
 closure=[r for r in report['existingRows'] if r['arm']==arm and r['round']==0 and r['group']=='closure']
 report['existingSummary'][arm]={'top1':sum(x['metrics']['top1'] for x in r),'coreBody':sum(x['metrics']['coreBody'] for x in r),'closure':sum(x['metrics']['allRequired'] for x in closure),'newQuestionP50Ms':statistics.median(x['ms'] for x in r),'warmP50Ms':statistics.median(x['ms'] for x in warm)}
old={(r['id'],r['form']):r['ok'] for r in rows if r['arm']=='original'}
fixed={(r['id'],r['form']):r['ok'] for r in rows if r['arm']=='after'}
before_rows={(r['id'],r['round']):r for r in report['existingRows'] if r['arm']=='before'}
regressions=[r['id'] for r in report['existingRows'] if r['arm']=='after' and before_rows[(r['id'],r['round'])]['metrics']['coreBody'] and not r['metrics']['coreBody']]
checks={'all_36_format_queries_return_sql':all(fixed.values()) and len(fixed)==36,'every_original_hit_preserved':all(not ok or fixed[k] for k,ok in old.items()),'all_existing_queries_return':all(r['ok'] for r in report['existingRows']),'no_previously_covered_core_lost':not regressions,'four_dependency_closures_preserved':report['existingSummary']['after']['closure']==4,'fixed_top1_preserved':report['existingSummary']['after']['top1']>=report['existingSummary']['before']['top1'],'output_budget_preserved':all(r['bytes']<=12000 for r in rows if r['arm']=='after'),'warm_p50_under_100ms':report['existingSummary']['after']['warmP50Ms']<100}
report['checks']=checks;report['regressions']=regressions;report['passed']=all(checks.values());save();print(json.dumps({k:report[k] for k in ['formatSummary','existingSummary','checks','regressions','passed']},ensure_ascii=False,indent=2));raise SystemExit(0 if report['passed'] else 1)
