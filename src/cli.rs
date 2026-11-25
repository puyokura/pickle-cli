//! Argument parsing for the `pickle` CLI.
//!
//! Hand-written, same flat shape as the Muse CLI: `command [subcommand]
//! [positionals...] [--flag value] [--switch]`. Repeated flags accumulate, so
//! `--set k=v --set k2=v2` works without a JSON blob.
//!
//! Windows note: PowerShell 5.1 mangles `--json "{...}"` before the native
//! process sees it (double quotes are dropped). `--set key=value` contains no
//! quotes and therefore survives every shell; prefer it.

use crate::error::{PickleError, Result};
use std::collections::BTreeMap;

/// Switches that never take a value. Without this list `--raw` would swallow
/// the following positional.
const BOOLEAN_SWITCHES: [&str; 9] = [
    "help",
    "json",
    "raw",
    "follow",
    "confirm",
    "verbose",
    "insecure-state-warning",
    "all",
    "stdin",
];

pub fn is_boolean_switch(name: &str) -> bool {
    BOOLEAN_SWITCHES.contains(&name)
}

pub struct Args {
    pub positionals: Vec<String>,
    flags: BTreeMap<String, Vec<String>>,
    switches: Vec<String>,
}

impl Args {
    pub fn parse(raw: &[String]) -> Result<Self> {
        let mut positionals = Vec::new();
        let mut flags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut switches = Vec::new();
        let mut index = 0usize;

        while index < raw.len() {
            let token = &raw[index];
            if let Some(rest) = token.strip_prefix("--") {
                if rest.is_empty() {
                    return Err(PickleError::Unsupported("empty flag `--`".into()));
                }
                if let Some((name, value)) = rest.split_once('=') {
                    flags
                        .entry(name.to_string())
                        .or_default()
                        .push(value.to_string());
                    index += 1;
                    continue;
                }
                let takes_value = if is_boolean_switch(rest) {
                    false
                } else {
                    match raw.get(index + 1) {
                        Some(next) => !next.starts_with("--"),
                        None => false,
                    }
                };
                if takes_value {
                    flags
                        .entry(rest.to_string())
                        .or_default()
                        .push(raw[index + 1].clone());
                    index += 2;
                } else {
                    switches.push(rest.to_string());
                    index += 1;
                }
                continue;
            }
            if let Some(rest) = token.strip_prefix('-') {
                if rest.len() == 1 {
                    switches.push(rest.to_string());
                    index += 1;
                    continue;
                }
            }
            positionals.push(token.clone());
            index += 1;
        }

        Ok(Self {
            positionals,
            flags,
            switches,
        })
    }

    pub fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|flag| flag == name) || self.flags.contains_key(name)
    }

    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    pub fn flag_all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }

    /// Every flag and switch supplied, for "unknown flag" validation.
    pub fn supplied(&self) -> Vec<String> {
        let mut names: Vec<String> = self.flags.keys().cloned().collect();
        names.extend(self.switches.iter().cloned());
        names.sort();
        names.dedup();
        names
    }

    /// Reject any flag the command does not declare. Catches the common
    /// mistake of putting `--json` on a command that only reads `--set`.
    pub fn reject_unknown(&self, command: &str, allowed: &[&str]) -> Result<()> {
        for name in self.supplied() {
            if !allowed.contains(&name.as_str()) {
                return Err(PickleError::Unsupported(format!(
                    "{command}: unknown flag --{name}"
                )));
            }
        }
        Ok(())
    }

    /// Build a JSON object from repeated `--set key=value` pairs.
    ///
    /// Values are typed the way a caller expects: `true`/`false` become booleans,
    /// integers and floats become numbers, comma-separated lists become arrays,
    /// everything else stays a string. `key.nested` sets a nested object key.
    /// Merge the JSON routes into one object.
    ///
    /// `--json-file PATH` then `--json-body JSON`, then `--json-body -` (stdin),
    /// each overriding the previous; `--set` is applied on top by `set_object`.
    ///
    /// The names avoid `--json`, which already means "print one line" here.
    /// `--set` alone cannot express an array of objects, which some bodies need.
    pub fn json_object(&self) -> Result<serde_json::Value> {
        let mut merged = serde_json::Map::new();
        let mut sources: Vec<(&'static str, String)> = Vec::new();
        if let Some(path) = self.flag("json-file") {
            let text = std::fs::read_to_string(path)
                .map_err(|err| PickleError::State(format!("{path}: {err}")))?;
            sources.push(("--json-file", text));
        }
        if let Some(raw) = self.flag("json-body") {
            if raw == "-" {
                let mut line = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
                    .map_err(|err| PickleError::State(format!("stdin: {err}")))?;
                sources.push(("--json-body -", line));
            } else {
                sources.push(("--json-body", raw.to_string()));
            }
        }
        for (label, text) in sources {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|err| PickleError::Unsupported(format!("{label}: not JSON ({err})")))?;
            let kind = match &parsed {
                serde_json::Value::Array(_) => "an array",
                serde_json::Value::String(_) => "a string",
                serde_json::Value::Number(_) => "a number",
                serde_json::Value::Bool(_) => "a boolean",
                serde_json::Value::Null => "null",
                serde_json::Value::Object(_) => "an object",
            };
            let Some(object) = parsed.as_object() else {
                return Err(PickleError::Unsupported(format!(
                    "{label}: expected a JSON object, got {kind}"
                )));
            };
            for (key, value) in object {
                merged.insert(key.clone(), value.clone());
            }
        }
        Ok(serde_json::Value::Object(merged))
    }

    pub fn set_object(&self) -> Result<serde_json::Value> {
        // Start from the JSON routes, then let `--set` win, so a caller can
        // provide a large object from a file and tweak one field inline.
        let mut root = match self.json_object()? {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        for pair in self.flag_all("set") {
            let Some((key, raw)) = pair.split_once('=') else {
                return Err(PickleError::Unsupported(format!(
                    "--set needs key=value, got {pair}"
                )));
            };
            insert_path(&mut root, key, parse_scalar(raw));
        }
        Ok(serde_json::Value::Object(root))
    }
}

/// Insert `a.b.c` into nested objects, creating missing levels.
fn insert_path(
    root: &mut serde_json::Map<String, serde_json::Value>,
    path: &str,
    value: serde_json::Value,
) {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_string(), value.clone());
            return;
        }
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        match entry {
            serde_json::Value::Object(map) => current = map,
            _ => return,
        }
    }
}

/// Type a raw `--set` value.
pub fn parse_scalar(raw: &str) -> serde_json::Value {
    match raw {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        "null" => return serde_json::Value::Null,
        _ => {}
    }
    if let Ok(value) = raw.parse::<i64>() {
        return serde_json::Value::Number(value.into());
    }
    if let Ok(value) = raw.parse::<f64>() {
        if value.is_finite() {
            if let Some(number) = serde_json::Number::from_f64(value) {
                return serde_json::Value::Number(number);
            }
        }
    }
    if raw.contains(',') {
        let items: Vec<serde_json::Value> = raw
            .split(',')
            .filter(|item| !item.is_empty())
            .map(parse_scalar)
            .collect();
        if !items.is_empty() {
            return serde_json::Value::Array(items);
        }
    }
    serde_json::Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(tokens: &[&str]) -> Args {
        Args::parse(&tokens.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn positional_then_flag() {
        let args = parse(&["sync", "--env", "dev"]);
        assert_eq!(args.positionals, vec!["sync"]);
        assert_eq!(args.flag("env"), Some("dev"));
    }

    #[test]
    fn switch_does_not_swallow_next() {
        let args = parse(&["rooms", "--raw", "abc"]);
        assert_eq!(args.positionals, vec!["rooms", "abc"]);
        assert!(args.has("raw"));
    }

    #[test]
    fn repeated_set_accumulates() {
        let args = parse(&[
            "send",
            "r1",
            "--set",
            "text=hi",
            "--set",
            "context.kind=card",
        ]);
        assert_eq!(args.flag_all("set").len(), 2);
        let obj = args.set_object().unwrap();
        assert_eq!(obj["text"], json!("hi"));
        assert_eq!(obj["context"]["kind"], json!("card"));
    }

    #[test]
    fn scalar_typing() {
        assert_eq!(parse_scalar("true"), json!(true));
        assert_eq!(parse_scalar("42"), json!(42));
        assert_eq!(parse_scalar("4.5"), json!(4.5));
        assert_eq!(parse_scalar("a,b,c"), json!(["a", "b", "c"]));
        assert_eq!(parse_scalar("hello"), json!("hello"));
    }

    #[test]
    fn unknown_flag_is_rejected() {
        let args = parse(&["status", "--nope"]);
        assert!(args.reject_unknown("status", &["json"]).is_err());
        assert!(args.reject_unknown("status", &["json", "nope"]).is_ok());
    }

    #[test]
    fn equals_form_flag() {
        let args = parse(&["sync", "--env=dev"]);
        assert_eq!(args.flag("env"), Some("dev"));
    }
}
