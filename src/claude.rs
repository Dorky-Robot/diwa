//! Shared utilities for interacting with the Claude CLI.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Directory `claude` runs in. The CLI treats its cwd as a project root and
/// walks it gathering context — run it from wherever the caller happens to
/// be and that walk leaks outside diwa's world. The daemon made this
/// concrete: launchd starts agents with cwd `/`, so the walk descended into
/// ~/Desktop and macOS raised a "diwa would like to access files in your
/// Desktop folder" TCC prompt (the LaunchAgent is the responsible process,
/// so the child's file access is billed to diwa). Our prompts embed all the
/// commit data they need, so claude gets a dedicated empty dir instead.
fn claude_cwd() -> Option<PathBuf> {
    let base = if let Some(dir) = std::env::var_os("DIWA_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".diwa")
    };
    let cwd = base.join("agent-cwd");
    std::fs::create_dir_all(&cwd).ok()?;
    Some(cwd)
}

/// Model alias handed to `claude --model`. `sonnet` tracks the current Sonnet;
/// override with `DIWA_MODEL` (an alias or a full id such as
/// `claude-haiku-4-5-20251001`). Sonnet at low effort won the extraction
/// comparison in docs/qwen-eval: best "why" on the hard commits, ~9s each.
const DEFAULT_MODEL: &str = "sonnet";

/// Effort passed to `claude --effort`. Override with `DIWA_EFFORT`
/// (low|medium|high|xhigh|max). `off` omits the flag and sets
/// MAX_THINKING_TOKENS=0, which is what makes Haiku 4.5 fast and cheap: its
/// default is to think for ~2k tokens per commit and `--effort low` does not stop that.
const DEFAULT_EFFORT: &str = "low";

/// A call that has not finished by now is hung (the known failure is user
/// hooks loading in headless mode). Override with `DIWA_CLAUDE_TIMEOUT_SECS`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

const SYSTEM_PROMPT: &str = "You are one step in a developer tool. Follow the instructions in the user message exactly and output only what they ask for.";

/// Arguments for one headless call. Each flag removes a measured cost:
/// - `--setting-sources project`: user settings (hooks) make headless `-p` hang.
/// - `--tools ""` + custom system prompt + no MCP/slash commands: the default
///   Claude Code prompt and tool/connector list is ~45k input tokens per call;
///   this trims it to ~2k, which matters when the quota is a Max subscription.
/// - `--no-session-persistence`: no transcript litter per call.
fn build_args(model: &str, effort: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--setting-sources",
        "project",
        "--no-session-persistence",
        "--tools",
        "",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--system-prompt",
        SYSTEM_PROMPT,
        "--model",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(model.to_string());
    if effort != "off" {
        args.push("--effort".into());
        args.push(effort.to_string());
    }
    args
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}

/// Send a prompt to `claude -p` and return the response text.
pub fn prompt(text: &str) -> Result<String> {
    let model = env_or("DIWA_MODEL", DEFAULT_MODEL);
    let effort = env_or("DIWA_EFFORT", DEFAULT_EFFORT);
    let timeout = Duration::from_secs(
        env_or("DIWA_CLAUDE_TIMEOUT_SECS", "")
            .parse()
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
    );

    let mut cmd = Command::new("claude");
    if let Some(cwd) = claude_cwd() {
        cmd.current_dir(cwd);
    }
    if effort == "off" {
        cmd.env("MAX_THINKING_TOKENS", "0");
    }
    let mut child = cmd
        .args(build_args(&model, &effort))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run claude CLI — is it installed and authenticated?")?;

    // Feed stdin and drain both pipes on their own threads so a large prompt
    // or large reply cannot deadlock against the timeout loop below.
    let mut stdin = child.stdin.take().context("failed to open claude stdin")?;
    let input = text.to_string();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().context("failed to open claude stdout")?;
    let mut stderr = child.stderr.take().context("failed to open claude stderr")?;
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("claude process failed")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "claude timed out after {}s (model {model}, effort {effort}) and was killed",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(100));
    };
    let _ = writer.join();
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();

    if !status.success() {
        // `claude -p` reports e.g. an expired login on stdout, not stderr.
        let out = String::from_utf8_lossy(&stdout);
        anyhow::bail!(
            "claude exited with error: {} {}",
            String::from_utf8_lossy(&stderr).trim(),
            out.trim().chars().take(300).collect::<String>()
        );
    }

    let raw = String::from_utf8_lossy(&stdout).trim().to_string();
    let env = parse_envelope(&raw);
    if env.is_error {
        anyhow::bail!("claude reported an error: {}", env.text.chars().take(300).collect::<String>());
    }
    record_cost(&model, &effort, &env);
    Ok(env.text)
}

/// (calls, input tokens, output tokens) summed over a usage log. Lines that
/// do not parse are skipped; the `cost_usd` field older lines carry is ignored.
pub fn usage_totals(log: &str) -> (u64, u64, u64) {
    log.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .fold((0, 0, 0), |(c, i, o), v| {
            (c + 1, i + v["in"].as_u64().unwrap_or(0), o + v["out"].as_u64().unwrap_or(0))
        })
}

/// What `claude -p --output-format json` returns, reduced to what we use.
#[derive(Debug, PartialEq)]
pub(crate) struct Envelope {
    pub text: String,
    pub is_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Parse the JSON envelope; anything that is not one is treated as plain text.
pub(crate) fn parse_envelope(raw: &str) -> Envelope {
    let plain = || Envelope {
        text: raw.to_string(),
        is_error: false,
        input_tokens: 0,
        output_tokens: 0,
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return plain();
    };
    let Some(text) = v.get("result").and_then(|r| r.as_str()) else {
        return plain();
    };
    let u = |k: &str| v["usage"][k].as_u64().unwrap_or(0);
    Envelope {
        text: text.to_string(),
        is_error: v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false),
        input_tokens: u("input_tokens") + u("cache_creation_input_tokens") + u("cache_read_input_tokens"),
        output_tokens: u("output_tokens"),
    }
}

/// One line per call in `<diwa dir>/cost.jsonl` (name kept so older lines
/// stay in the same file): ts, model, effort, in, out tokens. Dollars are
/// not recorded — usage is on a flat subscription. Sum with `usage_totals`.
/// Best effort: never fails a call.
fn record_cost(model: &str, effort: &str, env: &Envelope) {
    let base = match std::env::var_os("DIWA_DIR") {
        Some(d) => PathBuf::from(d),
        None => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(".diwa"),
            None => return,
        },
    };
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "model": model, "effort": effort,
        "in": env.input_tokens, "out": env.output_tokens,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(base.join("cost.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Deserialize `tags` from either a string or an array of strings.
///
/// The prompt asks for "space-separated tags" but Sonnet returns an array in
/// roughly one reply in five. With a strict `Option<String>` that discards the
/// whole batch (and was a hard parse failure in 2 of 25 commits), so accept
/// both and join arrays with spaces.
pub fn lenient_tags<'de, D>(d: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Tags {
        One(String),
        Many(Vec<String>),
    }
    Ok(Option::<Tags>::deserialize(d)?.map(|t| match t {
        Tags::One(s) => s,
        Tags::Many(v) => v.join(" "),
    }))
}

/// Try to parse a JSON array from Claude's response.
///
/// Handles: raw JSON, JSON in markdown fences, JSON embedded in prose.
pub fn parse_json_array<T: serde::de::DeserializeOwned>(response: &str) -> Result<Vec<T>> {
    let trimmed = response.trim();

    // Try direct parse.
    if let Ok(parsed) = serde_json::from_str::<Vec<T>>(trimmed) {
        return Ok(parsed);
    }

    // Try extracting from markdown fences.
    if let Some(json_str) = extract_json_from_fences(trimmed) {
        if let Ok(parsed) = serde_json::from_str::<Vec<T>>(&json_str) {
            return Ok(parsed);
        }
    }

    // Try finding a JSON array anywhere in the response.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            let slice = &trimmed[start..=end];
            if let Ok(parsed) = serde_json::from_str::<Vec<T>>(slice) {
                return Ok(parsed);
            }
        }
    }

    anyhow::bail!("could not parse JSON array from claude response")
}

fn extract_json_from_fences(text: &str) -> Option<String> {
    let start_markers = ["```json\n", "```json\r\n", "```\n", "```\r\n"];
    for marker in &start_markers {
        if let Some(start) = text.find(marker) {
            let json_start = start + marker.len();
            if let Some(end) = text[json_start..].find("```") {
                return Some(text[json_start..json_start + end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_model_effort_and_the_hang_guard() {
        let a = build_args("sonnet", "low");
        let pos = |f: &str| a.iter().position(|x| x == f).unwrap();
        assert_eq!(a[pos("--setting-sources") + 1], "project");
        assert_eq!(a[pos("--model") + 1], "sonnet");
        assert_eq!(a[pos("--effort") + 1], "low");
        assert_eq!(a[pos("--tools") + 1], "");
    }

    #[test]
    fn effort_off_omits_the_flag() {
        assert!(!build_args("claude-haiku-4-5-20251001", "off").contains(&"--effort".to_string()));
    }

    #[derive(serde::Deserialize)]
    struct T {
        #[serde(default, deserialize_with = "lenient_tags")]
        tags: Option<String>,
    }

    #[test]
    fn tags_accept_string_array_or_absent() {
        let v: Vec<T> = parse_json_array(r#"[{"tags":"a b"},{"tags":["c","d"]},{}]"#).unwrap();
        assert_eq!(v[0].tags.as_deref(), Some("a b"));
        assert_eq!(v[1].tags.as_deref(), Some("c d"));
        assert_eq!(v[2].tags, None);
    }

    #[test]
    fn test_parse_clean_json() {
        let response = r#"[{"name":"a"},{"name":"b"}]"#;
        let parsed: Vec<serde_json::Value> = parse_json_array(response).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_parse_fenced_json() {
        let response = "Here:\n```json\n[{\"name\":\"a\"}]\n```\n";
        let parsed: Vec<serde_json::Value> = parse_json_array(response).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_parse_json_with_preamble() {
        let response = "I found:\n\n[{\"name\":\"a\"}]";
        let parsed: Vec<serde_json::Value> = parse_json_array(response).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_parse_garbage_fails() {
        let result: Result<Vec<serde_json::Value>> = parse_json_array("not json at all");
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn totals_tolerate_old_lines_with_cost() {
        let log = "{\"cost_usd\":0.04,\"in\":100,\"out\":10,\"model\":\"sonnet\"}\nnot json\n{\"in\":5,\"out\":1}\n";
        assert_eq!(usage_totals(log), (2, 105, 11));
    }

    #[test]
    fn parses_result_and_tokens() {
        let e = parse_envelope(r#"{"result":"[]","is_error":false,"total_cost_usd":0.0125,"usage":{"input_tokens":2,"cache_creation_input_tokens":1983,"cache_read_input_tokens":5,"output_tokens":14}}"#);
        assert_eq!(e.text, "[]");
        assert!(!e.is_error);
        assert_eq!((e.input_tokens, e.output_tokens), (1990, 14));
    }

    #[test]
    fn error_flag_and_plain_text_fallback() {
        assert!(parse_envelope(r#"{"result":"nope","is_error":true}"#).is_error);
        let p = parse_envelope("just text");
        assert_eq!(p.text, "just text");
        assert!(!p.is_error);
    }
}
