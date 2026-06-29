use crate::utils::{self, On401};
use shared::api::ScheduledTaskListResponse;
use shared::SessionInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[hook]
pub(super) fn use_scheduled_task_blocker(
    menu_session: Option<Uuid>,
    sessions: Vec<SessionInfo>,
) -> bool {
    let stop_has_tasks = use_state(|| false);

    {
        let stop_has_tasks = stop_has_tasks.clone();
        use_effect_with((menu_session, sessions), move |(menu_session, sessions)| {
            if let Some(session_id) = menu_session {
                if let Some(session) = sessions.iter().find(|session| session.id == *session_id) {
                    let working_directory = session.working_directory.clone();
                    let stop_has_tasks = stop_has_tasks.clone();
                    spawn_local(async move {
                        if let Ok(data) = utils::fetch_json::<ScheduledTaskListResponse>(
                            "/api/scheduled-tasks",
                            On401::Ignore,
                        )
                        .await
                        {
                            let has_scheduled_task = data.tasks.iter().any(|task| {
                                task.fields.working_directory == working_directory && task.enabled
                            });
                            stop_has_tasks.set(has_scheduled_task);
                        }
                    });
                }
            } else {
                stop_has_tasks.set(false);
            }

            || ()
        });
    }

    *stop_has_tasks
}
