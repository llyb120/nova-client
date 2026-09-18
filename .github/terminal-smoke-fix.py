from pathlib import Path
p=Path('scripts/windows-terminal-smoke.mjs')
s=p.read_text(encoding='utf-8')
old="    await page.getByRole('tab', { name: /终端 1/ }).click();"
new="    await page.getByRole('region', { name: '交互式终端' }).getByRole('tab').nth(0).click();"
assert s.count(old)==1
s=s.replace(old,new)
old="    await page.screenshot({ path: `windows-terminal-${shell.name}.png` });"
new="    assert.equal(await page.evaluate(() => window.nativeTerminalSmoke.active().error()), '', `${shell.name}: terminal displays an error`);\n"+old
assert s.count(old)==1
s=s.replace(old,new)
p.write_text(s,encoding='utf-8')
