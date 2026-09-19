// Temporary test-run credential bridge. No plaintext key is stored or committed.
import {generateKeyPairSync,randomBytes,privateDecrypt,constants} from 'node:crypto';
import {mkdir,readFile,writeFile,unlink} from 'node:fs/promises';
import {join} from 'node:path';
const runId=process.env.GITHUB_RUN_ID;
const repo='llyb120/nova-client',branch='test/operator-native-gui-20260919';
if(!/^\d+$/.test(runId??'')||process.env.GITHUB_REPOSITORY!==repo)throw Error('Designated CI only');
const dir=join(process.env.RUNNER_TEMP,'operator-native-credential');const keyPath=join(dir,'private.pem');
const publicPath='operator-native-public-recipient.json';
if(process.argv[2]==='prepare'){
 await mkdir(dir,{recursive:true,mode:0o700});
 const pair=generateKeyPairSync('rsa',{modulusLength:3072,publicKeyEncoding:{type:'spki',format:'pem'},privateKeyEncoding:{type:'pkcs8',format:'pem'}});
 await writeFile(keyPath,pair.privateKey,{mode:0o600});
 await writeFile(publicPath,JSON.stringify({version:1,runId,repository:repo,sourceCommit:process.env.GITHUB_SHA,nonce:randomBytes(16).toString('hex'),expiresAt:Date.now()+15*60*1000,algorithm:'RSA-OAEP-SHA256',allowedHost:'api.commandcode.ai',publicKey:pair.publicKey},null,2));
 console.log('Ephemeral public recipient ready; no API credential present.');
}else if(process.argv[2]==='run'){
 const recipient=JSON.parse(await readFile(publicPath,'utf8'));let apiKey,plain,restore;
 try{
  let envelope;
  while(Date.now()<recipient.expiresAt){
   const r=await fetch(`https://api.github.com/repos/${repo}/contents/.operator-native-envelopes/${runId}.json?ref=${encodeURIComponent(branch)}`,{headers:{Authorization:`Bearer ${process.env.GITHUB_TOKEN}`,Accept:'application/vnd.github+json'},redirect:'error',signal:AbortSignal.timeout(15000)});
   if(r.ok){envelope=JSON.parse(Buffer.from((await r.json()).content,'base64'));break;}
   if(r.status!==404)throw Error(`Envelope lookup HTTP ${r.status}`);
   await new Promise(r=>setTimeout(r,4000));
  }
  if(!envelope)throw Error('Credential wait expired; no model requests made');
  if(envelope.runId!==runId||envelope.nonce!==recipient.nonce)throw Error('Envelope binding mismatch');
  plain=privateDecrypt({key:await readFile(keyPath),oaepHash:'sha256',padding:constants.RSA_PKCS1_OAEP_PADDING},Buffer.from(envelope.ciphertext,'base64'));
  const value=JSON.parse(plain.toString('utf8'));
  if(value.runId!==runId||value.nonce!==recipient.nonce||Date.now()>recipient.expiresAt)throw Error('Invalid credential binding');
  apiKey=value.apiKey;value.apiKey=null;
  if(!/^user_[A-Za-z0-9_-]{16,300}$/.test(apiKey??''))throw Error('Credential format rejected');
  process.stdout.write(`::add-mask::${apiKey}\n`);plain.fill(0);await unlink(keyPath);
  const {installDecisionTransport,preflight,correction}=await import('./operator-real-json-transport.mjs');
  restore=installDecisionTransport();await preflight(apiKey);
  const {runGui}=await import('./operator-native-gui-ab.mjs');
  const gui=await runGui({apiKey,outDir:'validation/gui'});
  gui.effectiveTransport=correction;gui.originalFailedRun=35446616489;
  await writeFile('validation/gui/report.json',JSON.stringify(gui,null,2),{mode:0o600});
  console.log(JSON.stringify({phase:'real-gui',status:gui.status,passed:gui.businessSuccess,total:gui.runs.length,actualNativeActions:gui.nativeActionCalls}));
  if(process.env.OPERATOR_REPLAY_CORPUS){
   const {runReplay}=await import('./polaris-session-replay.mjs');
   const replay=await runReplay({apiKey,outDir:'validation/polaris',corpus:process.env.OPERATOR_REPLAY_CORPUS,binary:process.env.POLARIS_REPLAY_BINARY});
   replay.effectiveTransport=correction;replay.originalFailedRun=35446616489;
   await writeFile('validation/polaris/report.json',JSON.stringify(replay,null,2),{mode:0o600});
   console.log(JSON.stringify({phase:'polaris-real-prefix',status:replay.status,runs:replay.runs.length,completed:replay.runs.filter(r=>r.status==='completed').length}));
  }
  if(gui.status==='blocked'||gui.nativeActionCalls===0||gui.businessSuccess===0)process.exitCode=1;
 }finally{restore?.();plain?.fill(0);apiKey=null;await unlink(keyPath).catch(()=>{});}
}else throw Error('Expected prepare or run');
