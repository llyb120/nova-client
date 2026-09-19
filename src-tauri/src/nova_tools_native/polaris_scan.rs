/// The legacy scanner deliberately omits one-line methods/properties. They are
/// valid retrieval units (notably JS IPC adapters), so supplement them without
/// modifying the baseline engine or accepting incidental function call sites.
fn scan_source(text: &str, file: &str) -> FileEntry {
    let mut entry = super::scan_source(text, file);
    if file.ends_with(".py") || file.ends_with(".pyi") { return entry; }
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let (code, starts, after) = stripped_depth(&lines, file.ends_with(".rs"));
    let existing = entry.syms.iter().map(|s| s.ln).collect::<HashSet<_>>();
    for (i, line) in code.iter().enumerate() {
        if existing.contains(&(i + 1)) || starts[i] == 0 || starts[i] > 2 || after[i] > starts[i] { continue; }
        let Some((name, kind)) = declaration(line.trim_start(), starts[i]) else { continue; };
        if !matches!(kind.as_str(), "method" | "prop") { continue; }
        // The unit filter independently checks the actual method-body / arrow
        // declaration. A plain `sendRequest(...)` remains excluded.
        entry.syms.push(Symbol { ln: i + 1, end: i + 1, depth: starts[i], kind, name,
            sig: signature(&lines[i]), exp: false });
    }
    entry.syms.sort_by_key(|s| s.ln);
    entry
}
