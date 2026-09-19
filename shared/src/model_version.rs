//! Compact model-version extraction for the session-pill watermark.
//!
//! The dashboard rail renders a faint model number next to the agent logo on
//! each session pill so a glance distinguishes, say, an Opus 4.8 session from
//! a Fable 5 one. This module turns a full model **id** (as it arrives from the
//! agent's per-turn result, e.g. `"claude-opus-4-8"` or `"gpt-5.5-codex"`) into
//! a compact token like `"4.8"` / `"5.5"`.
//!
//! Why digit-pattern extraction rather than the SDK model catalogs
//! (`claude_codes::ClaudeModel` / `codex_codes::CodexModel`): the catalogs key
//! on picker **cli args** (`"opus"`, `"sonnet"`, aliases) — not the fully
//! qualified id string the runtime reports on each turn — so they can't map
//! `"claude-haiku-4-5-20251001"` back to a version. The id form always carries
//! the version as a dash- or dot-separated run of small numbers, optionally
//! followed by a date/build suffix (`20251001`), so we extract that directly.
//! This also gracefully handles ids the catalog has never heard of (a
//! newly-shipped model): as long as it follows the `family-<version>` shape we
//! still get a sensible token, and anything unrecognizable yields `None` (the
//! caller renders the logo alone — never the raw id).

/// Extract a compact display version from a model id string.
///
/// Returns `None` when no plausible version can be found, so the caller renders
/// the logo watermark alone rather than an ugly raw id.
///
/// Examples:
/// ```
/// use shared::compact_model_version;
/// assert_eq!(compact_model_version("claude-opus-4-8").as_deref(), Some("4.8"));
/// assert_eq!(compact_model_version("claude-fable-5").as_deref(), Some("5"));
/// assert_eq!(compact_model_version("claude-sonnet-5").as_deref(), Some("5"));
/// assert_eq!(
///     compact_model_version("claude-haiku-4-5-20251001").as_deref(),
///     Some("4.5")
/// );
/// assert_eq!(compact_model_version("gpt-5.5-codex").as_deref(), Some("5.5"));
/// assert_eq!(compact_model_version("garbled-nonsense"), None);
/// ```
pub fn compact_model_version(model_id: &str) -> Option<String> {
    // Version components are short numbers. A pure-digit run of 4+ digits is a
    // date/build snapshot (`20251001`, `1106`), not a version part, so we treat
    // it as a terminator rather than a component.
    const MAX_VERSION_PART_DIGITS: usize = 3;

    #[derive(PartialEq)]
    enum Kind {
        /// A version component, e.g. `4`, `8`, or `5.5` (dotted is always one).
        Version,
        /// A date/build suffix like `20251001` — terminates the version run.
        DateOrBuild,
        /// Anything else (a family word like `opus`, `codex`).
        Other,
    }

    fn classify(tok: &str) -> Kind {
        if tok.is_empty() {
            return Kind::Other;
        }
        // Numeric-ish: digits with optional internal dots (`5`, `5.5`).
        let numeric_ish = tok
            .split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()));
        if !numeric_ish {
            return Kind::Other;
        }
        if tok.contains('.') {
            // A dotted number is unambiguously a version ("5.5").
            Kind::Version
        } else if tok.len() <= MAX_VERSION_PART_DIGITS {
            Kind::Version
        } else {
            Kind::DateOrBuild
        }
    }

    let mut parts: Vec<&str> = Vec::new();
    for tok in model_id.split('-') {
        match classify(tok) {
            Kind::Version => parts.push(tok),
            // Once the version run has started, any non-version token ends it
            // (so a trailing date or family suffix isn't mixed in). Before it
            // starts, skip leading family words.
            Kind::DateOrBuild | Kind::Other => {
                if !parts.is_empty() {
                    break;
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

/// Default Claude context window when nothing marks the model as larger.
/// Mirrors the CLI's `ber` constant (verified in Claude Code 2.1.220).
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
/// Window for models flagged as 1M-context.
const MILLION_CONTEXT_WINDOW: u64 = 1_000_000;

/// Models the CLI's capability table marked `native_1m` — a 1M window with
/// **no `[1m]` tag required.
///
/// **FROZEN legacy-fallback data (#1533) — do not maintain.** Live Claude
/// turns carry the CLI's own resolved window on the wire
/// (`ResultMessage.model_usage[model].context_window` →
/// `TurnMetrics.model_context_window`), which supersedes this table for all
/// new data: a newly shipped 1M model reports its own window, entitlement and
/// provider gating included, with no transcription step. This list exists
/// solely so rows recorded *before* proxies forwarded that value keep a
/// sensible gauge; it was transcribed from the Claude Code 2.1.220 binary
/// (each id carrying `context: { window: 1e6, native_1m: true }`) and is
/// deliberately never updated for newer CLIs — models released after the
/// wire-forwarding era never need it.
const NATIVE_1M_MODELS: &[&str] = &[
    "claude-fable-5",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-5",
];

/// True when `model_id` is one of [`NATIVE_1M_MODELS`], allowing a trailing
/// date/build suffix (`claude-opus-4-8-20260101`) but not a longer version
/// (`claude-opus-4-80`).
fn is_native_1m_model(model_id: &str) -> bool {
    NATIVE_1M_MODELS.iter().any(|base| {
        model_id == *base
            || model_id
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// True when the model id carries the CLI's `[1m]` tag.
///
/// The CLI's own test is a bare regex on the id — `function Wb(e){ return
/// /\[1m\]/i.test(e) }` — and ids like `claude-opus-4-7[1m]` appear verbatim on
/// the wire (see the `claude-codes` result fixture), so matching the tag is
/// exact, not a heuristic.
fn has_one_million_tag(model_id: &str) -> bool {
    crate::strings::contains_case_insensitive(model_id, "[1m]")
}

/// Nominal context-window size (in tokens) for a Claude model id.
///
/// **Fallback only.** Live Claude turns carry the CLI's own resolved window
/// on the wire (`ResultMessage.model_usage[model].context_window`, forwarded
/// as `TurnMetrics.model_context_window` — #1529), which is authoritative:
/// it reflects native-1M capability, entitlement/provider gating, and the
/// agent host's `CLAUDE_CODE_MAX_CONTEXT_TOKENS`. This map serves rows
/// recorded before proxies forwarded that value, and any turn that reports
/// nothing. Anything unrecognized returns `None` (caller hides the gauge
/// rather than guess).
///
/// Mirrors the resolution order the CLI uses (`mZc`, Claude Code 2.1.220):
///
/// 1. the `[1m]` id tag ⇒ 1M,
/// 2. the `native_1m` capability or `claude-mythos-preview` ⇒ 1M,
/// 3. a per-model override,
/// 4. `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, for non-`claude-` ids only,
/// 5. otherwise 200k.
///
/// **Known gaps vs. the CLI** (#1517/#1529/#1533 — acceptable for a fallback):
/// steps 2–4 are only partially reachable from a model id. `native_1m` is
/// covered by the frozen [`NATIVE_1M_MODELS`] list, so 1M models newer than
/// the freeze are missed here (they never need this path — their turns carry
/// the wire window); the per-request 1M beta *header* isn't visible here at
/// all; and the env override is read from the environment of whichever host
/// evaluates this fn, which matches the agent host only in single-host
/// deployments. Those cases under-report the window (a session reads fuller
/// than it is) rather than over-reporting it.
pub fn context_window_for(model_id: &str) -> Option<u64> {
    let id = model_id.to_ascii_lowercase();
    let is_claude = ["opus", "sonnet", "haiku"]
        .iter()
        .any(|family| id.contains(family))
        || id.starts_with("claude");
    if !is_claude {
        return None;
    }

    // 1. Explicit `[1m]` tag on the id.
    if has_one_million_tag(&id) {
        return Some(MILLION_CONTEXT_WINDOW);
    }
    // 2. Models that are natively 1M: the transcribed capability table, plus
    //    `claude-mythos-preview`, which the CLI special-cases by name.
    if is_native_1m_model(&id) || id.contains("mythos-preview") {
        return Some(MILLION_CONTEXT_WINDOW);
    }
    // 4. Env override. The CLI applies it only to non-`claude-` ids (so it can't
    //    silently misreport a first-party model), and we keep that restriction.
    if !id.starts_with("claude-") {
        if let Some(env_window) = env_max_context_tokens() {
            return Some(env_window);
        }
    }
    // 5. Default.
    Some(DEFAULT_CONTEXT_WINDOW)
}

/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, when set to a positive integer.
///
/// WASM has no process environment, so this is a compile-time `None` there —
/// the frontend never resolves windows itself (it consumes the value computed
/// where the turn was recorded), so that costs nothing.
fn env_max_context_tokens() -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS")
            .ok()?
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_model_version, context_window_for, NATIVE_1M_MODELS};

    #[test]
    fn claude_dashed_major_minor() {
        assert_eq!(
            compact_model_version("claude-opus-4-8").as_deref(),
            Some("4.8")
        );
    }

    #[test]
    fn claude_single_component() {
        assert_eq!(
            compact_model_version("claude-fable-5").as_deref(),
            Some("5")
        );
        assert_eq!(
            compact_model_version("claude-sonnet-5").as_deref(),
            Some("5")
        );
    }

    #[test]
    fn claude_with_date_suffix_dropped() {
        assert_eq!(
            compact_model_version("claude-haiku-4-5-20251001").as_deref(),
            Some("4.5")
        );
    }

    #[test]
    fn codex_dotted_version() {
        assert_eq!(
            compact_model_version("gpt-5.5-codex").as_deref(),
            Some("5.5")
        );
    }

    #[test]
    fn codex_trailing_family_word_stops_run() {
        // The `-codex` suffix must not leak into the token.
        assert_eq!(compact_model_version("gpt-5-codex").as_deref(), Some("5"));
    }

    #[test]
    fn unknown_or_garbled_yields_none() {
        assert_eq!(compact_model_version(""), None);
        assert_eq!(compact_model_version("garbled-nonsense"), None);
        assert_eq!(compact_model_version("claude"), None);
        // A bare uuid-ish token has no small numeric run.
        assert_eq!(compact_model_version("abc123def"), None);
    }

    #[test]
    fn date_only_after_family_is_none() {
        // No version parts before the date suffix → nothing to show.
        assert_eq!(compact_model_version("some-model-20251001"), None);
    }

    #[test]
    fn three_component_version_joins_all() {
        assert_eq!(compact_model_version("foo-1-2-3").as_deref(), Some("1.2.3"));
    }

    /// Recognized Claude families resolve to the default window. Uses only ids
    /// the CLI's capability table actually puts at 200k — `claude-opus-4-8`,
    /// `claude-sonnet-5`, and `claude-fable-5` were originally asserted here at
    /// 200k, which the 2.1.220 capability table shows is wrong: they are
    /// `native_1m` (see `context_window_honors_natively_one_million_models`).
    #[test]
    fn context_window_recognizes_claude_families() {
        for id in [
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-6",
        ] {
            assert_eq!(context_window_for(id), Some(200_000), "{id}");
        }
    }

    #[test]
    fn context_window_none_for_non_claude_or_unknown() {
        assert_eq!(context_window_for("gpt-5-codex"), None);
        assert_eq!(context_window_for(""), None);
        assert_eq!(context_window_for("garbled-nonsense"), None);
    }

    /// The `[1m]` tag means a 1M window (#1517). Before this, a tagged model
    /// resolved to 200k and the fullness gauge read 5x too full. The tag appears
    /// verbatim on the wire — the `claude-codes` result fixture carries
    /// `claude-opus-4-7[1m]`.
    #[test]
    fn context_window_honors_the_one_million_tag() {
        for id in [
            "claude-opus-4-7[1m]",
            "claude-sonnet-4-6[1M]",
            "claude-opus-4-8[1m]",
        ] {
            assert_eq!(context_window_for(id), Some(1_000_000), "{id}");
        }
        // A 200k model without the tag stays at the default. (Deliberately not
        // `claude-opus-4-7` as originally written — that one is `native_1m`, so
        // it is 1M tagged or not.)
        assert_eq!(context_window_for("claude-sonnet-4-6"), Some(200_000));
        assert_eq!(context_window_for("claude-opus-4-6"), Some(200_000));
    }

    /// Natively-1M models are 1M without needing the tag, via the frozen
    /// legacy list (see the fn docs — live turns carry the wire window and
    /// never consult it).
    #[test]
    fn context_window_honors_natively_one_million_models() {
        assert_eq!(context_window_for("claude-mythos-preview"), Some(1_000_000));
    }

    /// The transcribed [`NATIVE_1M_MODELS`] table is pinned exactly: adding,
    /// removing, or renaming an entry fails here, so table rot surfaces as
    /// red rather than a wrong context gauge.
    #[test]
    fn context_window_honors_every_transcribed_native_1m_model() {
        assert_eq!(
            NATIVE_1M_MODELS,
            [
                "claude-fable-5",
                "claude-opus-4-7",
                "claude-opus-4-8",
                "claude-opus-5",
                "claude-sonnet-5",
            ]
            .as_slice()
        );
        for id in NATIVE_1M_MODELS {
            assert_eq!(context_window_for(id), Some(1_000_000), "{id}");
        }
        // Trailing date/build suffixes still match.
        assert_eq!(
            context_window_for("claude-opus-4-8-20260101"),
            Some(1_000_000)
        );
    }

    /// The tag still wins for an id that is neither a known family nor
    /// `claude-`-prefixed, so an unrecognized-but-tagged model isn't dropped.
    #[test]
    fn context_window_tag_applies_across_recognized_families() {
        assert_eq!(context_window_for("claude-haiku-4-5[1m]"), Some(1_000_000));
        // Still `None` when nothing identifies it as Claude at all.
        assert_eq!(context_window_for("some-other-model[1m]"), None);
    }
}
