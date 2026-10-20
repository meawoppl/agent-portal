//! Model-name shortening used for assistant group labels and cost tooltips.

use super::super::{codex_label, shorten_codex_model_name, shorten_model_name};

#[test]
fn test_shorten_model_name() {
    assert_eq!(
        shorten_model_name("claude-opus-4-5-20251101"),
        Some("Opus 4.5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-sonnet-4-5-20250929"),
        Some("Sonnet 4.5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-haiku-4-5-20251001"),
        Some("Haiku 4.5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-3-5-sonnet-20241022"),
        Some("Sonnet 3.5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-opus-4-6"),
        Some("Opus 4.6".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-opus-4-7[1m]"),
        Some("Opus 4.7".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-sonnet-4-5"),
        Some("Sonnet 4.5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-fable-5"),
        Some("Fable 5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-mythos-5"),
        Some("Mythos 5".to_string())
    );
    assert_eq!(
        shorten_model_name("claude-fable-5-20260601"),
        Some("Fable 5".to_string())
    );
    assert_eq!(shorten_model_name("claude-opus"), Some("Opus".to_string()));
    assert_eq!(shorten_model_name(""), None);
    assert_eq!(shorten_model_name("<unknown>"), None);
    assert_eq!(shorten_model_name("gpt-4-turbo"), Some("gpt".to_string()));
}

#[test]
fn codex_models_shorten_to_family_and_version() {
    for (id, expected) in [
        ("gpt-5.6-sol", "Sol 5.6"),
        ("gpt-6-astra", "Astra 6"),
        ("gpt-5.5-codex", "5.5"),
        ("gpt-5-codex-mini", "Mini 5"),
        ("gpt-5.5", "5.5"),
        ("gpt-5-2025-08-07", "5"),
    ] {
        assert_eq!(
            shorten_codex_model_name(id).as_deref(),
            Some(expected),
            "{id}"
        );
    }
    for id in ["", "o4-mini", "gpt-", "gpt-oss", "claude-opus-4-8"] {
        assert_eq!(shorten_codex_model_name(id), None, "{id}");
    }
}

#[test]
fn codex_name_tag_includes_the_model_when_known() {
    assert_eq!(codex_label(Some("gpt-5.6-sol")), "Codex - Sol 5.6");
    assert_eq!(codex_label(Some("gpt-6-astra")), "Codex - Astra 6");
    assert_eq!(codex_label(Some("o4-mini")), "Codex");
    assert_eq!(codex_label(None), "Codex");
}
