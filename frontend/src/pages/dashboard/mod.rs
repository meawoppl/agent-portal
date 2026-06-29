//! Dashboard page components
//!
//! This module contains the main dashboard page and its sub-components:
//! - `DashboardPage`: Main orchestrating component
//! - `SessionRail`: Horizontal carousel of session pills
//! - `SessionView`: Terminal view for a single session
//! - `PermissionDialog`: Permission prompt and AskUserQuestion dialogs

mod page;
mod page_bootstrap;
mod page_state;
mod permission_dialog;
mod session_order;
mod session_rail;
mod session_view;
mod types;

pub use page::DashboardPage;
pub use types::{load_rail_position, save_rail_position, RailPosition};
