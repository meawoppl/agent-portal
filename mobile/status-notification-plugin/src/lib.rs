use serde::Serialize;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

#[cfg(target_os = "android")]
use tauri::plugin::PluginHandle;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Plugin(#[from] tauri::plugin::mobile::PluginInvokeError),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusNotificationPayload {
    pub title: String,
    pub summary: String,
    pub dashboard_url: String,
    pub status_url: String,
    pub auth_token: String,
    pub sessions_json: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearStatusNotification;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusNotificationLine {
    pub session_id: String,
    pub name: String,
    pub state: String,
    pub url: String,
}

#[cfg(target_os = "android")]
mod mobile {
    use serde::de::DeserializeOwned;
    use tauri::{
        plugin::{PluginApi, PluginHandle},
        AppHandle, Runtime,
    };

    const PLUGIN_IDENTIFIER: &str = "io.txcl.agentportal.status";

    pub fn init<R: Runtime, C: DeserializeOwned>(
        _app: &AppHandle<R>,
        api: PluginApi<R, C>,
    ) -> crate::Result<PluginHandle<R>> {
        api.register_android_plugin(PLUGIN_IDENTIFIER, "StatusNotificationPlugin")
            .map_err(Into::into)
    }
}

pub struct StatusNotification<R: Runtime>(
    #[cfg(target_os = "android")] PluginHandle<R>,
    #[cfg(not(target_os = "android"))] std::marker::PhantomData<fn() -> R>,
);

impl<R: Runtime> StatusNotification<R> {
    pub fn show(&self, payload: StatusNotificationPayload) -> Result<()> {
        #[cfg(target_os = "android")]
        {
            self.0
                .run_mobile_plugin("show", payload)
                .map_err(Into::into)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = payload;
            Ok(())
        }
    }

    pub fn clear(&self) -> Result<()> {
        #[cfg(target_os = "android")]
        {
            self.0
                .run_mobile_plugin("clear", ClearStatusNotification)
                .map_err(Into::into)
        }
        #[cfg(not(target_os = "android"))]
        {
            Ok(())
        }
    }
}

pub trait StatusNotificationExt<R: Runtime> {
    fn status_notification(&self) -> &StatusNotification<R>;
}

impl<R: Runtime, T: Manager<R>> StatusNotificationExt<R> for T {
    fn status_notification(&self) -> &StatusNotification<R> {
        self.state::<StatusNotification<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("status-notification")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let notification = StatusNotification(mobile::init(app, api)?);
            #[cfg(not(target_os = "android"))]
            let notification = StatusNotification(std::marker::PhantomData);
            app.manage(notification);
            Ok(())
        })
        .build()
}
