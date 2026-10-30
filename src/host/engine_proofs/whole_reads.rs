//! The wall around whole reads of a stream.
//!
//! `read_whole_raw(`, `read_whole(` and `decode_stream_data(` hold a stream's
//! data in one buffer. Every caller in the engine is named in `whole_reads.txt`, one
//! `module::function` a line; a call from anywhere else fails this proof,
//! and a listed caller that no longer calls fails it too, so the list only
//! shrinks as the callers move to readers.

use std::collections::BTreeSet;
use std::path::Path;

const NEEDLES: [&str; 3] = ["read_whole_raw(", "read_whole(", "decode_stream_data("];

/// The first line of a `#[cfg(test)]` module: everything after it is test code.
fn code_end(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        let next = lines[i + 1..].iter().map(|l| l.trim()).find(|l| !l.starts_with("#["));
        if next.is_some_and(|l| {
            l.trim_start_matches("pub ")
                .trim_start_matches("pub(crate) ")
                .starts_with("mod ")
        }) {
            return i;
        }
    }
    lines.len()
}

/// The name of the function whose body holds line `at`; `None` for a function
/// that exists only in a test build (`#[cfg(test)]`).
fn enclosing_fn(lines: &[&str], at: usize) -> Option<String> {
    for (index, line) in lines[..=at].iter().enumerate().rev() {
        let mut rest = line.trim();
        for prefix in ["pub(crate) ", "pub(super) ", "pub ", "const ", "async ", "unsafe ", "extern \"C\" "] {
            rest = rest.trim_start_matches(prefix);
        }
        if let Some(after) = rest.strip_prefix("fn ") {
            let end = after.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(after.len());
            let test_only = lines[..index]
                .iter()
                .rev()
                .map(|l| l.trim())
                .take_while(|l| l.starts_with("#[") || l.starts_with("///") || l.starts_with("//"))
                .any(|l| l == "#[cfg(test)]");
            return (!test_only).then(|| after[..end].to_string());
        }
    }
    panic!("no enclosing fn above line {at}");
}

fn callers_under(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.ends_with("host/engine_proofs") {
                continue;
            }
            callers_under(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.split('\n').collect();
            let relative = path.strip_prefix(root).unwrap().with_extension("");
            let mut module = relative.to_string_lossy().replace('/', "::");
            if let Some(stripped) = module.strip_suffix("::mod") {
                module = stripped.to_string();
            }
            for at in 0..code_end(&lines) {
                let line = lines[at].trim();
                let defines = NEEDLES.iter().any(|n| line.contains(&format!("fn {n}")));
                if line.starts_with("//") || defines || !NEEDLES.iter().any(|n| line.contains(n)) {
                    continue;
                }
                if let Some(name) = enclosing_fn(&lines, at) {
                    out.insert(format!("{module}::{name}"));
                }
            }
        }
    }
}

/// No Dart battery sees it: it reads the engine's own source text, and Dart cannot tell a stream read whole from one read in pieces.
#[test]
fn only_the_listed_functions_read_a_stream_whole() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeSet::new();
    callers_under(&src, &src, &mut found);
    let listed: BTreeSet<String> = include_str!("whole_reads.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let new: Vec<_> = found.difference(&listed).collect();
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(
        new.is_empty() && gone.is_empty(),
        "callers not on the list: {new:?}; listed callers that no longer read whole: {gone:?}"
    );
}
