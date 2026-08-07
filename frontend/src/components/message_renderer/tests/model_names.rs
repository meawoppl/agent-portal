//! Model-name shortening used for assistant group labels and cost tooltips.

use super::super::shorten_model_name;

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
