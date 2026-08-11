//! Agent model extraction from argv.
//!
//! Firstmate-launched agents carry their model explicitly on the command line
//! (e.g. `opencode --model zai-coding-plan/glm-5.2`, `claude --model opus`).
//! This module parses the `--model` flag in both its space-separated and
//! `=`-joined forms and renders the value compactly by stripping any provider
//! prefix (`zai-coding-plan/glm-5.2` -> `glm-5.2`).
//!
//! When no model flag is present, [`resolve_model`] returns `None`; the caller
//! renders that as `-` rather than guessing.

/// Parse the model value from argv, looking for `--model X` (separate token)
/// or `--model=X` (joined). Returns the raw value (with provider prefix) or
/// `None` if no model flag is present.
pub fn extract_model(cmdline: &[String]) -> Option<String> {
    for (i, tok) in cmdline.iter().enumerate().skip(1) {
        if tok == "--model" {
            return cmdline.get(i + 1).map(|s| s.to_string());
        }
        if let Some(rest) = tok.strip_prefix("--model=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Strip a provider prefix for compact display: `zai-coding-plan/glm-5.2` ->
/// `glm-5.2`. A value with no `/` is returned unchanged.
pub fn display_model(full: &str) -> &str {
    match full.rsplit_once('/') {
        Some((_, last)) => last,
        None => full,
    }
}

/// Resolve the model to show for an agent process: extract from argv, strip the
/// provider prefix, and return the compact form. Returns `None` when argv
/// carries no model flag (the caller shows `-`).
pub fn resolve_model(cmdline: &[String]) -> Option<String> {
    extract_model(cmdline).map(|m| display_model(&m).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn separate_flag_form() {
        let cmd = argv(&["opencode", "--model", "zai-coding-plan/glm-5.2"]);
        assert_eq!(
            extract_model(&cmd).as_deref(),
            Some("zai-coding-plan/glm-5.2")
        );
        assert_eq!(resolve_model(&cmd).as_deref(), Some("glm-5.2"));
    }

    #[test]
    fn joined_flag_form() {
        let cmd = argv(&["claude", "--model=opus"]);
        assert_eq!(extract_model(&cmd).as_deref(), Some("opus"));
        assert_eq!(resolve_model(&cmd).as_deref(), Some("opus"));
    }

    #[test]
    fn joined_flag_form_with_prefix() {
        let cmd = argv(&["opencode", "--model=zai-coding-plan/glm-5.2"]);
        assert_eq!(resolve_model(&cmd).as_deref(), Some("glm-5.2"));
    }

    #[test]
    fn model_with_no_prefix_stays_intact() {
        assert_eq!(display_model("opus"), "opus");
        assert_eq!(display_model("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn model_with_nested_prefix_strips_to_last_segment() {
        // Only the segment after the last `/` is kept.
        assert_eq!(display_model("a/b/c"), "c");
        assert_eq!(display_model("anthropic/claude-3-opus"), "claude-3-opus");
    }

    #[test]
    fn no_model_flag_returns_none() {
        let cmd = argv(&["claude", "--verbose", "-p"]);
        assert!(extract_model(&cmd).is_none());
        assert!(resolve_model(&cmd).is_none());
    }

    #[test]
    fn model_flag_at_end_with_no_value() {
        // `--model` as the final token with no following value -> None.
        let cmd = argv(&["opencode", "--model"]);
        assert!(extract_model(&cmd).is_none());
    }

    #[test]
    fn model_flag_among_other_flags() {
        let cmd = argv(&[
            "claude",
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--effort",
            "xhigh",
        ]);
        assert_eq!(resolve_model(&cmd).as_deref(), Some("opus"));
    }

    #[test]
    fn only_first_model_flag_wins() {
        let cmd = argv(&["opencode", "--model", "opus", "--model", "sonnet"]);
        assert_eq!(resolve_model(&cmd).as_deref(), Some("opus"));
    }

    #[test]
    fn empty_cmdline_returns_none() {
        assert!(extract_model(&[]).is_none());
        assert!(resolve_model(&[]).is_none());
    }

    #[test]
    fn model_flag_in_argv0_position_ignored() {
        // argv[0] is the program; a `--model` there is not a flag.
        let cmd = argv(&["--model"]);
        assert!(extract_model(&cmd).is_none());
    }
}
