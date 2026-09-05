//! Turning session context into vocabulary hints for the recognizer.
//!
//! This is the reason server-side STT is worth having at all. A generic
//! recognizer hears "cargo clippy on the multi provider identity branch" and
//! writes *"cargo clip he on the multi provider identity branch"*; told in
//! advance that `clippy` and `meawoppl/multi-provider-identity-model` are live
//! terms in this session, it gets them right. The browser API has no such hook.
//!
//! Everything here comes from columns we already have on `sessions`, so biasing
//! costs one row read and no extra state.

/// Upper bound on hints handed to a provider. Deepgram bills and rate-limits on
/// keyterm count, and a long tail of low-value tokens measurably *hurts*
/// accuracy by biasing toward rare words — so the list stays short and specific.
const MAX_KEYTERMS: usize = 40;

/// Terms too common to be worth a slot: biasing toward them can only pull the
/// recognizer away from a correct ordinary word.
const STOP_TERMS: &[&str] = &[
    "the", "and", "for", "src", "lib", "app", "home", "users", "repos", "tmp", "var", "main",
    "master", "git", "com", "www", "http", "https",
];

/// Build the vocabulary hints for a session.
///
/// Sources, in descending value: the repository name, the branch (split into
/// its meaningful words, since branch slugs are exactly the identifiers people
/// say out loud), the working directory's leaf, and the agent name.
pub fn session_keyterms(
    working_directory: &str,
    git_branch: Option<&str>,
    repo_url: Option<&str>,
    agent_type: &str,
) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();

    if let Some(repo) = repo_url.and_then(repo_name) {
        push_term(&mut terms, &repo);
    }
    if let Some(branch) = git_branch {
        // A branch like `meawoppl/multi-provider-identity-model` is a phrase
        // someone will dictate almost verbatim, so contribute both the whole
        // slug and its words.
        push_term(&mut terms, branch);
        for word in split_slug(branch) {
            push_term(&mut terms, &word);
        }
    }
    if let Some(leaf) = path_leaf(working_directory) {
        push_term(&mut terms, &leaf);
    }
    push_term(&mut terms, agent_type);

    terms.truncate(MAX_KEYTERMS);
    terms
}

/// Add a term unless it is empty, a stop word, or already present
/// (case-insensitively).
fn push_term(terms: &mut Vec<String>, term: &str) {
    let term = term.trim().trim_matches('/').to_string();
    if term.len() < 3 || term.len() > 64 {
        return;
    }
    let lowered = term.to_ascii_lowercase();
    if STOP_TERMS.contains(&lowered.as_str()) {
        return;
    }
    if terms.iter().any(|t| t.to_ascii_lowercase() == lowered) {
        return;
    }
    terms.push(term);
}

/// `https://github.com/meawoppl/agent-portal.git` → `agent-portal`.
fn repo_name(repo_url: &str) -> Option<String> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let last = trimmed.rsplit('/').next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_string())
}

/// `/home/meawoppl/repos/agent-portal` → `agent-portal`.
fn path_leaf(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\']).next()?;
    (!last.is_empty()).then(|| last.to_string())
}

/// Split a branch slug into dictatable words: `meawoppl/github-oauth_provider`
/// → `meawoppl`, `github`, `oauth`, `provider`.
fn split_slug(slug: &str) -> Vec<String> {
    slug.split(['/', '-', '_', '.'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_repo_branch_directory_and_agent() {
        let terms = session_keyterms(
            "/home/meawoppl/repos/agent-portal",
            Some("meawoppl/github-oauth-provider"),
            Some("https://github.com/meawoppl/agent-portal.git"),
            "claude",
        );

        assert!(terms.contains(&"agent-portal".to_string()));
        assert!(terms.contains(&"meawoppl/github-oauth-provider".to_string()));
        assert!(terms.contains(&"oauth".to_string()));
        assert!(terms.contains(&"provider".to_string()));
        assert!(terms.contains(&"claude".to_string()));
    }

    /// The repo name arrives from both `repo_url` and the working directory —
    /// paying for it twice would waste a keyterm slot.
    #[test]
    fn duplicate_terms_are_collapsed_case_insensitively() {
        let terms = session_keyterms(
            "/home/meawoppl/repos/Agent-Portal",
            Some("agent-portal"),
            Some("https://github.com/meawoppl/agent-portal"),
            "claude",
        );
        let occurrences = terms
            .iter()
            .filter(|t| t.eq_ignore_ascii_case("agent-portal"))
            .count();
        assert_eq!(occurrences, 1, "got {terms:?}");
    }

    #[test]
    fn common_words_are_not_worth_a_keyterm_slot() {
        let terms = session_keyterms("/src", Some("main"), None, "claude");
        assert!(!terms.iter().any(|t| t.eq_ignore_ascii_case("main")));
        assert!(!terms.iter().any(|t| t.eq_ignore_ascii_case("src")));
    }

    #[test]
    fn a_session_with_no_git_context_still_yields_something_usable() {
        let terms = session_keyterms("/home/meawoppl/scratch", None, None, "codex");
        assert!(terms.contains(&"scratch".to_string()));
        assert!(terms.contains(&"codex".to_string()));
    }

    #[test]
    fn handles_ssh_remotes_and_trailing_slashes() {
        assert_eq!(
            repo_name("git@github.com:meawoppl/agent-portal.git"),
            Some("agent-portal".to_string())
        );
        assert_eq!(
            repo_name("https://github.com/meawoppl/agent-portal/"),
            Some("agent-portal".to_string())
        );
    }

    #[test]
    fn the_list_is_bounded() {
        let long_branch = (0..200)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join("-");
        let terms = session_keyterms("/tmp/x", Some(&long_branch), None, "claude");
        assert!(terms.len() <= MAX_KEYTERMS, "got {}", terms.len());
    }
}
