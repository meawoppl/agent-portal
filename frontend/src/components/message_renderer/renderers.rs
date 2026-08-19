//! Rendering functions for each message type, one module per message family.

mod assistant;
mod errors;
mod media;
mod portal;
mod result;
mod system;
mod tools;
mod user;

pub(crate) use assistant::assistant_label;
pub use assistant::{
    render_assistant_message, render_assistant_message_content, render_content_blocks,
};
pub use errors::{render_error_message, render_local_error, render_rate_limit_event};
pub(crate) use portal::{
    agent_message_event_from_agent_facing_text, portal_text, render_agent_message_body,
    render_agent_message_event, render_agent_message_from_source, render_portal_message,
    render_portal_message_content,
};
pub use result::render_result_message;
pub use system::{render_conversation_reset, render_system_message};
pub use user::{
    render_optimistic_user_message, render_optimistic_user_message_content, render_user_message,
    render_user_message_content,
};
