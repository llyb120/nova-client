"""Test-fixture-only pixel calibration correction, no native action/source changes.
XCap PNG has resampled boundary pixels; exact RGB found only 32/64 marker pixels.
Use a distinctive magenta band. Keep bounding-box, scale and all action assertions.
"""
from pathlib import Path
p=Path('scripts/operator-native-regression.mjs');s=p.read_text()
a='png.data[i]===251&&png.data[i+1]===17&&png.data[i+2]===241'
b='png.data[i]>200&&png.data[i+1]<80&&png.data[i+2]>200'
assert s.count(a)==1
s=s.replace(a,b)
p.write_text(s)
Path('validation/fixture-pixel-correction.txt').write_text('Native screenshot color resampling: replace exact RGB matcher with red>200, green<80, blue>200; no target, input action or success assertion changed.\n')
