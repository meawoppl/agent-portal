use super::message_renderer::truncate_str;
use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use yew::prelude::*;

thread_local! {
    static EXPANDED_SET: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

fn content_hash(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

#[derive(Properties, PartialEq)]
pub struct ExpandableTextProps {
    pub full_text: AttrValue,
    pub max_len: usize,
    /// Wrapper element tag: "pre", "div", or "span"
    #[prop_or("pre".into())]
    pub tag: AttrValue,
    #[prop_or_default]
    pub class: Classes,
}

/// Character-based expandable text. Shows truncated content with a clickable
/// toggle to reveal the full text. If the text fits within `max_len`, renders
/// as-is with no toggle.
#[function_component(ExpandableText)]
pub fn expandable_text(props: &ExpandableTextProps) -> Html {
    let render_trigger = use_state(|| 0u32);
    let hash = content_hash(&props.full_text);
    let expanded = EXPANDED_SET.with(|set| set.borrow().contains(&hash));
    let text = &*props.full_text;

    if text.len() <= props.max_len {
        return match props.tag.as_str() {
            "span" => html! { <span class={props.class.clone()}>{ text }</span> },
            "div" => html! { <div class={props.class.clone()}>{ text }</div> },
            _ => html! { <pre class={props.class.clone()}>{ text }</pre> },
        };
    }

    let remaining = text.len() - props.max_len;

    let toggle = {
        let render_trigger = render_trigger.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            EXPANDED_SET.with(|set| {
                let mut s = set.borrow_mut();
                if !s.remove(&hash) {
                    s.insert(hash);
                }
            });
            render_trigger.set(*render_trigger + 1);
        })
    };

    let (display, toggle_label) = if expanded {
        (text.to_string(), "show less".to_string())
    } else {
        (
            truncate_str(text, props.max_len).to_string(),
            format!("... {} more chars", remaining),
        )
    };

    match props.tag.as_str() {
        "span" => html! {
            <span class={props.class.clone()}>
                { &display }
                <span class="expandable-toggle" onclick={toggle}>{ toggle_label }</span>
            </span>
        },
        "div" => html! {
            <div class={props.class.clone()}>
                { &display }
                <span class="expandable-toggle" onclick={toggle}>{ toggle_label }</span>
            </div>
        },
        _ => html! {
            <pre class={props.class.clone()}>
                { &display }
                <span class="expandable-toggle" onclick={toggle}>{ toggle_label }</span>
            </pre>
        },
    }
}

#[derive(Properties, PartialEq)]
pub struct ExpandableLinesProps {
    pub content: AttrValue,
    pub max_lines: usize,
    #[prop_or_default]
    pub class: Classes,
}

/// Line-based expandable content for file previews. Shows the first N lines
/// with a clickable toggle to reveal all lines.
#[function_component(ExpandableLines)]
pub fn expandable_lines(props: &ExpandableLinesProps) -> Html {
    let render_trigger = use_state(|| 0u32);
    let hash = content_hash(&props.content);
    let expanded = EXPANDED_SET.with(|set| set.borrow().contains(&hash));
    let content = &*props.content;
    let all_lines: Vec<&str> = content.lines().collect();
    let total = all_lines.len();

    if total <= props.max_lines {
        return html! {
            <pre class={classes!(props.class.clone(), "write-content")}>
                { for all_lines.iter().enumerate().map(|(i, line)| html! {
                    <div class="write-line">
                        <span class="line-number">{ format!("{:>4}", i + 1) }</span>
                        <span class="line-content">{ *line }</span>
                    </div>
                })}
            </pre>
        };
    }

    let toggle = {
        let render_trigger = render_trigger.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            EXPANDED_SET.with(|set| {
                let mut s = set.borrow_mut();
                if !s.remove(&hash) {
                    s.insert(hash);
                }
            });
            render_trigger.set(*render_trigger + 1);
        })
    };

    let visible = if expanded {
        &all_lines[..]
    } else {
        &all_lines[..props.max_lines]
    };
    let remaining = total - props.max_lines;

    html! {
        <pre class={classes!(props.class.clone(), "write-content")}>
            { for visible.iter().enumerate().map(|(i, line)| html! {
                <div class="write-line">
                    <span class="line-number">{ format!("{:>4}", i + 1) }</span>
                    <span class="line-content">{ *line }</span>
                </div>
            })}
            <div class="write-truncated expandable-toggle" onclick={toggle}>
                { if expanded {
                    "show less".to_string()
                } else {
                    format!("... {} more lines", remaining)
                }}
            </div>
        </pre>
    }
}
