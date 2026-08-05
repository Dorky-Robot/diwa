//! Shared utilities for interacting with the Claude CLI.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

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

/// Send a prompt to `claude -p` and return the response text.
pub fn prompt(text: &str) -> Result<String> {
    let mut cmd = Command::new("claude");
    if let Some(cwd) = claude_cwd() {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .args(["-p", "--output-format", "text"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run claude CLI — is it installed and authenticated?")?;

    child
        .stdin
        .as_mut()
        .context("failed to write to claude stdin")?
        .write_all(text.as_bytes())?;

    let output = child.wait_with_output().context("claude process failed")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude exited with error: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
