from pathlib import Path
p=Path('scripts/chrome-precision-smoke.mjs');s=p.read_text(encoding='utf8')
old="await page.evaluate('const g=document.querySelector(\"#paint\").getContext(\"2d\");g.fillStyle=\"blue\";g.fillRect(400,160,480,320)')"
new="await page.evaluate('{const g=document.querySelector(\"#paint\").getContext(\"2d\");g.fillStyle=\"blue\";g.fillRect(400,160,480,320)}')"
assert s.count(old)==1
p.write_text(s.replace(old,new),encoding='utf8')
