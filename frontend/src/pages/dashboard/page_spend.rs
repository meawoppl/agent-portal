use yew::prelude::*;

pub struct SpendBadgeAnimation {
    pub tier_class: &'static str,
    pub animating: bool,
}

/// Map total spend to its tier on the $1 / $10 / $100 / $1,000 / $10,000
/// ladder (0 = below $1, 5 = at or above $10,000).
fn spend_tier(spend: f64) -> u8 {
    if spend >= 10000.0 {
        5
    } else if spend >= 1000.0 {
        4
    } else if spend >= 100.0 {
        3
    } else if spend >= 10.0 {
        2
    } else if spend >= 1.0 {
        1
    } else {
        0
    }
}

/// CSS class for the spend badge, derived from the spend tier.
fn spend_tier_class(tier: u8) -> &'static str {
    match tier {
        5 => "spend-10000",
        4 => "spend-1000",
        3 => "spend-100",
        2 => "spend-10",
        1 => "spend-1",
        _ => "",
    }
}

fn animation_duration_ms(tier: u8) -> u32 {
    match tier {
        1 => 500,
        2 => 2000,
        3 => 5000,
        4 => 10000,
        _ => 20000,
    }
}

#[hook]
pub fn use_spend_badge_animation(total_user_spend: f64) -> SpendBadgeAnimation {
    let prev_spend_tier = use_state(|| 0u8);
    let spend_animating = use_state(|| false);
    let spend_initialized = use_state(|| false);
    let current_tier = spend_tier(total_user_spend);

    {
        let spend_animating = spend_animating.clone();
        let prev_spend_tier = prev_spend_tier.clone();
        let spend_initialized = spend_initialized.clone();
        use_effect_with(current_tier, move |tier| {
            let tier = *tier;
            if !*spend_initialized {
                // First tier value from page load: record it, don't animate.
                spend_initialized.set(true);
                prev_spend_tier.set(tier);
            } else if tier > *prev_spend_tier {
                spend_animating.set(true);
                let spend_animating = spend_animating.clone();
                let handle =
                    gloo::timers::callback::Timeout::new(animation_duration_ms(tier), move || {
                        spend_animating.set(false);
                    });
                prev_spend_tier.set(tier);
                handle.forget();
            } else if tier != *prev_spend_tier {
                prev_spend_tier.set(tier);
            }
            || ()
        });
    }

    SpendBadgeAnimation {
        tier_class: spend_tier_class(current_tier),
        animating: *spend_animating,
    }
}

#[cfg(test)]
mod tests {
    use super::{animation_duration_ms, spend_tier, spend_tier_class};

    #[test]
    fn spend_tier_uses_configured_thresholds() {
        assert_eq!(spend_tier(0.99), 0);
        assert_eq!(spend_tier(1.0), 1);
        assert_eq!(spend_tier(10.0), 2);
        assert_eq!(spend_tier(100.0), 3);
        assert_eq!(spend_tier(1000.0), 4);
        assert_eq!(spend_tier(10000.0), 5);
    }

    #[test]
    fn spend_tier_class_maps_to_existing_css_classes() {
        assert_eq!(spend_tier_class(0), "");
        assert_eq!(spend_tier_class(1), "spend-1");
        assert_eq!(spend_tier_class(2), "spend-10");
        assert_eq!(spend_tier_class(3), "spend-100");
        assert_eq!(spend_tier_class(4), "spend-1000");
        assert_eq!(spend_tier_class(5), "spend-10000");
    }

    #[test]
    fn animation_duration_grows_with_spend_tier() {
        assert_eq!(animation_duration_ms(1), 500);
        assert_eq!(animation_duration_ms(2), 2000);
        assert_eq!(animation_duration_ms(3), 5000);
        assert_eq!(animation_duration_ms(4), 10000);
        assert_eq!(animation_duration_ms(5), 20000);
    }
}
