from pathlib import Path
p=Path('scripts/windows-terminal-smoke.mjs')
s=p.read_text(encoding='utf-8')
def replace(old,new):
    global s
    assert s.count(old)==1, old
    s=s.replace(old,new)
replace("    await page.getByRole('tab', { name: /终端 1/ }).click();", "    await page.getByRole('region', { name: '交互式终端' }).getByRole('tab').nth(0).click();")
old="    await page.screenshot({ path: `windows-terminal-${shell.name}.png` });"
replace(old,"    assert.equal(await page.evaluate(() => window.nativeTerminalSmoke.active().error()), '', `${shell.name}: terminal displays an error`);\n"+old)
replace("    await page.evaluate(command => window.nativeTerminalSmoke.active().terminal.paste(command + '\\r'), shell.command);", "    await page.waitForFunction(() => window.nativeTerminalSmoke.buffer().trim().endsWith('>'));\n    await page.evaluate(command => window.nativeTerminalSmoke.active().terminal.paste(command), shell.command);\n    await page.locator('.xterm-helper-textarea').focus();\n    await page.keyboard.press('Enter');")
replace("    await page.evaluate(() => window.nativeTerminalSmoke.active().terminal.paste('exit\\r'));", "    await page.evaluate(() => window.nativeTerminalSmoke.active().terminal.paste('exit'));\n    await page.locator('.xterm-helper-textarea').focus();\n    await page.keyboard.press('Enter');")
replace("  report.push({ result: 'failed', error: String(error.stack || error) });", """  report.push({ result: 'failed', error: String(error.stack || error) });
  if (process.env.TEST_WINDOWS_DIAGNOSTICS && app?.pid) {
    const diagnostic = spawnSync('powershell.exe', ['-NoProfile', '-File', process.env.TEST_WINDOWS_DIAGNOSTICS, String(app.pid)], { encoding: 'utf8', timeout: 15000 });
    appLogs.push(diagnostic.stdout || '', diagnostic.stderr || '');
    console.error(diagnostic.stdout, diagnostic.stderr);
  }""")
p.write_text(s,encoding='utf-8')
