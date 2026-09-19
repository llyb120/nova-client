/** One-time encrypted credential delivery for a single, audited CI run.
 * Private key stays in RUNNER_TEMP, is never cached/uploaded, and is deleted before inference.
 * Only public-key metadata and RSA-OAEP ciphertext can appear in git/artifacts.
 */
import { generateKeyPairSync, randomBytes, privateDecrypt, constants } from 'node:crypto';
import { mkdir, writeFile, readFile, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { runBenchmark } from './operator-commandcode-ab.mjs';

const dir=join(process.env.RUNNER_TEMP??'/tmp','operator-one-time-recipient');
const keyPath=join(dir,'private.pem');
const publicPath='operator-public-recipient.json';
const runId=process.env.GITHUB_RUN_ID;
if(!runId||!/^\d+$/.test(runId))throw new Error('Run only inside the designated CI workflow');
if(process.argv[2]==='prepare'){
  await mkdir(dir,{recursive:true,mode:0o700});
  const {publicKey,privateKey}=generateKeyPairSync('rsa',{modulusLength:3072,publicKeyEncoding:{type:'spki',format:'pem'},privateKeyEncoding:{type:'pkcs8',format:'pem'}});
  await writeFile(keyPath,privateKey,{mode:0o600});
  const recipient={version:1,algorithm:'RSA-OAEP-SHA256',runId,repository:process.env.GITHUB_REPOSITORY,sourceCommit:process.env.GITHUB_SHA,nonce:randomBytes(16).toString('hex'),expiresAt:Date.now()+15*60*1000,allowedHost:'api.commandcode.ai',publicKey};
  await writeFile(publicPath,JSON.stringify(recipient,null,2));console.log('One-time public recipient is ready; no credential has been provided.');
}else if(process.argv[2]==='run'){
  const recipient=JSON.parse(await readFile(publicPath,'utf8'));
  const path=`.operator-test-envelopes/${runId}.json`;
  const repo=process.env.GITHUB_REPOSITORY;
  if(repo!=='llyb120/nova-client')throw new Error('Unexpected repository');
  const ref='work/operator-commandcode-ab-20260919';
  let encrypted,plain,apiKey;
  try{
    while(Date.now()<recipient.expiresAt){
      const response=await fetch(`https://api.github.com/repos/${repo}/contents/${path}?ref=${encodeURIComponent(ref)}`,{headers:{Authorization:`Bearer ${process.env.GITHUB_TOKEN}`,Accept:'application/vnd.github+json'},redirect:'error',signal:AbortSignal.timeout(15000)});
      if(response.ok){const file=await response.json();encrypted=JSON.parse(Buffer.from(file.content,'base64').toString('utf8'));break;}
      if(response.status!==404)throw new Error(`Encrypted envelope lookup HTTP ${response.status}`);
      await new Promise(r=>setTimeout(r,4000));
    }
    if(!encrypted)throw new Error('No credential envelope received before expiry; no model calls made');
    if(encrypted.runId!==runId||encrypted.nonce!==recipient.nonce)throw new Error('Envelope is not for this CI run');
    plain=privateDecrypt({key:await readFile(keyPath),oaepHash:'sha256',padding:constants.RSA_PKCS1_OAEP_PADDING},Buffer.from(encrypted.ciphertext,'base64'));
    const value=JSON.parse(plain.toString('utf8'));
    if(value.runId!==runId||value.nonce!==recipient.nonce||Date.now()>recipient.expiresAt)throw new Error('Invalid or expired credential binding');
    apiKey=value.apiKey;value.apiKey=null;
    if(typeof apiKey!=='string'||!/^user_[A-Za-z0-9_-]{16,300}$/.test(apiKey))throw new Error('Credential format rejected');
    // GitHub workflow command, not a normal log line. Never interpolate secrets in YAML or argv.
    process.stdout.write(`::add-mask::${apiKey}\n`);
    await unlink(keyPath);plain.fill(0);
    const report=await runBenchmark({apiKey,fixturesPath:'validation/operator-cases.json',outPath:'validation/operator-commandcode-ab.json',repeats:2});
    console.log(JSON.stringify({status:report.status,modelCallsAttempted:report.modelCallsAttempted,modelCallsSucceeded:report.modelCallsSucceeded}));
    if(report.status==='blocked')process.exitCode=1;
  }finally{plain?.fill(0);apiKey=null;await unlink(keyPath).catch(()=>{});}
}else{throw new Error('Expected prepare or run');}
