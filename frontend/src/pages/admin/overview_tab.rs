//! Admin overview tab — system stats and overview

use crate::utils;
use yew::prelude::*;

use super::AdminStats;

/// Format token count with K/M suffix for readability
fn format_tokens(count: i64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[derive(Properties, PartialEq)]
struct StatCardProps {
    label: String,
    value: String,
    #[prop_or_default]
    subvalue: Option<String>,
    #[prop_or_default]
    class: Option<String>,
}

#[function_component(StatCard)]
fn stat_card(props: &StatCardProps) -> Html {
    let class = classes!("admin-stat-card", props.class.clone());
    html! {
        <div class={class}>
            <div class="stat-value">{ &props.value }</div>
            <div class="stat-label">{ &props.label }</div>
            {
                if let Some(ref sub) = props.subvalue {
                    html! { <div class="stat-subvalue">{ sub }</div> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct AdminOverviewTabProps {
    pub stats: Option<AdminStats>,
}

#[function_component(AdminOverviewTab)]
pub fn admin_overview_tab(props: &AdminOverviewTabProps) -> Html {
    if let Some(ref s) = props.stats {
        html! {
            <div class="admin-overview">
                <div class="stats-grid">
                    <StatCard
                        label="Total Users"
                        value={s.total_users.to_string()}
                        subvalue={Some(format!("{} admins, {} disabled", s.admin_users, s.disabled_users))}
                    />
                    <StatCard
                        label="Total Sessions"
                        value={s.total_sessions.to_string()}
                        subvalue={Some(format!("{} active", s.active_sessions))}
                    />
                    <StatCard
                        label="Connected Clients"
                        value={format!("{}", s.connected_proxy_clients + s.connected_web_clients)}
                        subvalue={Some(format!("{} proxy, {} web", s.connected_proxy_clients, s.connected_web_clients))}
                    />
                    <StatCard
                        label="Total API Spend"
                        value={utils::format_dollars(s.total_spend_usd)}
                        class="spend-card"
                    />
                    <StatCard
                        label="Input Tokens"
                        value={format_tokens(s.total_input_tokens)}
                    />
                    <StatCard
                        label="Output Tokens"
                        value={format_tokens(s.total_output_tokens)}
                    />
                </div>
            </div>
        }
    } else {
        html! { <p>{ "No stats available" }</p> }
    }
}
