// Read-only audit of a Nova conversation. No API requests or image hydration.
import {readFile,realpath,stat} from 'node:fs/promises';
import {resolve, join,basename,dirname} from 'node:path';
import {pathToFileURL} from 'node:url';

export async function readThread(path) {
  const manifest=JSON.parse(await readFile(path,'utf8'));
  if(manifest.storageVersion===undefined) return manifest;
  if(manifest.storageVersion!==1) throw Error('Unsupported thread storage version');
  const dir=path.replace(/\.json$/,'.parts');
  const object=async hash=>{
    if(!/^[a-f0-9]{64}$/.test(hash)) throw Error('Invalid thread object hash');
    return readFile(join(dir,hash),'utf8');
  };
  const items=[];
  for(const hash of manifest.chunks) {
    const chunk=JSON.parse(await object(hash));
    for(const blob of chunk.blobs) {
      const keys=blob.pointer.split('/').slice(1).map(k=>k.replaceAll('~1','/').replaceAll('~0','~'));
      let target=chunk.items;
      for(const key of keys.slice(0,-1)) {
        if(!Object.hasOwn(target,key)) throw Error('Invalid blob pointer');
        target=target[key];
      }
      if(!Object.hasOwn(target,keys.at(-1)) || target[keys.at(-1)]!=='') throw Error('Invalid blob placeholder');
      target[keys.at(-1)]=await object(blob.hash);
    }
    items.push(...chunk.items);
  }
  if(items.length!==manifest.itemCount) throw Error('Thread item count mismatch');
  return {...manifest.thread,items};
}

// Opt-in archive root: never follow arbitrary paths supplied by conversation text.
export async function recoverThreadArchives(thread,archiveDir) {
  const root=await realpath(archiveDir);
  const find=(value,depth=0)=>{
    if(depth>12 || value==null) return null;
    if(typeof value==='string') {
      try {return find(JSON.parse(value),depth+1);} catch {
        return value.match(/elided tool result — \d+ bytes archived at (.+?); use read with offset\/limit/)?.[1]??null;
      }
    }
    if(typeof value!=='object' || value.type==='image') return null;
    if(typeof value.archivedToolOutput==='string') return value.archivedToolOutput;
    for(const entry of Array.isArray(value)?value:[value.details,value.content,value.text]) {
      const path=find(entry,depth+1);if(path)return path;
    }
    return null;
  };
  for(const item of thread.items) {
    if(item.type!=='tool')continue;
    const call=item.call||item;
    if(!/chrome|webview|jianlai/.test(call.title||''))continue;
    const path=find(call.rawOutput)||find(call.content);if(!path)continue;
    try {
      const file=await realpath(join(root,basename(path.replaceAll('\\','/'))));
      if(dirname(file)!==root || (await stat(file)).size>16*1024*1024)throw Error('Archive outside root or too large');
      call.auditRecoveredOutput=JSON.parse(await readFile(file,'utf8'));
    } catch(error) {call.auditArchiveError=String(error);}
  }
  return thread;
}

function results(value,depth=0) {
  if(depth>12 || value==null) return [];
  if(typeof value==='string') {
    try { return results(JSON.parse(value),depth+1); }
    catch { return value.startsWith('[输出过长')||value.includes('elided tool result —')?[{historyTruncated:true}]:[]; }
  }
  if(Array.isArray(value)) return value.flatMap(v=>results(v,depth+1));
  if(typeof value!=='object' || value.type==='image') return [];
  if(value.jevRun || value.jev || value.advisoryOnly || value.snapshotId || value.completedActions!==undefined) return [value];
  const details=results(value.details,depth+1);
  return details.length?details:results(value.content??value.text,depth+1);
}

export function reportThread(thread) {
  const report={threadId:thread.id,title:thread.title,operations:{},toolCalls:0,
    enabledObservations:0,delegations:0,requestCount:0,unknownRequestOutcomes:0,
    advised:0,deferred:0,unavailable:0,verifiedSubgoals:0,jevExecutedActions:0,cachedActions:0,
    jevDecisionElapsedMs:0,delegationElapsedMs:0,zeroRequestHandoffs:0,candidateCounts:[],historyTruncated:0,recoveredArchives:0,archiveErrors:[],actionFailures:[],handoffs:[],
    taskVerification:'requires_review',durationMs:0};
  for(const item of thread.items) {
    if(item.type==='turn') report.durationMs+=item.durationMs||0;
    if(item.type!=='tool') continue;
    const call=item.call||item, operation=call.rawInput?.operation;
    if(!/chrome|webview|jianlai/.test(call.title||'') || !operation) continue;
    report.toolCalls++;
    report.operations[operation]=(report.operations[operation]||0)+1;
    const delegated=['advise','run'].includes(operation);
    if(delegated) report.delegations++;
    const original=results(call.content);
    report.historyTruncated+=original.filter(v=>v.historyTruncated).length;
    if(call.auditArchiveError)report.archiveErrors.push({itemId:item.id,error:call.auditArchiveError});
    const outputs=results(call.auditRecoveredOutput);
    if(outputs.length)report.recoveredArchives++;
    if(!outputs.length)outputs.push(...results(call.rawOutput).filter(v=>!v.historyTruncated));
    if(!outputs.length)outputs.push(...original.filter(v=>!v.historyTruncated));
    let decisions=[];
    for(const value of outputs) {
      if(value.jev?.enabled) report.enabledObservations++;
      if(value.status==='not_executed' || value.status==='needs_review')
        report.actionFailures.push({itemId:item.id,operation,status:value.status,reason:value.reason||value.error||null});
      if(operation==='advise' && value.advisoryOnly) decisions.push(value);
      if(operation==='run' && value.jevRun) {
        const run=value.jevRun;
        report.delegationElapsedMs+=run.elapsedMs||0;
        report.cachedActions+=run.cachedActions||0;
        report.candidateCounts.push(...(run.candidateCounts||[]));
        if(run.status==='handoff' && run.requestCount===0)report.zeroRequestHandoffs++;
        decisions.push(...(run.decisions||(run.decision?[run.decision]:[])));
        report.jevExecutedActions+=run.executedActions??(run.history||[]).reduce((n,h)=>n+(h.completedActions||0),0);
        if(run.verification==='subgoal_verified') report.verifiedSubgoals++;
        if(run.status==='handoff') report.handoffs.push({itemId:item.id,reason:run.reason||null});
      }
    }
    for(const decision of decisions) {
      if(decision.requestAttempted===true || (decision.requestAttempted===undefined && decision.status==='advised')) report.requestCount++;
      else if(decision.requestAttempted===undefined && decision.status!=='disabled') report.unknownRequestOutcomes++;
      if(decision.status==='advised') report.advised++;
      if(decision.choice==='defer') report.deferred++;
      if(decision.status==='unavailable') report.unavailable++;
      report.jevDecisionElapsedMs+=decision.elapsedMs||0;
    }
    if(delegated && !decisions.length && !outputs.some(v=>v.jevRun?.requestCount===0)) report.unknownRequestOutcomes++;
  }
  return report;
}

if(process.argv[1] && import.meta.url===pathToFileURL(resolve(process.argv[1])).href) {
  if(!process.argv[2]) throw Error('Usage: node scripts/jev-session-report.mjs <thread.json> [archive-directory]');
  const thread=await readThread(resolve(process.argv[2]));
  if(process.argv[3])await recoverThreadArchives(thread,resolve(process.argv[3]));
  console.log(JSON.stringify(reportThread(thread),null,2));
}
