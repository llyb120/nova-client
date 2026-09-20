"""Test whether the accepted 20/20 ranking can start from fewer syntax-parsed files.
Only INITIAL_FILES changes. No ranking, dependency closure, output budget, model,
Reasonix, or source-scope changes.
"""
from pathlib import Path
import hashlib
p=Path("src-tauri/src/nova_tools_native/polaris_demand_v2.rs")
b=p.read_bytes()
got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
assert got=="0f5fe46653dd5248151468b0e32e536ce280681f",got
s=b.decode("utf-8")
old="const INITIAL_FILES: usize = 16;"
assert s.count(old)==1
s=s.replace(old,"const INITIAL_FILES: usize = 14;")
p.write_text(s,encoding="utf-8")
print("INITIAL_FILES: 16 -> 14; all other production behavior unchanged.")
