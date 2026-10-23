// WorkBuddy's own Electron runtime supplies the build key; no credential is written back to disk.
const { createHash, createDecipheriv } = require('node:crypto');
const { readFileSync, statSync } = require('node:fs');

function decodeWorkBuddyToken(value, secret) {
  if (typeof value === 'string') return value;
  if (value?.$wbEncrypted !== 1 || Object.keys(value).sort().join() !== '$wbEncrypted,envelope' || typeof secret !== 'string') throw Error('Unsupported WorkBuddy login encryption');
  const bytes = Buffer.from(value.envelope, 'base64');
  if (bytes.toString('base64') !== value.envelope) throw Error('Invalid WorkBuddy envelope');
  const sealed = JSON.parse(bytes.toString('utf8')), key = createHash('sha256').update(secret).digest();
  try {
    const keyId = createHash('sha256').update(key).digest('hex').slice(0, 16);
    if (sealed.suite !== 1 || sealed.keyId !== keyId) throw Error('WorkBuddy login key mismatch');
    const nonce = Buffer.from(sealed.nonce, 'base64'), tag = Buffer.from(sealed.authTag, 'base64');
    if (nonce.length !== 12 || tag.length !== 16) throw Error('Invalid WorkBuddy envelope');
    const u32 = n => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
    const text = s => { const b = Buffer.from(s); return Buffer.concat([u32(b.length), b]); };
    // ponytail: WorkBuddy 5.8.x sym-v1 field framing; reject new schemes until verified.
    const decipher = createDecipheriv('aes-256-gcm', key, nonce);
    decipher.setAAD(Buffer.concat([Buffer.from('WB-AAD\0'), Buffer.from([1]), text('WBEV1'), text('sym-v1'), u32(1), text(keyId), Buffer.from([2, 0, 0])]));
    decipher.setAuthTag(tag);
    return Buffer.concat([decipher.update(Buffer.from(sealed.ciphertext, 'base64')), decipher.final()]).toString('utf8');
  } finally { key.fill(0); }
}

exports.decodeWorkBuddyToken = decodeWorkBuddyToken;
if (process.argv[1] === '--nova-workbuddy-login') {
  try {
    const file = process.argv[2];
    if (statSync(file).size > 1024 * 1024) throw Error('Login too large');
    const session = JSON.parse(readFileSync(file, 'utf8'));
    const payload = JSON.parse(process._linkedBinding('electron_browser_workbuddy_storage').loggerGet());
    process.stdout.write(JSON.stringify({ accessToken: decodeWorkBuddyToken(session.auth?.accessToken, payload.atRestSecretKey), envelope: session.auth?.accessToken }));
  } catch { process.stderr.write('Cannot unlock WorkBuddy login. Open WorkBuddy and sign in again.\n'); process.exitCode = 1; }
}
