//! Dashboard page components
//!
//! This module contains the main dashboard page and its sub-components:
//! - `DashboardPage`: Main orchestrating component
//! - `SessionRail`: Horizontal carousel of session pills
//! - `SessionView`: Terminal view for a single session
//! - `PermissionDialog`: Permission prompt and AskUserQuestion dialogs

mod page;
mod permission_dialog;
mod session_rail;
mod session_view;
mod types;

pub use page::DashboardPage;
pub use types::{load_rail_orientation, save_rail_orientation, RailOrientation};
