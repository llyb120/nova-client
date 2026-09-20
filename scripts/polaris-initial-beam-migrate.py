"""Test whether the accepted 20/20 ranking can start from fewer syntax-parsed files.
Only INITIAL_FILES changes. No ranking, dependency closure, output budget, model,
Reasonix, or source-scope changes.
"""
from pathlib import Path
import hashlib
p=Path("src-tauri/src/nova_tools_native/polaris_demand_v2.rs")
b=p.read_bytes()
got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
assert got=="3c814ff3e30916e15b7e357bda2338a52c4aa341",got
s=b.decode("utf-8")
old="const INITIAL_FILES: usize = 16;"
assert s.count(old)==1
s=s.replace(old,"const INITIAL_FILES: usize = 12;")
p.write_text(s,encoding="utf-8")
print("INITIAL_FILES: 16 -> 12; all other production behavior unchanged.")
