// Validates packaging and local receiver boundaries, never claims to test Chrome APIs.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const read = async path => JSON.parse(await readFile(new URL(path, import.meta.url), 'utf8'));
const launch = await read('./out/launch.json');
const manifest = await read('./out/extension/manifest.json');
const config = await read('./out/extension/config.json');
assert.equal(manifest.manifest_version, 3);
assert.ok(manifest.permissions.includes('debugger'));
for (const file of [manifest.background.service_worker,manifest.action.default_popup,'popup.js','native_browser_page.js']) {
  assert.ok((await readFile(new URL(`./out/extension/${file}`, import.meta.url))).length);
}
assert.ok((await (await fetch(launch.fixtureUrl)).text()).includes(`整页底部标记 ${config.nonce}`));
assert.equal((await fetch(`${launch.origin}/result`,{method:'POST',body:'{}'})).status,403);
assert.equal((await fetch(`${launch.origin}/result`,{method:'POST',headers:{Origin:'https://example.com',Authorization:`Bearer ${config.token}`},body:'{}'})).status,403);
assert.equal((await fetch(`${launch.origin}/result`,{method:'POST',headers:{Origin:`chrome-extension://${launch.extensionId}`,Authorization:`Bearer ${config.token}`},body:JSON.stringify({passed:false,screenshot:'invalid'})})).status,400);
console.log('PASS extension packaging, fixture and receiver authorization; Chrome runtime still needs manual extension loading.');
