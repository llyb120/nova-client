"""Deterministic context-only edits: 1 Polaris call, 0 follow-up source reads.
This is a structural sufficiency test, not an LLM success-rate measurement. Edits and
independent behavior tests are fixed before execution and are outside the corpus.
"""
import argparse, importlib.util, json, os, pathlib, re, subprocess, tempfile, time
P=pathlib.Path
spec=importlib.util.spec_from_file_location('ab',P('scripts/polaris-ab.py'));ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
p=argparse.ArgumentParser();p.add_argument('--binary',type=P,required=True);p.add_argument('--out',type=P,default=P('validation/context-only-edit'));a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
cases=[
 {'id':'retry-policy','task':'请求失败后增加429限流重试，仍然遵守最大尝试次数限制',
  'source':'const MAX_ATTEMPTS: u8 = 3;\nstruct Attempt { status: u16, used: u8 }\n// 请求失败后的重试策略及尝试次数限制\nfn retry_request(a: Attempt) -> bool { under_budget(a.used) && is_temporary(a.status) }\nfn under_budget(used: u8) -> bool { used < MAX_ATTEMPTS }\nfn is_temporary(status: u16) -> bool { status == 503 }\n',
  'old':'status == 503','new':'status == 503 || status == 429',
  'tests':'#[test] fn behavior() { assert!(retry_request(Attempt{status:429,used:0})); assert!(retry_request(Attempt{status:503,used:1})); assert!(!retry_request(Attempt{status:401,used:0})); assert!(!retry_request(Attempt{status:429,used:3})); }'},
 {'id':'queue-dedup','task':'发送队列按照请求id去重，不能因为队列非空就丢掉不同的请求',
  'source':'struct Queue { ids: Vec<u64> }\n// 发送队列按请求id去重\nfn enqueue(q: &mut Queue, id: u64) -> bool { if contains_id(q,id) { return false; } q.ids.push(id); true }\nfn contains_id(q: &Queue, id: u64) -> bool { let _ = id; !q.ids.is_empty() }\n',
  'old':'let _ = id; !q.ids.is_empty()','new':'q.ids.contains(&id)',
  'tests':'#[test] fn behavior() { let mut q=Queue{ids:vec![1]}; assert!(enqueue(&mut q,2)); assert!(!enqueue(&mut q,1)); assert_eq!(q.ids,vec![1,2]); }'},
 {'id':'snapshot-guard','task':'快照校验必须使用相同版本，不接受旧版本，还要保留权限及有效期检查',
  'source':'const MAX_AGE: u64 = 180;\nstruct Snapshot { version: u64, age: u64, allowed: bool }\n// 快照权限、有效期及当前版本校验\nfn allow_input(s: Snapshot, current: u64) -> bool { s.allowed && s.age < MAX_AGE && same_version(s.version,current) }\nfn same_version(observed: u64, current: u64) -> bool { observed <= current }\n',
  'old':'observed <= current','new':'observed == current',
  'tests':'#[test] fn behavior() { assert!(!allow_input(Snapshot{version:1,age:0,allowed:true},2)); assert!(allow_input(Snapshot{version:2,age:0,allowed:true},2)); assert!(!allow_input(Snapshot{version:2,age:180,allowed:true},2)); assert!(!allow_input(Snapshot{version:2,age:0,allowed:false},2)); }'},
 {'id':'restore-bounds','task':'恢复窗口位置时，超过上边界应限制到上边界而不是跳到下边界',
  'source':'struct Window { x: i32, y: i32 }\nconst LIMIT: i32 = 100;\n// 恢复窗口位置，约束到当前屏幕的上下边界\nfn restore_window(w: Window) -> Window { Window{x:clamp_value(w.x,0,LIMIT),y:clamp_value(w.y,0,LIMIT)} }\nfn clamp_value(value: i32, low: i32, high: i32) -> i32 { if value > high { low } else if value < low { low } else { value } }\n',
  'old':'if value > high { low }','new':'if value > high { high }',
  'tests':'#[test] fn behavior() { let w=restore_window(Window{x:120,y:-1}); assert_eq!(w.x,100); assert_eq!(w.y,0); let v=restore_window(Window{x:50,y:100}); assert_eq!(v.x,50); assert_eq!(v.y,100); }'},
]
report={'kind':'deterministic-context-only-edit','answerModelCalls':0,'cases':[],'method':__doc__}
def save(): (a.out/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2))
def test_source(text,folder,name):
    source=folder/(name+'.rs');exe=folder/(name+('.exe' if os.name=='nt' else ''))
    source.write_text(text);build=subprocess.run(['rustc','--edition=2021','--test',str(source),'-o',str(exe)],capture_output=True,text=True,timeout=30)
    run=subprocess.run([str(exe)],capture_output=True,text=True,timeout=10) if build.returncode==0 else None
    return {'compiled':build.returncode==0,'passed':run is not None and run.returncode==0,'build':build.stderr,'test':run.stdout if run else ''}
for case in cases:
    row={'id':case['id'],'polarisCalls':0,'supplementalSourceReads':0,'passed':False}
    folder=a.out/case['id'];folder.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='polaris-edit-corpus-') as tmp:
        root=P(tmp);(root/'task.rs').write_text(case['source'])
        # Test bodies are never in the retrievable repository.
        original=test_source(case['source']+'\n'+case['tests'],folder,'oracle-before')
        assert original['compiled'] and not original['passed'],'fixture must demonstrate a real bug'
        env={k:v for k,v in os.environ.items() if not k.startswith('NOVA_POLARIS_')};env['NOVA_DATA_DIR']=str(root/'cache')
        engine=ab.Engine(a.binary.resolve(),env,folder/'retrieval.log')
        try:
            response=engine.ask({'root':str(root),'mode':'candidate','params':{'task':case['task'],'maxBytes':12000}});row['polarisCalls']+=1
        finally:engine.close()
        packet=response.get('result',{}).get('text','');(folder/'packet.txt').write_text(packet);row['retrievalOk']=response['ok'];row['bytes']=len(packet.encode());row['ms']=response['ms']
        # From here the editor uses packet text only. It never opens root/task.rs.
        source_lines={}
        for section in ab.sections(packet):
            if section['file']!='task.rs':continue
            for ln in section['body'].splitlines():
                m=re.match(r'^(\d+): (.*)$',ln)
                if m:
                    n=int(m[1]);assert n not in source_lines or source_lines[n]==m[2];source_lines[n]=m[2]
        reconstructed='\n'.join(source_lines[n] for n in sorted(source_lines))+'\n'
        row['returnedLines']=len(source_lines)
        row['before']=test_source(reconstructed+case['tests'],folder,'packet-before')
        if reconstructed.count(case['old'])==1:
            patched=reconstructed.replace(case['old'],case['new'])
            row['after']=test_source(patched+case['tests'],folder,'packet-after')
            row['passed']=row['before']['compiled'] and not row['before']['passed'] and row['after']['passed']
        else:row['error']='exact edit location missing or ambiguous in returned packet'
    report['cases'].append(row);save();print(json.dumps({k:row[k] for k in ('id','polarisCalls','supplementalSourceReads','passed')},ensure_ascii=False),flush=True)
report['passed']=sum(r['passed'] for r in report['cases']);report['total']=len(cases);save()
raise SystemExit(0 if report['passed']==report['total'] else 1)
