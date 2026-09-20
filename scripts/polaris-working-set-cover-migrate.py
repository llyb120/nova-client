"""Reduce dependency fan-out after the landed 20/20 intent-champion selection.
Keep all declaration selection behavior unchanged; only stop expanding dependencies from
lower-ranked fifth/sixth seeds. Quality gates must remain 20/20 and 4/4 before landing.
"""
from pathlib import Path
import hashlib
ROOT=Path("src-tauri/src/nova_tools_native")
def load(name,expected):
    p=ROOT/name;b=p.read_bytes()
    got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
    assert got==expected,(name,got)
    return p,b.decode("utf-8")
def sub(s,a,b):
    assert s.count(a)==1,a[:180]
    return s.replace(a,b)

p,s=load("polaris_demand_v2.rs","3c814ff3e30916e15b7e357bda2338a52c4aa341")
old='''        let mut expand=seeds.iter().take(6).map(|(id,_)|*id).collect::<Vec<_>>();'''
new='''        // Packet assembly exposes at most four primary roots. Expanding
        // dependencies from lower-ranked fifth/sixth recall seeds adds parse
        // work that cannot become a primary working-set root unless reached by
        // a verified edge from a stronger seed. Keep four seed frontiers; direct
        // dependencies still recurse for three bounded rounds below.
        let mut expand=seeds.iter().take(4).map(|(id,_)|*id).collect::<Vec<_>>();'''
s=sub(s,old,new)
p.write_text(s,encoding="utf-8")
print("Dependency discovery fan-out reduced from six recall seeds to four; depth and closure unchanged.")
