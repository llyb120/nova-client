"""Compare the currently accepted one-shot engine against marginal-goal working-set selection.
Uses the already-fixed regression labels; this is an engineering gate, not blind accuracy.
"""
import argparse,hashlib,importlib.util,json,os,pathlib,statistics,tempfile
P=pathlib.Path
spec=importlib.util.spec_from_file_location("ab",P("scripts/polaris-ab.py"))
ab=importlib.util.module_from_spec(spec);spec.loader.exec_module(ab)
p=argparse.ArgumentParser()
p.add_argument("--current",type=P,required=True)
p.add_argument("--candidate",type=P,required=True)
p.add_argument("--accepted",type=P,required=True)
p.add_argument("--corpus",type=P,required=True)
p.add_argument("--out",type=P,default=P("validation/working-set-cover"))
a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
current=a.current.resolve();candidate=a.candidate.resolve()
assert hashlib.sha256(current.read_bytes()).hexdigest()=="462e6ed7223ac058924b574c783dcd2b26d9d4df143a0b21521df19f70a6e53e"
assert hashlib.sha256(candidate.read_bytes()).digest()!=hashlib.sha256(current.read_bytes()).digest()
for b in (current,candidate): b.chmod(b.stat().st_mode|0o111)
labels=json.loads((a.accepted/"fastcontext/labels-before-run.json").read_text(encoding="utf-8"))
labels=[x for x in labels if x["group"] in ("regression","closure")]
env={k:v for k,v in os.environ.items() if not k.startswith("NOVA_POLARIS_")}
report={"method":__doc__,"answerModelCalls":0,"budgetBytes":12000,"rows":[],"binaries":{
 "current":hashlib.sha256(current.read_bytes()).hexdigest(),
 "candidate":hashlib.sha256(candidate.read_bytes()).hexdigest(),
}}
def save():(a.out/"report.json").write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
def run(engine,c):
    r=engine.ask({"root":str(a.corpus.resolve()),"mode":"candidate","params":c["params"]})
    text=r.get("result",{}).get("text","")
    sections=ab.sections(text);m=ab.assess(c,text)
    missing=[g for g in c["primary"]+c["support"] if not any(ab.matched(s,g) for s in sections)]
    m["allRequired"]=bool(c["primary"]) and not missing
    return r,text,m
with tempfile.TemporaryDirectory(prefix="polaris-cover-") as tmp:
 cache=P(tmp)
 for arm,binary in [("current",current),("candidate",candidate)]:
  # Each query gets a fresh process/cache for first-query latency and coverage.
  for c in labels:
   e=ab.Engine(binary,{**env,"NOVA_DATA_DIR":str(cache/f"cold-{arm}-{c['id']}")},a.out/f"cold-{arm}-{c['id']}.log")
   try:r,text,m=run(e,c)
   finally:e.close()
   raw=a.out/"raw"/c["id"];raw.mkdir(parents=True,exist_ok=True)
   (raw/f"{arm}-cold.txt").write_text(text,encoding="utf-8")
   report["rows"].append({"arm":arm,"phase":"cold","id":c["id"],"group":c["group"],"kind":c["kind"],"ok":r.get("ok"),"ms":r.get("ms"),"bytes":len(text.encode()),"metrics":m});save()
  # Same resident process: different questions first, then repeated warm questions.
  e=ab.Engine(binary,{**env,"NOVA_DATA_DIR":str(cache/f"resident-{arm}")},a.out/f"resident-{arm}.log")
  try:
   for phase in ["residentFirst","warm"]:
    for c in labels:
     r,text,m=run(e,c)
     raw=a.out/"raw"/c["id"];raw.mkdir(parents=True,exist_ok=True)
     (raw/f"{arm}-{phase}.txt").write_text(text,encoding="utf-8")
     report["rows"].append({"arm":arm,"phase":phase,"id":c["id"],"group":c["group"],"kind":c["kind"],"ok":r.get("ok"),"ms":r.get("ms"),"bytes":len(text.encode()),"metrics":m});save()
  finally:e.close()
def pct(values,p):
    values=sorted(values);return values[min(len(values)-1,max(0,int(len(values)*p+0.999999)-1))]
report["summary"]={}
for arm in ["current","candidate"]:
 natural=[r for r in report["rows"] if r["arm"]==arm and r["phase"]=="cold" and r["group"]=="regression" and r["kind"]=="natural"]
 closure=[r for r in report["rows"] if r["arm"]==arm and r["phase"]=="cold" and r["group"]=="closure"]
 resident=[r for r in report["rows"] if r["arm"]==arm and r["phase"]=="residentFirst" and r["group"]=="regression" and r["kind"]=="natural"]
 warm=[r for r in report["rows"] if r["arm"]==arm and r["phase"]=="warm" and r["group"]=="regression" and r["kind"]=="natural"]
 report["summary"][arm]={
  "naturalQueries":len(natural),
  "top1":sum(r["metrics"]["top1"] for r in natural),
  "coreBody":sum(r["metrics"]["coreBody"] for r in natural),
  "closure":sum(r["metrics"]["allRequired"] for r in closure),
  "coldP50Ms":statistics.median(r["ms"] for r in natural),
  "coldP95Ms":pct([r["ms"] for r in natural],.95),
  "residentFirstP50Ms":statistics.median(r["ms"] for r in resident),
  "warmP50Ms":statistics.median(r["ms"] for r in warm),
  "maxBytes":max(r["bytes"] for r in natural),
 }
missing=[r["id"] for r in report["rows"] if r["arm"]=="candidate" and r["phase"]=="cold" and r["group"]=="regression" and r["kind"]=="natural" and not r["metrics"]["coreBody"]]
c=report["summary"]["candidate"];b=report["summary"]["current"]
checks={
 "all_requests_returned":all(r["ok"] for r in report["rows"]),
 "natural_core_20_of_20":c["coreBody"]==20,
 "top1_not_lower":c["top1"]>=b["top1"],
 "four_dependency_closures":c["closure"]==4,
 "fixed_budget":all(r["bytes"]<=12000 for r in report["rows"] if r["arm"]=="candidate"),
 "cold_p50_not_over_350ms":c["coldP50Ms"]<=350,
 "cold_p50_not_over_135pct_current":c["coldP50Ms"]<=b["coldP50Ms"]*1.35,
 "resident_new_question_p50_not_over_220ms":c["residentFirstP50Ms"]<=220,
 "warm_p50_under_100ms":c["warmP50Ms"]<=100,
}
report["missingNatural"]=missing;report["checks"]=checks;report["passed"]=all(checks.values());save()
print(json.dumps({"summary":report["summary"],"missingNatural":missing,"checks":checks,"passed":report["passed"]},ensure_ascii=False,indent=2))
raise SystemExit(0 if report["passed"] else 1)
