//! Dashboard page components
//!
//! This module contains the main dashboard page and its sub-components:
//! - `DashboardPage`: Main orchestrating component
//! - `SessionRail`: Horizontal carousel of session pills
//! - `SessionView`: Terminal view for a single session
//! - `PermissionDialog`: Permission prompt and AskUserQuestion dialogs

mod page;
mod page_bootstrap;
mod page_focus;
mod page_spend;
mod page_state;
mod permission_dialog;
mod session_order;
mod session_rail;
mod session_view;
mod types;

pub use page::DashboardPage;
pub use types::{
    load_group_by_host, load_rail_position, load_vim_mode, save_group_by_host, save_rail_position,
    save_vim_mode, RailPosition,
};
