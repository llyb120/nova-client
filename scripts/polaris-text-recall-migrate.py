"""Restore AST-independent literal recall on the frozen query-first baseline.
Only this exact source baseline is accepted; labels, defaults and Reasonix stay unchanged.
"""
from pathlib import Path
import hashlib
root=Path('src-tauri/src/nova_tools_native')
def load(name,expected):
    path=root/name;data=path.read_bytes()
    blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
    assert blob==expected,(name,blob)
    return path,data.decode('utf-8')
def sub(s,a,b):
    assert s.count(a)==1,a[:120]
    return s.replace(a,b)
p,s=load('polaris_demand_v2.rs','6da02a98e879263e3bef677f343fc65ed1a03dd0')
s='include!("polaris_text.rs");\n'+s
s=sub(s,'pub candidate_declarations:usize,pub materialized_declarations:usize,','pub candidate_declarations:usize,pub materialized_declarations:usize,\n    pub literal_ranges:usize,')
s=sub(s,'if is_searchable_implementation_file(&file)&&(role=="implementation"||(q.test_intent&&role=="test")||(q.doc_intent&&role=="documentation")){','if literal_candidate(&file,q){')
s=sub(s,'        let role=file_role(&file);\n','')
s=sub(s,'let text=match fs::read_to_string(&path){Ok(t)=>t,Err(_)=>{partial=true;continue;}};', '''let bytes=match fs::read(&path){Ok(b)=>b,Err(_)=>{partial=true;continue;}};
        stats.scanned_bytes+=bytes.len() as u64;
        if bytes.contains(&0){continue;}
        let text=match String::from_utf8(bytes){Ok(t)=>t,Err(_)=>continue};''')
s=sub(s,'stats.scanned_files+=1;stats.scanned_bytes+=text.len() as u64;','stats.scanned_files+=1;')
s=sub(s,'for &hit in &hits{df[hit]+=1;}','if structural_candidate(&file,q){for &hit in &hits{df[hit]+=1;}}')
s=sub(s,'let n=rows.len().max(1) as f64;','let n=rows.iter().filter(|r|structural_candidate(&r.file,q)).count().max(1) as f64;')
s=sub(s,'let mut order=(0..rows.len()).filter(|&i|rows[i].score>0.0).collect::<Vec<_>>();','let mut order=(0..rows.len()).filter(|&i|rows[i].score>0.0&&structural_candidate(&rows[i].file,q)).collect::<Vec<_>>();')
s=sub(s,'(explicit||caller).then_some((i,if explicit{10000.0+r.score}else{r.score}))','((explicit||caller)&&structural_candidate(&r.file,q)).then_some((i,if explicit{10000.0+r.score}else{r.score}))')
s=sub(s,'    let c=subset(units,parsed.len(),stats.reparsed_files,partial);','''    let (literal,literal_partial)=literal_units(&rows,&units,q,deadline)?;
    stats.literal_ranges=literal.len();partial|=literal_partial;units.extend(literal);
    let c=subset(units,parsed.len(),stats.reparsed_files,partial);''')
p.write_text(s,encoding='utf-8')
p,s=load('polaris.rs','a0c0c01c045267824e38f329bcbb6cafa4a27f60')
s=sub(s,'u.role=="implementation"||(q.test_intent','matches!(u.role,"implementation"|"source-text")||(q.test_intent')
s=sub(s,'    ranked.truncate(32);','    index::prioritize_literal(&mut ranked,&units,q);\n    ranked.truncate(32);')
p.write_text(s,encoding='utf-8')
p,s=load('polaris_packet.rs','2c25309b0a6c2f77762ed74c5aec492dbb8a05b9')
s=sub(s,'let complete=start<=u.owner_start&&end>=u.owner_end;', 'let complete=start<=u.owner_start&&end>=u.owner_end&&(u.role!="source-text"||(start==1&&end==u.source.len()));')
s=sub(s,'if complete{"BODY"}else{"PARTIAL"}', 'if u.role=="source-text"{"SOURCE_RANGE"}else if complete{"BODY"}else{"PARTIAL"}')
s=sub(s,'"sourceHash":u.file_hash,"complete":complete', '"sourceHash":u.file_hash,"complete":complete,"sourceRange":u.role=="source-text"')
s=sub(s,'"reason":"large-unit"', '"reason":if u.role=="source-text"{"source-range-only"}else{"large-unit"}')
p.write_text(s,encoding='utf-8')
p=root/'polaris_text.rs';s=p.read_text(encoding='utf-8')
s=sub(s,'start:start+1,end,owner_start:1,owner_end:source.len()', 'start:start+1,end,owner_start:start+1,owner_end:end')
s=sub(s,'let mut selected = Vec::new(); let mut partial = false;', 'let mut selected = Vec::new(); let mut partial = candidates.len()>16;')
s=sub(s,'    selected.truncate(4);', '    partial |= selected.len()>4;\n    selected.truncate(4);')
p.write_text(s,encoding='utf-8')
print('Restored source-text recall in one request; no global index or API call.')
