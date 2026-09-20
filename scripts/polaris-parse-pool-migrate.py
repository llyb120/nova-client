"""Replace per-4-file parse barriers with one bounded worker pool.
Candidate set, file count, declaration scanner, ranking, dependency closure and output
are unchanged. Baseline is the current landed 20/20 dependency-prune source.
"""
from pathlib import Path
import hashlib
p=Path("src-tauri/src/nova_tools_native/polaris_demand_v2.rs")
b=p.read_bytes()
got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
assert got=="0f5fe46653dd5248151468b0e32e536ce280681f",got
s=b.decode("utf-8")
old='''    for batch in work.chunks(4){
        if Instant::now()>=deadline{for &id in batch{parsed.remove(&id);}partial=true;continue;}
        let built=thread::scope(|scope|{
            let jobs=batch.iter().map(|&id|scope.spawn(move ||(id,build_light(&rows[id])))).collect::<Vec<_>>();
            jobs.into_iter().filter_map(|job|job.join().ok()).collect::<Vec<_>>()
        });
        if built.len()!=batch.len(){partial=true;}
        for (id,file) in built{cache.entries.insert(rows[id].file.clone(),file);stats.reparsed_files+=1;}
    }'''
new='''    if !work.is_empty() {
        if Instant::now()>=deadline {for &id in &work{parsed.remove(&id);}partial=true;}
        else {
            // One bounded pool removes the per-4-file barrier: a large file no
            // longer prevents another worker from starting the next small file.
            // Results are sorted before publication, so retrieval stays deterministic.
            let workers=thread::available_parallelism().map(|n|n.get()).unwrap_or(4).clamp(1,8).min(work.len());
            let cursor=std::sync::atomic::AtomicUsize::new(0);
            let output=std::sync::Mutex::new(Vec::<(usize,LightFile)>::with_capacity(work.len()));
            thread::scope(|scope|{
                for _ in 0..workers {
                    let output=&output;let cursor=&cursor;let work=&work;
                    scope.spawn(move || loop {
                        let n=cursor.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                        let Some(&id)=work.get(n)else{break;};
                        if Instant::now()>=deadline{break;}
                        let file=build_light(&rows[id]);
                        output.lock().unwrap().push((id,file));
                    });
                }
            });
            let mut built=output.into_inner().unwrap_or_else(|poisoned|poisoned.into_inner());
            built.sort_by_key(|(id,_)|*id);
            if built.len()!=work.len(){partial=true;let done=built.iter().map(|(id,_)|*id).collect::<HashSet<_>>();for &id in &work{if !done.contains(&id){parsed.remove(&id);}}}
            for (id,file) in built{cache.entries.insert(rows[id].file.clone(),file);stats.reparsed_files+=1;}
        }
    }'''
assert s.count(old)==1
s=s.replace(old,new)
p.write_text(s,encoding="utf-8")
print("Replaced chunk barriers with a deterministic bounded parse worker pool.")
