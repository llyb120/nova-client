use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const DEFAULT_LINES: usize = 2000;
const MAX_LINES: usize = 2000;
const CONTENT_POOL_BYTES: usize = 48 * 1024;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PathRequest {
    Path(String),
    Range {
        path: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
    },
}

#[derive(Debug, Deserialize)]
struct ReadParams {
    paths: Vec<PathRequest>,
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8(usize),
    Utf16Le(usize),
    Utf16Be(usize),
}

fn resolve_target(root: &Path, input: &str) -> Result<PathBuf, String> {
    if input.trim().is_empty() {
        return Err("file path is empty".into());
    }
    Ok(if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    })
}

fn detect_encoding(sample: &[u8]) -> Encoding {
    if sample.starts_with(&[0xff, 0xfe]) {
        return Encoding::Utf16Le(2);
    }
    if sample.starts_with(&[0xfe, 0xff]) {
        return Encoding::Utf16Be(2);
    }
    if sample.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Encoding::Utf8(3);
    }
    let pairs = sample.len().min(512) / 2;
    if pairs >= 4 {
        let mut even = 0usize;
        let mut odd = 0usize;
        for (index, byte) in sample.iter().take(pairs * 2).enumerate() {
            if *byte == 0 {
                if index % 2 == 0 {
                    even += 1;
                } else {
                    odd += 1;
                }
            }
        }
        if odd as f64 / pairs as f64 > 0.6 && even as f64 / (pairs as f64) < 0.1 {
            return Encoding::Utf16Le(0);
        }
        if even as f64 / pairs as f64 > 0.6 && odd as f64 / (pairs as f64) < 0.1 {
            return Encoding::Utf16Be(0);
        }
    }
    Encoding::Utf8(0)
}

fn paginate<I>(lines: I, offset: usize, limit: usize, max_bytes: usize) -> Result<Value, String>
where
    I: IntoIterator<Item = Result<String, String>>,
{
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut has_more = false;
    let mut stop_reason = "eof";
    let mut long_line_bytes = None;
    for (index, line) in lines.into_iter().enumerate() {
        let line_number = index + 1;
        let line = line?;
        if line_number < offset {
            continue;
        }
        if rows.len() == limit {
            has_more = true;
            stop_reason = "lineLimit";
            break;
        }
        let separator = usize::from(!rows.is_empty());
        if bytes + separator + line.len() > max_bytes {
            has_more = true;
            stop_reason = if rows.is_empty() { "longLine" } else { "byteBudget" };
            if rows.is_empty() {
                long_line_bytes = Some(line.len());
            }
            break;
        }
        bytes += separator + line.len();
        rows.push(line);
    }
    let lines_read = rows.len();
    let mut value = json!({
        "content": rows.join("\n"),
        "startLine": offset,
        "linesRead": lines_read,
        "bytesRead": bytes,
        "maxContentBytes": max_bytes,
        "hasMore": has_more,
        "rangeComplete": !matches!(stop_reason, "byteBudget" | "longLine"),
        "stopReason": stop_reason,
    });
    if lines_read > 0 {
        value["endLine"] = json!(offset + lines_read - 1);
    }
    if has_more {
        value["nextOffset"] = json!(offset + lines_read);
    }
    if let Some(line_bytes) = long_line_bytes {
        value["lineBytes"] = json!(line_bytes);
    }
    Ok(value)
}

fn utf8_lines(mut file: File, bom: usize, offset: usize, limit: usize, max_bytes: usize) -> Result<Value, String> {
    if bom > 0 {
        file.seek(SeekFrom::Start(bom as u64))
            .map_err(|e| e.to_string())?;
    }
    let reader = BufReader::new(file);
    paginate(
        reader.split(b'\n').enumerate().map(|(index, row)| {
            let mut row = row.map_err(|e| e.to_string())?;
            if row.last() == Some(&b'\r') {
                row.pop();
            }
            String::from_utf8(row).map_err(|e| format!("not UTF-8 at line {}: {e}", index + 1))
        }),
        offset,
        limit,
        max_bytes,
    )
}

fn utf16_lines(
    path: &Path,
    little_endian: bool,
    bom: usize,
    offset: usize,
    limit: usize,
    max_bytes: usize,
) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let body = bytes.get(bom..).ok_or("invalid UTF-16 BOM")?;
    if body.len() % 2 != 0 {
        return Err("invalid UTF-16 byte length".into());
    }
    let units = body
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    let text = String::from_utf16(&units).map_err(|e| format!("invalid UTF-16: {e}"))?;
    paginate(
        text.split_terminator('\n')
            .map(|line| Ok(line.trim_end_matches('\r').to_string())),
        offset,
        limit,
        max_bytes,
    )
}

fn read_one(root: &Path, request: PathRequest, max_bytes: usize) -> Value {
    let (display, offset, limit) = match request {
        PathRequest::Path(path) => (path, 1, DEFAULT_LINES),
        PathRequest::Range {
            path,
            offset,
            limit,
        } => (
            path,
            offset.unwrap_or(1).max(1),
            limit.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES),
        ),
    };
    let result = (|| -> Result<Value, String> {
        let target = resolve_target(root, &display)?;
        let mut file = File::open(&target).map_err(|e| e.to_string())?;
        let mut sample = [0u8; 512];
        let count = file.read(&mut sample).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut value = match detect_encoding(&sample[..count]) {
            Encoding::Utf8(bom) => utf8_lines(file, bom, offset, limit, max_bytes)?,
            Encoding::Utf16Le(bom) => utf16_lines(&target, true, bom, offset, limit, max_bytes)?,
            Encoding::Utf16Be(bom) => utf16_lines(&target, false, bom, offset, limit, max_bytes)?,
        };
        value["path"] = Value::String(display.clone());
        Ok(value)
    })();
    result.unwrap_or_else(|error| json!({ "path": display, "error": error }))
}

pub fn read_files(root: &Path, params: Value) -> Result<Value, String> {
    let params: ReadParams =
        serde_json::from_value(params).map_err(|e| format!("invalid read_files arguments: {e}"))?;
    if params.paths.is_empty() {
        return Err("paths must not be empty".into());
    }
    let count = params.paths.len();
    let per_file_bytes = CONTENT_POOL_BYTES / count.max(1);
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 8)
        .min(count);
    let requests = Mutex::new(params.paths.into_iter().map(Some).collect::<Vec<_>>());
    let results = Mutex::new((0..count).map(|_| None).collect::<Vec<Option<Value>>>());
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let request = requests.lock().unwrap()[index].take().unwrap();
                let value = read_one(root, request, per_file_bytes);
                results.lock().unwrap()[index] = Some(value);
            });
        }
    });
    Ok(Value::Array(
        results
            .into_inner()
            .unwrap()
            .into_iter()
            .map(Option::unwrap)
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_ranges_and_reports_next_offset() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "一\r\n二\r\n三\r\n").unwrap();
        let out = read_files(
            dir.path(),
            json!({"paths":[{"path":"a.txt","offset":2,"limit":1}]}),
        )
        .unwrap();
        assert_eq!(out[0]["content"], "二");
        assert_eq!(out[0]["nextOffset"], 3);
    }

    #[test]
    fn reads_utf16_little_endian() {
        let dir = tempdir().unwrap();
        let mut bytes = vec![0xff, 0xfe];
        for unit in "hello\r\n你好".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        fs::write(dir.path().join("a.txt"), bytes).unwrap();
        let out = read_files(dir.path(), json!({"paths":["a.txt"]})).unwrap();
        assert_eq!(out[0]["content"], "hello\n你好");
    }

    #[test]
    fn returns_per_file_utf8_error() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("bad.bin"), [0xff, 0x00, 0xff]).unwrap();
        let out = read_files(dir.path(), json!({"paths":["bad.bin"]})).unwrap();
        assert!(out[0]["error"].as_str().unwrap().contains("UTF-8"));
    }
}
