// Reproduce the corrected adapter run. Test configuration only, no Nova settings.
// COMMAND_CODE_API_KEY is read once then removed before any child process starts.
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {installDecisionTransport,preflight,correction} from './operator-real-json-transport.mjs';
import {runGui} from './operator-native-gui-ab.mjs';
import {runReplay} from './polaris-session-replay.mjs';
let apiKey=process.env.COMMAND_CODE_API_KEY;
delete process.env.COMMAND_CODE_API_KEY;
delete process.env.GITHUB_TOKEN;
delete process.env.GH_TOKEN;
if(!/^user_[A-Za-z0-9_-]{16,300}$/.test(apiKey??''))throw Error('Set COMMAND_CODE_API_KEY in your local environment');
const out=resolve(process.env.OPERATOR_TEST_OUTPUT||'validation');await mkdir(out,{recursive:true});
const restore=installDecisionTransport();
try{
 await preflight(apiKey,out);
 const gui=await runGui({apiKey,outDir:resolve(out,'gui')});gui.effectiveTransport=correction;
 await writeFile(resolve(out,'gui/report.json'),JSON.stringify(gui,null,2),{mode:0o600});
 console.log(JSON.stringify({phase:'real-gui',tasks:gui.runs.length,success:gui.businessSuccess,actualNativeActions:gui.nativeActionCalls}));
 if(process.env.OPERATOR_REPLAY_CORPUS&&process.env.POLARIS_REPLAY_BINARY){
  const replay=await runReplay({apiKey,outDir:resolve(out,'polaris'),corpus:process.env.OPERATOR_REPLAY_CORPUS,binary:process.env.POLARIS_REPLAY_BINARY});replay.effectiveTransport=correction;
  await writeFile(resolve(out,'polaris/report.json'),JSON.stringify(replay,null,2),{mode:0o600});
  console.log(JSON.stringify({phase:'polaris',episodes:replay.runs.length,finished:replay.runs.filter(r=>r.status==='completed').length}));
 }
 if(gui.status==='blocked'||gui.nativeActionCalls===0||gui.businessSuccess===0)process.exitCode=1;
}finally{restore();apiKey=null;}
