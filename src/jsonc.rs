//! JSONC -> JSON, so a hand-edited config file can carry comments.
//!
//! `config.json` is written with inline comments explaining every key (a setting nobody
//! can find is a setting nobody uses) and is then edited by a human in Notepad. That
//! makes it JSONC: `//` and `/* */` comments, and a trailing comma left behind after
//! deleting a line. `serde_json` accepts neither, so the text is normalised first.
//!
//! Deliberately a preprocessor and not a dependency: the whole grammar addition is
//! "comments are whitespace, one dangling comma is forgivable", which is thirty lines
//! and testable, against a crate that would also pull in a second JSON model.
//!
//! Comments become spaces rather than nothing, and newlines survive, so a parse error
//! still points at the line and column the user is looking at in their editor.

/// Strip a BOM, comments and trailing commas. Anything inside a string literal is
/// untouched.
pub fn to_json(raw: &str) -> String {
    // Notepad's "UTF-8 with BOM" and PowerShell 5.1's `Set-Content -Encoding UTF8` both
    // put U+FEFF first, and `serde_json` rejects it as an unexpected value. Since this
    // file exists to be edited by hand on Windows, a BOM is an expected input, not a
    // malformed one.
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let stripped = strip_comments(raw);
    strip_trailing_commas(&stripped)
}

fn strip_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            // An escape consumes exactly the next character, so `"\\"` ends the string
            // and `"\""` does not.
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut star = false;
                for skipped in chars.by_ref() {
                    if star && skipped == '/' {
                        break;
                    }
                    star = skipped == '*';
                    // Keep the line count: a block comment may span lines.
                    if skipped == '\n' {
                        out.push('\n');
                    }
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }

    out
}

fn strip_trailing_commas(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;

    for (i, c) in raw.char_indices() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == ',' {
            // A comma whose next non-space character closes the container is the one
            // JSON forbids and every editor leaves behind.
            let next = bytes[i + 1..]
                .iter()
                .find(|b| !b.is_ascii_whitespace())
                .copied();
            if matches!(next, Some(b'}') | Some(b']')) {
                continue;
            }
        }

        out.push(c);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::to_json;

    #[test]
    fn line_and_block_comments_become_whitespace() {
        let raw = "{\n // a\n \"a\": 1, /* b */ \"b\": 2\n}";
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert_eq!(value["a"], 1);
        assert_eq!(value["b"], 2);
    }

    #[test]
    fn comment_markers_inside_strings_survive() {
        let raw = r#"{"path": "C:\\a//b", "note": "/* not a comment */"}"#;
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert_eq!(value["path"], "C:\\a//b");
        assert_eq!(value["note"], "/* not a comment */");
    }

    #[test]
    fn trailing_commas_are_forgiven_in_both_containers() {
        let raw = "{\"list\": [1, 2, ],\n}";
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert_eq!(value["list"], serde_json::json!([1, 2]));
    }

    #[test]
    fn commas_inside_strings_are_not_trailing() {
        let raw = r#"{"a": "x,", "b": ["y,"]}"#;
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert_eq!(value["a"], "x,");
        assert_eq!(value["b"][0], "y,");
    }

    #[test]
    fn escaped_quote_does_not_end_the_string() {
        let raw = r#"{"a": "he said \"hi\" // not a comment"}"#;
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert_eq!(value["a"], "he said \"hi\" // not a comment");
    }

    #[test]
    fn plain_json_is_unchanged() {
        let raw = r#"{"a":[1,2],"b":{"c":"d"}}"#;
        assert_eq!(to_json(raw), raw);
    }

    /// What `Set-Content -Encoding UTF8` (Windows PowerShell 5.1) and Notepad's "UTF-8
    /// with BOM" produce. Without the strip this parse fails and the registry reads empty.
    #[test]
    fn a_leading_bom_is_not_a_parse_error() {
        let raw = "\u{feff}{\"voicePacks\": []}";
        assert!(serde_json::from_str::<serde_json::Value>(raw).is_err());
        let value: serde_json::Value = serde_json::from_str(&to_json(raw)).unwrap();
        assert!(value["voicePacks"].is_array());
    }
}
