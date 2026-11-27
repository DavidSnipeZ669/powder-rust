//! Converts a minified JavaScript object literal (as emitted by Powder's bundler) to JSON:
//! bare keys get quoted, !0/!1 become true/false, `void 0` becomes null, other identifiers become strings.
use anyhow::{bail, Result};
use serde_json::Value;

pub fn to_json(src: &str) -> Result<Value> {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + src.len() / 8);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' | b'\'' | b'`' => {
                // copy string, normalising to double quotes
                let q = c;
                i += 1;
                out.push('"');
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        if b[i + 1] == b'\'' {
                            out.push('\'');
                        } else {
                            out.push('\\');
                            out.push(b[i + 1] as char);
                        }
                        i += 2;
                        continue;
                    }
                    if q != b'"' && b[i] == b'"' {
                        out.push_str("\\\"");
                    } else {
                        // copy the full UTF-8 char
                        let ch = src[i..].chars().next().unwrap();
                        out.push(ch);
                        i += ch.len_utf8();
                        continue;
                    }
                    i += 1;
                }
                out.push('"');
                i += 1;
            }
            // comments (Rewind's engine marks the games it installs with /*powder-rs:ID*/)
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i = src[i + 2..].find("*/").map(|k| i + 2 + k + 2).unwrap_or(b.len());
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                i = src[i..].find('\n').map(|k| i + k + 1).unwrap_or(b.len());
            }
            b'!' if i + 1 < b.len() && (b[i + 1] == b'0' || b[i + 1] == b'1') => {
                out.push_str(if b[i + 1] == b'0' { "true" } else { "false" });
                i += 2;
            }
            b'.' if i + 1 < b.len() && b[i + 1].is_ascii_digit() => {
                out.push_str("0.");
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] == b'.') {
                    i += 1;
                }
                let word = &src[s..i];
                let mut j = i;
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                if j < b.len() && b[j] == b':' {
                    out.push('"');
                    out.push_str(word);
                    out.push('"');
                } else {
                    match word {
                        "true" | "false" | "null" => out.push_str(word),
                        "void" => {
                            out.push_str("null");
                            while i < b.len() && (b[i] == b' ' || b[i] == b'0') {
                                i += 1;
                            }
                        }
                        "undefined" => out.push_str("null"),
                        w => {
                            out.push('"');
                            out.push_str(w.rsplit('.').next().unwrap_or(w));
                            out.push('"');
                        }
                    }
                }
            }
            // tolerate the stray commas left where a commented-out entry was
            b',' if matches!(out.trim_end().chars().last(), Some(',') | Some('{') | Some('[')) => {
                i += 1;
            }
            b'}' | b']' if out.trim_end().ends_with(',') => {
                let t = out.trim_end().len() - 1;
                out.truncate(t);
                out.push(c as char);
                i += 1;
            }
            _ => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    match serde_json::from_str(&out) {
        Ok(v) => Ok(v),
        Err(e) => bail!("js literal: {e} near {}", &out[e.column().saturating_sub(60).min(out.len())..(e.column() + 20).min(out.len())]),
    }
}

/// Byte range of the balanced {...} starting at `start` (which must be '{').
pub fn balanced(src: &str, start: usize) -> Option<usize> {
    let b = src.as_bytes();
    let (mut depth, mut i, mut q) = (0i64, start, 0u8);
    while i < b.len() {
        let c = b[i];
        if q != 0 {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                q = 0;
            }
        } else {
            match c {
                b'"' | b'\'' | b'`' => q = c,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod comment_tests {
    #[test]
    fn skips_engine_markers() {
        let v = super::to_json(r#"{A:{x:!0},/*powder-rs:CDMW4*/,"CDMW4":{y:1}/*/powder-rs:CDMW4*/}"#).unwrap();
        assert_eq!(v["CDMW4"]["y"], 1);
        assert_eq!(v["A"]["x"], true);
    }
}
