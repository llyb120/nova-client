"""Reduce cold parsing cost without shrinking dependency closure or intent coverage.
Keep the eight strongest files exactly as ranked. Fill the remaining eight initial parse slots from the
top lexical candidate pool using relevance per estimated parse cost. Dependency fan-out/depth
and the landed 20/20 declaration champion are unchanged.
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
    assert s.count(a)==1,a[:200]
    return s.replace(a,b)

p,s=load("polaris_demand_v2.rs","3c814ff3e30916e15b7e357bda2338a52c4aa341")
old='''    let mut parsed=HashSet::new();let initial=order.iter().copied().take(INITIAL_FILES).collect::<Vec<_>>();
    partial|=declarations(&rows,&initial,&mut cache,&mut parsed,&mut stats,deadline);'''
new='''    let mut parsed=HashSet::new();
    // Always retain the eight strongest lexical files. For the remaining cold
    // slots, prefer focused evidence that is cheaper to parse. This only changes
    // scheduling among already-discovered candidates; it does not narrow the
    // search surface, dependency frontier, or result budget.
    let mut initial=order.iter().copied().take(8).collect::<Vec<_>>();
    let fixed=initial.iter().copied().collect::<HashSet<_>>();
    let mut efficient=order.iter().copied().take(96).filter(|id|!fixed.contains(id)).collect::<Vec<_>>();
    efficient.sort_by(|a,b|{
        let value=|id:usize|{
            let kib=rows[id].text.len() as f64/65536.0;
            rows[id].score/(1.0+kib.sqrt())
        };
        value(*b).total_cmp(&value(*a)).then(rows[*a].file.cmp(&rows[*b].file))
    });
    initial.extend(efficient.into_iter().take(INITIAL_FILES.saturating_sub(initial.len())));
    partial|=declarations(&rows,&initial,&mut cache,&mut parsed,&mut stats,deadline);'''
s=sub(s,old,new)
p.write_text(s,encoding="utf-8")
print("Cold parse scheduling now keeps top-2 lexical files and fills remaining slots by relevance/parse-cost.")
