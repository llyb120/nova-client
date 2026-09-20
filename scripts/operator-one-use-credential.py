"""One-use RSA recipient owned by this runner. The private key is never uploaded."""
import base64, hashlib, json, os, re, subprocess, time, urllib.request
from pathlib import Path
root = Path(os.environ['RUNNER_TEMP']) / 'operator-key'
root.mkdir(mode=0o700, exist_ok=True)
private = root / 'private.pem'
public = root / 'public' / 'recipient.pem'
meta = public.parent / 'recipient.json'
if os.environ.get('KEY_STAGE') == 'prepare':
    public.parent.mkdir(exist_ok=True)
    subprocess.run(['openssl','genpkey','-algorithm','RSA','-pkeyopt','rsa_keygen_bits:3072','-out',str(private)],check=True,stderr=subprocess.DEVNULL)
    private.chmod(0o600)
    subprocess.run(['openssl','pkey','-in',str(private),'-pubout','-out',str(public)],check=True)
    meta.write_text(json.dumps({'runId':os.environ['GITHUB_RUN_ID'],'sourceSha':os.environ['OPTIMIZED_SHA'],'publicKeySha256':hashlib.sha256(public.read_bytes()).hexdigest()}))
    raise SystemExit(0)
expected=json.loads(meta.read_text())
url='https://api.github.com/repos/'+os.environ['GITHUB_REPOSITORY']+'/contents/.github/operator-optimization-envelope.json?ref=work%2Foperator-context-20260919'
key=None
try:
    for _ in range(450):
        try:
            req=urllib.request.Request(url,headers={'Authorization':'Bearer '+os.environ['GITHUB_TOKEN'],'Accept':'application/vnd.github+json','User-Agent':'operator-test-recipient'})
            with urllib.request.urlopen(req,timeout=15) as response: obj=json.load(response)
            env=json.loads(base64.b64decode(obj['content'],validate=False))
            if all(env.get(k)==v for k,v in expected.items()):
                ciphertext=base64.b64decode(env['ciphertext'],validate=True)
                assert len(ciphertext)==384
                key=subprocess.check_output(['openssl','pkeyutl','-decrypt','-inkey',str(private),'-pkeyopt','rsa_padding_mode:oaep','-pkeyopt','rsa_oaep_md:sha256'],input=ciphertext,stderr=subprocess.DEVNULL).decode()
                if not re.fullmatch(r'user_[A-Za-z0-9_-]{16,300}',key): raise ValueError('invalid key shape')
                break
        except Exception:
            pass
        time.sleep(2)
    if key is None: raise SystemExit('One-use model credential did not arrive; zero model requests.')
    child=os.environ.copy();child['COMMAND_CODE_API_KEY']=key
    child.pop('GITHUB_TOKEN',None);child.pop('GH_TOKEN',None)
    result=subprocess.run(['dbus-run-session','--','xvfb-run','-a','-s','-screen 0 1280x960x24','bash','-c','openbox > /tmp/operator-opt-openbox.log 2>&1 & node scripts/operator-optimization-ab.mjs'],env=child)
    raise SystemExit(result.returncode)
finally:
    private.unlink(missing_ok=True)
    key=None
