import assert from 'node:assert/strict';
import {test} from 'node:test';
import {reportThread,recoverThreadArchives} from './jev-session-report.mjs';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

test('Lyra elision is recovered once from an explicit archive root',async()=>{
  const dir=await mkdtemp(join(tmpdir(),'jev-audit-'));
  try {
    const value={snapshotId:'s',jevRun:{requestCount:2,executedActions:1,status:'handoff',decisions:[
      {status:'advised',choice:'action_0',requestAttempted:true,elapsedMs:909},
      {status:'advised',choice:'defer',requestAttempted:true,elapsedMs:529}]}};
    await writeFile(join(dir,'call-chrome.txt'),JSON.stringify(value));
    const thread={items:[{type:'tool',call:{title:'chrome',rawInput:{operation:'run'},content:[{type:'text',text:'{"broken": …[elided tool result — 50000 bytes archived at '+join(dir,'call-chrome.txt')+'; use read with offset/limit to inspect it] tail'}]}}]};
    assert.equal(reportThread(thread).historyTruncated,1);
    await recoverThreadArchives(thread,dir);
    const report=reportThread(thread);
    assert.equal(report.recoveredArchives,1);assert.equal(report.requestCount,2);
    assert.equal(report.jevExecutedActions,1);assert.equal(report.deferred,1);
    assert.equal(report.jevDecisionElapsedMs,1438);assert.equal(report.unknownRequestOutcomes,0);
    thread.items[0].call.rawOutput={details:value};
    assert.equal(reportThread(thread).requestCount,2,'never double count raw/recovered results');
    delete thread.items[0].call.auditRecoveredOutput;
    assert.equal(reportThread(thread).requestCount,2,'structured details survive an elided display');
  } finally {await rm(dir,{recursive:true,force:true});}
});

test('enabled is not a request; handoff and execution are not task success',()=>{
  const tool=(operation,value)=>({type:'tool',title:'mcp__nova_tools__chrome',rawInput:{operation},content:[{type:'content',content:{type:'text',text:JSON.stringify([{type:'text',text:JSON.stringify(value)}])}}]});
  const report=reportThread({items:[
    tool('inspect',{jev:{enabled:true},snapshotId:'s1'}),
    tool('run',{jevRun:{requestCount:0,decisions:[],status:'handoff',reason:'observation gap',elapsedMs:6}}),
    tool('advise',{status:'advised',advisoryOnly:true,requestAttempted:true,choice:'defer',elapsedMs:50}),
    tool('run',{jevRun:{status:'completed',verification:'subgoal_verified',executedActions:2,decisions:[{status:'advised',requestAttempted:true,choice:'done',elapsedMs:80}]}}),
    {type:'turn',durationMs:200}
  ]});
  assert.equal(report.enabledObservations,1);assert.equal(report.delegations,3);
  assert.equal(report.requestCount,2);assert.equal(report.unknownRequestOutcomes,0);
  assert.equal(report.deferred,1);assert.equal(report.verifiedSubgoals,1);
  assert.equal(report.jevExecutedActions,2);assert.equal(report.jevDecisionElapsedMs,130);
  assert.equal(report.taskVerification,'requires_review');assert.equal(report.handoffs[0].reason,'observation gap');
  assert.equal(report.zeroRequestHandoffs,1);assert.equal(report.delegationElapsedMs,6);
});

test('validated path reuse is a decision but not another model request',()=>{
  const value={jevRun:{status:'completed',requestCount:1,executedActions:2,cachedActions:1,decisions:[
    {status:'advised',requestAttempted:true,choice:'action_0',elapsedMs:500},
    {status:'advised',requestAttempted:false,choice:'action_1',source:'validated_path'}]}};
  const report=reportThread({items:[{type:'tool',call:{title:'chrome',rawInput:{operation:'run'},rawOutput:value}}]});
  assert.equal(report.requestCount,1);
  assert.equal(report.advised,2);
  assert.equal(report.cachedActions,1);
  assert.equal(report.jevExecutedActions,2);
  assert.equal(report.jevDecisionElapsedMs,500);
});
