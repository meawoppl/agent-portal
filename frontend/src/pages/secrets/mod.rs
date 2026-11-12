//! Metadata-only vault management. Saved plaintext is intentionally absent
//! from this page's model, requests, rendered cards and version history.
mod api;
mod authorize;
mod value_form;
use crate::{
    utils::{self, On401},
    Route,
};
pub use authorize::SecretAuthorizePage;
use shared::api::{
    SecretCreateRequest, SecretCredentialMetadata, SecretCredentialsResponse, SecretExpungeJob,
    SecretExpungeRequest, SecretExpungeState, SecretGrantRequest, SecretListResponse, SecretMember,
    SecretMembersResponse, SecretMetadata, SecretMutationRequest, SecretNamespace,
    SecretRestoreRequest, SecretRight, SecretValue, SecretVersionsResponse, SecretWriteRequest,
};
use uuid::Uuid;
use value_form::{FreshValue, ValueForm};
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Active,
    Deleted,
}
impl Tab {
    fn query(self) -> &'static str {
        match self {
            Self::Active => "deleted=false",
            Self::Deleted => "deleted=true",
        }
    }
}
fn deleted(secret: &SecretMetadata) -> bool {
    secret.tags.iter().any(|tag| tag == "deleted")
}
fn visible(secret: &SecretMetadata, tab: Tab) -> bool {
    deleted(secret) == (tab == Tab::Deleted)
}
fn audience(secret: &SecretMetadata) -> String {
    if secret.namespace == SecretNamespace::Organization {
        "Every active organization member, including future members".into()
    } else if secret.grants.is_empty() {
        "Owner only".into()
    } else {
        format!("Owner and {} selected member(s)", secret.grants.len())
    }
}

#[derive(Properties, PartialEq, Default)]
pub struct SecretsProps {
    #[prop_or_default]
    pub job_id: Option<String>,
    #[prop_or_default]
    pub embedded: bool,
}
pub enum Msg {
    Load,
    RefreshCredentials,
    Credentials(Result<SecretCredentialsResponse, String>),
    RevokeCredential(Uuid),
    CredentialRevoked(Result<(), String>),
    Tab(Tab),
    Loaded(Tab, Result<SecretListResponse, String>),
    Select(Uuid),
    Versions(Uuid, Result<SecretVersionsResponse, String>),
    Create,
    Replace,
    Cancel,
    Save(FreshValue),
    Finished(Result<(), String>),
    ConfirmDelete(bool),
    Delete,
    ConfirmRestore(bool),
    Restore,
    ShareUser(String),
    Share,
    Unshare(Uuid),
    Expunge,
    GlobalCleanup(bool),
    Job(Result<SecretExpungeJob, String>),
    RefreshJob,
    Members(Result<SecretMembersResponse, utils::FetchError>),
}
pub struct SecretsPage {
    tab: Tab,
    response: Option<SecretListResponse>,
    selected: Option<Uuid>,
    versions: Option<SecretVersionsResponse>,
    editing: Option<bool>,
    busy: bool,
    error: Option<String>,
    confirm_delete: bool,
    confirm_restore: bool,
    global_cleanup: bool,
    members: Vec<SecretMember>,
    directory_restricted: bool,
    share_user: Option<Uuid>,
    job: Option<SecretExpungeJob>,
    credentials: Option<Vec<SecretCredentialMetadata>>,
    heading_ref: NodeRef,
    focus_heading: bool,
}
impl SecretsPage {
    fn current(&self) -> Option<&SecretMetadata> {
        self.response
            .as_ref()?
            .secrets
            .iter()
            .find(|s| Some(s.id) == self.selected && visible(s, self.tab))
    }
    fn mutate<T: serde::Serialize + 'static>(
        &mut self,
        ctx: &Context<Self>,
        method: &'static str,
        path: String,
        request: T,
    ) {
        self.busy = true;
        self.error = None;
        self.editing = None;
        let link = ctx.link().clone();
        spawn_local(async move {
            let result = api::mutate(method, &path, &request).await.map(|_| ());
            link.send_message(Msg::Finished(result));
        });
    }
}
impl Component for SecretsPage {
    type Message = Msg;
    type Properties = SecretsProps;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Load);
        ctx.link().send_message(Msg::RefreshCredentials);
        let link = ctx.link().clone();
        spawn_local(async move {
            link.send_message(Msg::Members(
                utils::fetch_json::<SecretMembersResponse>("/api/secrets/members", On401::Ignore)
                    .await,
            ));
        });
        if ctx.props().job_id.is_some() {
            ctx.link().send_message(Msg::RefreshJob);
        }
        Self {
            tab: Tab::Active,
            response: None,
            selected: None,
            versions: None,
            editing: None,
            busy: false,
            error: None,
            confirm_delete: false,
            confirm_restore: false,
            global_cleanup: false,
            members: vec![],
            directory_restricted: false,
            share_user: None,
            job: None,
            credentials: None,
            heading_ref: NodeRef::default(),
            focus_heading: false,
        }
    }
    fn rendered(&mut self, _ctx: &Context<Self>, _first_render: bool) {
        // View changes replace the inventory, so move focus to its new title.
        if self.focus_heading {
            self.focus_heading = false;
            if let Some(heading) = self.heading_ref.cast::<web_sys::HtmlElement>() {
                let _ = heading.focus();
            }
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if ctx.props().job_id != old.job_id {
            self.job = None;
            ctx.link().send_message(Msg::RefreshJob);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::RefreshCredentials => {
                let link = ctx.link().clone();
                spawn_local(async move {
                    link.send_message(Msg::Credentials(
                        utils::fetch_json::<SecretCredentialsResponse>(
                            "/api/secrets/authorize/credentials",
                            On401::Ignore,
                        )
                        .await
                        .map_err(|_| "Could not load approved CLI access.".into()),
                    ));
                });
                false
            }
            Msg::Credentials(result) => {
                match result {
                    Ok(response) => self.credentials = Some(response.credentials),
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::RevokeCredential(id) => {
                if self.busy {
                    return false;
                }
                self.busy = true;
                self.error = None;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = api::mutate(
                        "POST",
                        &format!("/api/secrets/authorize/credentials/{id}/revoke"),
                        &(),
                    )
                    .await
                    .map(|_| ());
                    link.send_message(Msg::CredentialRevoked(result));
                });
                true
            }
            Msg::CredentialRevoked(result) => {
                self.busy = false;
                match result {
                    Ok(()) => ctx.link().send_message(Msg::RefreshCredentials),
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::Load => {
                self.confirm_restore = false;
                self.global_cleanup = false;
                self.confirm_delete = false;
                let tab = self.tab;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = utils::fetch_json::<SecretListResponse>(
                        &format!("/api/secrets?{}", tab.query()),
                        On401::Ignore,
                    )
                    .await
                    .map_err(|_| "Could not load secrets. Sign in again or retry.".into());
                    link.send_message(Msg::Loaded(tab, result));
                });
                true
            }
            Msg::Tab(tab) => {
                self.focus_heading = true;
                self.tab = tab;
                self.response = None;
                self.selected = None;
                self.editing = None;
                self.versions = None;
                self.confirm_delete = false;
                self.confirm_restore = false;
                self.global_cleanup = false;
                ctx.link().send_message(Msg::Load);
                true
            }
            Msg::Loaded(tab, result) => {
                self.confirm_restore = false;
                self.global_cleanup = false;
                self.confirm_delete = false;
                if tab != self.tab {
                    return false;
                }
                match result {
                    Ok(data) => {
                        self.response = Some(data);
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::Select(id) => {
                self.selected = Some(id);
                self.versions = None;
                self.editing = None;
                self.confirm_delete = false;
                self.confirm_restore = false;
                self.global_cleanup = false;
                self.share_user = None;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = utils::fetch_json::<SecretVersionsResponse>(
                        &format!("/api/secrets/{id}/versions"),
                        On401::Ignore,
                    )
                    .await
                    .map_err(|_| "Version metadata unavailable.".into());
                    link.send_message(Msg::Versions(id, result));
                });
                true
            }
            Msg::Versions(id, result) => {
                if self.selected != Some(id) {
                    return false;
                }
                match result {
                    Ok(data) => self.versions = Some(data),
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::Create => {
                self.editing = Some(false);
                self.error = None;
                true
            }
            Msg::Replace => {
                self.editing = Some(true);
                self.error = None;
                true
            }
            Msg::Cancel => {
                self.editing = None;
                true
            }
            Msg::Save(fresh) => {
                if self.busy {
                    return false;
                }
                if self.editing == Some(true) {
                    let Some(secret) = self.current() else {
                        return false;
                    };
                    self.mutate(
                        ctx,
                        "POST",
                        format!("/api/secrets/{}/versions", secret.id),
                        SecretWriteRequest {
                            expected_generation: secret.generation,
                            value: SecretValue(fresh.value),
                        },
                    );
                } else {
                    self.mutate(
                        ctx,
                        "POST",
                        "/api/secrets".into(),
                        SecretCreateRequest {
                            namespace: if fresh.organization {
                                SecretNamespace::Organization
                            } else {
                                SecretNamespace::Personal
                            },
                            name: fresh.name,
                            description: fresh.description,
                            value: SecretValue(fresh.value),
                        },
                    );
                }
                true
            }
            Msg::Finished(result) => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.error = None;
                        self.selected = None;
                        self.versions = None;
                        self.confirm_delete = false;
                        self.confirm_restore = false;
                        self.global_cleanup = false;
                        ctx.link().send_message(Msg::Load);
                    }
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::ConfirmDelete(value) => {
                self.confirm_delete = value;
                true
            }
            Msg::ConfirmRestore(value) => {
                self.confirm_restore = value;
                true
            }
            Msg::Delete => {
                if !self.confirm_delete || self.busy {
                    return false;
                }
                let Some(secret) = self.current() else {
                    return false;
                };
                self.mutate(
                    ctx,
                    "POST",
                    format!("/api/secrets/{}/delete", secret.id),
                    SecretMutationRequest {
                        expected_generation: secret.generation,
                    },
                );
                true
            }
            Msg::Restore => {
                if !self.confirm_restore || self.busy {
                    return false;
                }
                let Some(secret) = self.current() else {
                    return false;
                };
                let Some(request) = restore_request(secret, self.confirm_restore) else {
                    return false;
                };
                self.mutate(
                    ctx,
                    "POST",
                    format!("/api/secrets/{}/restore", secret.id),
                    request,
                );
                true
            }
            Msg::ShareUser(value) => {
                self.share_user = Uuid::parse_str(&value).ok();
                true
            }
            Msg::Share => {
                if self.busy {
                    return false;
                }
                let (Some(secret), Some(user_id)) = (self.current(), self.share_user) else {
                    return false;
                };
                self.mutate(
                    ctx,
                    "POST",
                    format!("/api/secrets/{}/grants", secret.id),
                    SecretGrantRequest {
                        expected_generation: secret.generation,
                        user_id,
                        rights: vec![SecretRight::Read, SecretRight::Use],
                    },
                );
                true
            }
            Msg::Unshare(user) => {
                if self.busy {
                    return false;
                }
                let Some(secret) = self.current() else {
                    return false;
                };
                self.mutate(
                    ctx,
                    "DELETE",
                    format!(
                        "/api/secrets/{}/grants/{user}?expected_generation={}",
                        secret.id, secret.generation
                    ),
                    SecretMutationRequest {
                        expected_generation: secret.generation,
                    },
                );
                true
            }
            Msg::Members(result) => {
                match result {
                    Ok(data) => {
                        self.members = data.members;
                        self.directory_restricted = false;
                    }
                    Err(utils::FetchError::Status(403)) => {
                        self.members.clear();
                        self.directory_restricted = true;
                    }
                    Err(_) => self.error = Some("Member directory unavailable.".into()),
                }
                true
            }
            Msg::GlobalCleanup(value) => {
                self.global_cleanup = value
                    && self
                        .response
                        .as_ref()
                        .is_some_and(|response| response.can_manage_organization);
                true
            }
            Msg::Expunge => {
                if self.busy {
                    return false;
                }
                let Some(secret) = self.current() else {
                    return false;
                };
                let path = format!("/api/secrets/{}/expunge", secret.id);
                let global = self.global_cleanup
                    && self
                        .response
                        .as_ref()
                        .is_some_and(|response| response.can_manage_organization);
                self.busy = true;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result=match api::mutate("POST",&path,&SecretExpungeRequest{all_versions:true,global}).await{Ok(response)=>response.json::<SecretExpungeJob>().await.map_err(|_| "Cleanup started but its status could not be read. Refresh job status.".into()),Err(error)=>Err(error)};
                    link.send_message(Msg::Job(result));
                });
                true
            }
            Msg::Job(result) => {
                self.busy = false;
                match result {
                    Ok(job) => {
                        self.job = Some(job);
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::RefreshJob => {
                let id = self
                    .job
                    .as_ref()
                    .map(|j| j.id.to_string())
                    .or_else(|| ctx.props().job_id.clone());
                if let Some(id) = id.and_then(|id| Uuid::parse_str(&id).ok()) {
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::Job(
                            utils::fetch_json::<SecretExpungeJob>(
                                &format!("/api/secrets/jobs/{id}"),
                                On401::Ignore,
                            )
                            .await
                            .map_err(|_| "Could not load cleanup status.".into()),
                        ));
                    });
                }
                false
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let can_publish = self
            .response
            .as_ref()
            .is_some_and(|r| r.can_manage_organization);
        let title = if self.tab == Tab::Deleted {
            "Deleted secrets"
        } else {
            "Active secrets"
        };
        html! {<section class={classes!("secrets-page",ctx.props().embedded.then_some("secrets-embedded"))} aria-label="Secrets management">
            <header class="secrets-heading"><div>
                if ctx.props().embedded {<h2 ref={self.heading_ref.clone()} tabindex="-1">{title}</h2>} else {<h1 ref={self.heading_ref.clone()} tabindex="-1">{title}</h1>}
                <p>{"Manage access and versions. Saved values stay hidden."}</p>
            </div>
            if !ctx.props().embedded {<Link<Route> to={Route::Dashboard}>{"Back to sessions"}</Link<Route>>}
            <div class="secrets-toolbar">
                if self.tab==Tab::Deleted {
                    <button type="button" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Tab(Tab::Active))}>{"Back to active secrets"}</button>
                } else {
                    <button type="button" class="secrets-add" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Create)}>{"Add secret"}</button>
                }
                <button type="button" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Load)}>{"Refresh"}</button>
            </div>
            </header>
            <crate::components::secret_verification::SecretVerification />
            if let Some(error)=&self.error {<p role="alert" class="secrets-error">{error}</p>}
            if self.tab==Tab::Deleted {<p class="secrets-notice">{"Deleted secrets cannot be used. Restore to enable access again. All versions are retained and names stay reserved."}</p>}
            if let Some(replacing)=self.editing {<ValueForm key={format!("{}-{replacing}",self.selected.map(|id|id.to_string()).unwrap_or_default())} {replacing} can_publish={can_publish} on_submit={ctx.link().callback(Msg::Save)} on_cancel={ctx.link().callback(|_|Msg::Cancel)} />}
            {self.namespace_table(ctx, SecretNamespace::Personal)}
            <hr class="secrets-divider"/>
            {self.namespace_table(ctx, SecretNamespace::Organization)}
            {self.detail(ctx)}
            if self.tab==Tab::Active {
                <footer class="secrets-deleted-navigation">
                    <button type="button" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Tab(Tab::Deleted))}>{"Show deleted secrets"}</button>
                </footer>
            }
            {self.credentials_view(ctx)}
            if let Some(job)=&self.job {{render_job(job,ctx.link().callback(|_|Msg::RefreshJob))}}
        </section>}
    }
}

impl SecretsPage {
    fn namespace_table(&self, ctx: &Context<Self>, namespace: SecretNamespace) -> Html {
        let (title, description) = match namespace {
            SecretNamespace::Personal => (
                "Personal",
                "Your personal secrets and those shared with you.",
            ),
            SecretNamespace::Organization => (
                "Organization",
                "Available to all active organization members, including future members.",
            ),
        };
        let description = if self.tab == Tab::Deleted {
            match namespace {
                SecretNamespace::Personal => "Deleted personal secrets and those shared with you.",
                SecretNamespace::Organization => "Organization secrets retained for restoration. Audience is confirmed when restoring.",
            }
        } else {
            description
        };
        let rows = self.response.as_ref().map(|response| {
            response
                .secrets
                .iter()
                .filter(|secret| secret.namespace == namespace && visible(secret, self.tab))
                .collect::<Vec<_>>()
        });
        html! {<section class="secrets-namespace" aria-label={format!("{title} secrets")}>
            <header><h3>{title}</h3><p>{description}</p></header>
            <div class="secrets-table-scroll"><table class="secrets-table" aria-label={format!("{title} secrets")}>
                <thead><tr><th scope="col">{"Name"}</th><th scope="col">{"Value"}</th><th scope="col">{"Version"}</th><th scope="col">{"Owner"}</th><th scope="col">{"Updated"}</th></tr></thead>
                <tbody>{match rows {
                    None => html!{<tr><td colspan="5">{if self.error.is_some(){"Secrets could not be loaded."}else{"Loading secrets…"}}</td></tr>},
                    Some(rows) if rows.is_empty() => html!{<tr><td colspan="5" class="secrets-empty">{format!("No {}{} secrets.",if self.tab==Tab::Deleted{"deleted "}else{""},title.to_lowercase())}</td></tr>},
                    Some(rows) => html!{<>{for rows.into_iter().map(|secret| {
                        let id=secret.id;
                        let owner=self.members.iter().find(|member|member.user_id==secret.owner_user_id)
                            .map(compact_member_label).unwrap_or_else(||secret.owner_user_id.to_string());
                        metadata_row(secret, &owner, self.selected==Some(id), &utils::format_timestamp(&secret.updated_at), ctx.link().callback(move |_|Msg::Select(id)))
                    })}</>},
                }}</tbody>
            </table></div>
        </section>}
    }
    fn detail(&self, ctx: &Context<Self>) -> Html {
        let Some(secret) = self.current() else {
            return html! {};
        };
        html! {<section class="secrets-detail" aria-label="Secret details"><h2>{&secret.name}</h2><span class="secrets-configured">{if self.tab==Tab::Deleted {"Deleted / unavailable"}else{"Configured"}}</span><p>{&secret.description}</p>
            <dl><dt>{"Audience"}</dt><dd>{audience(secret)}</dd><dt>{"Owner"}</dt><dd>{self.member_label(secret.owner_user_id)}</dd><dt>{"Updated"}</dt><dd>{utils::format_timestamp(&secret.updated_at)}</dd><dt>{"Reference"}</dt><dd><code>{format!("id/{}",secret.id)}</code></dd></dl>
            if deleted(secret) {
                <p>{format!("Deleted {} by {}",secret.deleted_at.as_deref().map(utils::format_timestamp).unwrap_or_else(||"at an unknown time".into()),secret.deleted_by.map(|id|id.to_string()).unwrap_or_else(||"an unavailable user".into()))}</p>
                if !secret.grants.is_empty() { <ul aria-label="Retained individual access">{for secret.grants.iter().map(|grant|html!{<li key={grant.user_id.to_string()}>{self.member_label(grant.user_id)}{format!(" — {}",grant.rights.iter().map(right_label).collect::<Vec<_>>().join(", "))}</li>})}</ul> }
                if secret.can_manage {
                    <p>{"Restoring keeps all versions and the reserved name. Previous execution tickets remain invalid."}</p>
                    <label class="secrets-check"><input type="checkbox" checked={self.confirm_restore} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ConfirmRestore(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{format!("Restore access for: {}. I confirm this audience.",audience(secret))}</label>
                    <button disabled={self.busy||!self.confirm_restore} onclick={ctx.link().callback(|_|Msg::Restore)}>{"Restore secret"}</button>
                }
            } else {
                if secret.can_write {<button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Replace)}>{"Replace value"}</button>}
                if secret.can_manage {
                    if secret.namespace==SecretNamespace::Personal {
                    <h3>{"Individual access"}</h3><p>{"Sharing grants read and use. Recipients cannot re-share through Portal. Revocation cannot recall a value already copied or delivered to a process."}</p>
                    <ul>{for secret.grants.iter().map(|grant|{let user=grant.user_id;html!{<li key={user.to_string()}>{self.member_label(user)}{format!(" — {}",grant.rights.iter().map(right_label).collect::<Vec<_>>().join(", "))}<button disabled={self.busy} onclick={ctx.link().callback(move |_|Msg::Unshare(user))}>{"Revoke"}</button></li>}})}</ul>
                    if self.directory_restricted {
                        <p>{"Member discovery requires a domain-locked Portal and a verified identity in that domain. Sharing by a known Portal user UUID remains available through the CLI."}</p>
                    } else {
                    <label>{"Existing active member"}<select value={self.share_user.map(|id|id.to_string()).unwrap_or_default()} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ShareUser(event.target_unchecked_into::<web_sys::HtmlSelectElement>().value()))}><option value="">{"Choose a member"}</option>{for self.members.iter().filter(|member|member.user_id!=secret.owner_user_id).map(|member|html!{<option value={member.user_id.to_string()}>{self.member_label(member.user_id)}</option>})}</select></label>
                    <button disabled={self.busy||self.share_user.is_none()} onclick={ctx.link().callback(|_|Msg::Share)}>{"Grant read and use"}</button>}
                    }
                    <h3>{"Delete"}</h3><p>{"Delete adds the deleted tag, disables reads and use, and keeps all encrypted versions. It does not clean up leaked copies. The name stays reserved."}</p>
                    <label class="secrets-check"><input type="checkbox" checked={self.confirm_delete} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ConfirmDelete(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"I understand deletion is reversible and retains the vault history."}</label><button disabled={self.busy||!self.confirm_delete} onclick={ctx.link().callback(|_|Msg::Delete)}>{"Delete secret"}</button>
                }
            }
            <h3>{"Retained versions"}</h3><p>{"All versions remain encrypted. This page shows metadata only, including for deleted secrets."}</p>
            {match &self.versions{None=>html!{<p>{"Loading version history…"}</p>},Some(history)=>html!{<ol class="secrets-versions">{for history.versions.iter().map(|version|html!{<li key={version.version}><strong>{format!("Version {}{}",version.version,if version.current{" · current"}else{""})}</strong><span class="secrets-configured">{"Configured"}</span><small>{format!("{} · {}",utils::format_timestamp(&version.created_at),version.created_by)}</small></li>})}</ol>}}}
            if secret.can_manage {<h3>{"Clean up leaked copies"}</h3><p>{"Scan all retained versions for copies in your own Portal records and archives. Organization-wide cleanup requires an administrator. This preserves the encrypted vault and does not revoke the provider credential. Backups, external copies and uninspected assets may remain; inspect the cleanup receipt."}</p>
                if self.response.as_ref().is_some_and(|response|response.can_manage_organization) {<label class="secrets-check"><input type="checkbox" checked={self.global_cleanup} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::GlobalCleanup(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"Include other members' Portal records and archives. I authorize organization-wide cleanup as an administrator."}</label>}
                <button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Expunge)}>{"Start cleanup for all versions"}</button>}
        </section>}
    }
    fn credentials_view(&self, ctx: &Context<Self>) -> Html {
        html! {<section class="secrets-job" aria-label="Approved CLI access"><h3>{"Approved CLI access"}</h3><p>{"Computers authorized to access secrets within your grants."}</p><details><summary>{"About approval and revocation"}</summary><p>{"Revoking access stops future requests, but cannot recall values already delivered. Verify your account before approving or revoking CLI access."}</p></details><button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::RefreshCredentials)}>{"Refresh CLI access"}</button>
            {match &self.credentials {None=>html!{<p>{"Loading approved CLI access…"}</p>},Some(credentials) if credentials.is_empty()=>html!{<p>{"No approved CLI credentials."}</p>},Some(credentials)=>html!{<ul class="secrets-credentials">{for credentials.iter().map(|credential|{let id=credential.id;html!{<li key={id.to_string()}><strong>{&credential.label}</strong><small>{format!("Created {} · Expires {}",utils::format_timestamp(&credential.created_at),utils::format_timestamp(&credential.expires_at))}</small>if let Some(time)=&credential.revoked_at {<span>{format!("Revoked {}",utils::format_timestamp(time))}</span>}else{<button disabled={self.busy} onclick={ctx.link().callback(move |_|Msg::RevokeCredential(id))}>{"Revoke CLI access"}</button>}</li>}})}</ul>}}}
        </section>}
    }
    fn member_label(&self, id: Uuid) -> String {
        self.members
            .iter()
            .find(|member| member.user_id == id)
            .map(member_label)
            .unwrap_or_else(|| id.to_string())
    }
}
fn member_label(member: &SecretMember) -> String {
    let providers: Vec<_> = member
        .identities
        .iter()
        .map(|identity| {
            let name = identity
                .handle
                .as_deref()
                .or(identity.email.as_deref())
                .unwrap_or(&identity.subject);
            let unverified =
                identity.handle.is_none() && identity.email.is_some() && !identity.email_verified;
            format!(
                "{}:{}{}",
                identity.provider,
                name,
                if unverified {
                    " (unverified email)"
                } else {
                    ""
                }
            )
        })
        .collect();
    format!(
        "{} · {} · {}",
        member.display_name,
        if providers.is_empty() {
            "Portal account".into()
        } else {
            providers.join("; ")
        },
        member.user_id
    )
}
fn right_label(right: &SecretRight) -> &'static str {
    match right {
        SecretRight::Read => "read",
        SecretRight::Use => "use",
        SecretRight::Write => "replace",
        SecretRight::Manage => "manage access",
    }
}
fn job_label(state: SecretExpungeState) -> &'static str {
    match state {
        SecretExpungeState::Queued => "Queued",
        SecretExpungeState::Discovering => "Scanning for copies",
        SecretExpungeState::Redacting => "Redacting records",
        SecretExpungeState::DeletingVersions => "Removing old archive copies",
        SecretExpungeState::Verifying => "Verifying cleanup",
        SecretExpungeState::CompleteActiveStorage => "Active-storage cleanup complete",
        SecretExpungeState::AwaitingBackupExpiry => "Waiting for backup expiry",
        SecretExpungeState::Blocked => "Partially complete — blocked",
        SecretExpungeState::Failed => "Cleanup failed",
    }
}
fn render_job(job: &SecretExpungeJob, refresh: Callback<MouseEvent>) -> Html {
    html! {<section class="secrets-job" aria-label="Cleanup receipt"><h2>{"Cleanup receipt"}</h2><p role="status">{job_label(job.state)}</p><p>{format!("{} records redacted · {} archive objects rewritten · {} bytes scanned · {} blocked targets",job.records_redacted,job.objects_rewritten,job.bytes_scanned,job.blocked_targets)}</p><p>{"Encrypted vault versions are retained. This receipt does not promise erasure from backups, external downloads, native agent history or provider logs."}</p><ul>{for job.limitations.iter().map(|limitation|html!{<li>{limitation}</li>})}</ul><Link<Route> to={Route::SecretJob{id:job.id.to_string()}}>{"Open this receipt"}</Link<Route>><button onclick={refresh}>{"Refresh status"}</button></section>}
}

fn restore_request(secret: &SecretMetadata, confirmed: bool) -> Option<SecretRestoreRequest> {
    (confirmed && deleted(secret) && secret.can_manage).then_some(SecretRestoreRequest {
        expected_generation: secret.generation,
        confirm_sharing: true,
    })
}
fn metadata_row(
    secret: &SecretMetadata,
    owner: &str,
    selected: bool,
    updated: &str,
    onclick: Callback<MouseEvent>,
) -> Html {
    html! {<tr key={secret.id.to_string()} class={classes!(selected.then_some("is-selected"))}>
        <th scope="row"><button type="button" class="secret-name" aria-expanded={selected.to_string()} {onclick}>{&secret.name}</button>
            if !secret.description.is_empty(){<small class="secrets-description">{&secret.description}</small>}
        </th>
        <td><span class="secrets-configured">{if secret.tags.iter().any(|tag|tag=="deleted"){"Deleted / unavailable"}else{"Configured"}}</span></td>
        <td>{secret.current_version}</td><td class="secrets-owner">{owner}</td>
        <td><time datetime={secret.updated_at.clone()}>{updated}</time></td>
    </tr>}
}
fn compact_member_label(member: &SecretMember) -> String {
    for identity in &member.identities {
        if identity.provider == "github" {
            if let Some(handle) = &identity.handle {
                return format!("{handle}@github");
            }
        }
        if identity.email_verified {
            if let Some(email) = &identity.email {
                return email.clone();
            }
        }
    }
    member.display_name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yew::virtual_dom::VNode;
    fn fixture() -> SecretMetadata {
        SecretMetadata {
            id: Uuid::nil(),
            owner_user_id: Uuid::nil(),
            namespace: SecretNamespace::Personal,
            name: "DEPLOY_TOKEN".into(),
            description: "Deployment access".into(),
            tags: vec![],
            current_version: 3,
            generation: 7,
            created_at: "2026-11-10T00:00:00Z".into(),
            updated_at: "2026-11-10T00:00:00Z".into(),
            deleted_at: None,
            deleted_by: None,
            can_read: true,
            can_use: true,
            can_write: true,
            can_manage: true,
            grants: vec![],
        }
    }
    fn text(node: &VNode) -> String {
        match node {
            VNode::VText(text) => text.text.to_string(),
            VNode::VTag(tag) => {
                assert!(
                    !["input", "textarea"].contains(&tag.tag()),
                    "saved metadata must never contain a value field"
                );
                tag.children().map(text).unwrap_or_default()
            }
            VNode::VList(list) => list.iter().map(text).collect(),
            _ => String::new(),
        }
    }
    #[test]
    fn sharing_labels_preserve_provider_identity_and_stable_portal_user_id() {
        let id = Uuid::new_v4();
        let member = SecretMember {
            user_id: id,
            display_name: "Alice".into(),
            email: "alice@example.test".into(),
            identities: vec![
                shared::api::SecretIdentityMetadata {
                    provider: "github".into(),
                    subject: "123".into(),
                    handle: Some("alice".into()),
                    email: None,
                    email_verified: false,
                },
                shared::api::SecretIdentityMetadata {
                    provider: "google".into(),
                    subject: "456".into(),
                    handle: None,
                    email: Some("alice@example.test".into()),
                    email_verified: true,
                },
            ],
        };
        let label = super::member_label(&member);
        assert!(label.contains("github:alice"));
        assert!(label.contains("google:alice@example.test"));
        assert!(label.contains(&id.to_string()));
    }
    #[test]
    fn active_and_deleted_saved_values_render_fixed_status_indicators() {
        let mut secret = fixture();
        let active = text(&metadata_row(
            &secret,
            "Owner",
            false,
            "2026-11-10",
            Callback::noop(),
        ));
        secret.tags.push("deleted".into());
        let removed = text(&metadata_row(
            &secret,
            "Owner",
            false,
            "2026-11-10",
            Callback::noop(),
        ));
        assert!(removed.contains("Deleted / unavailable"));
        assert!(!removed.contains("Configured"));
        assert_eq!(active.matches("Configured").count(), 1);
        assert!(!active.contains("Reveal"));
        assert!(!active.contains("Copy"));
        // Even an unexpected legacy payload is ignored by the metadata type.
        let mut wire = serde_json::to_value(&secret).unwrap();
        wire["value"] = "NEVER_RENDER_THIS".into();
        wire["value_length"] = 12345.into();
        let parsed: SecretMetadata = serde_json::from_value(wire).unwrap();
        let rendered = text(&metadata_row(
            &parsed,
            "Owner",
            false,
            "2026-11-10",
            Callback::noop(),
        ));
        assert!(!rendered.contains("NEVER_RENDER_THIS"));
        assert!(!rendered.contains("12345"));
    }
    #[test]
    fn exact_deleted_tag_is_excluded_from_every_active_view() {
        let mut secret = fixture();
        secret.tags.push("deleted".into());
        assert!(!visible(&secret, Tab::Active));
        assert!(visible(&secret, Tab::Deleted));
        secret.tags = vec!["not-deleted".into()];
        assert!(!visible(&secret, Tab::Deleted));
    }
    #[test]
    fn restore_requires_explicit_audience_confirmation_and_current_generation() {
        let mut secret = fixture();
        secret.namespace = SecretNamespace::Organization;
        assert!(restore_request(&secret, true).is_none());
        secret.tags.push("deleted".into());
        assert!(restore_request(&secret, false).is_none());
        assert!(audience(&secret).contains("future members"));
        let request = restore_request(&secret, true).unwrap();
        assert_eq!(request.expected_generation, 7);
        assert!(request.confirm_sharing);
        secret.can_manage = false;
        assert!(restore_request(&secret, true).is_none());
    }
    #[test]
    fn cleanup_receipt_reports_limits_without_claiming_global_erasure() {
        let job = SecretExpungeJob {
            id: Uuid::nil(),
            secret_id: Uuid::nil(),
            state: SecretExpungeState::Blocked,
            records_redacted: 2,
            objects_rewritten: 1,
            bytes_scanned: 100,
            blocked_targets: 1,
            limitations: vec!["Old backups remain".into()],
            created_at: String::new(),
            updated_at: String::new(),
        };
        // Render the receipt's immediate text (router link remains a component).
        let rendered = text(&render_job(&job, Callback::noop()));
        assert!(rendered.contains("blocked"));
        assert!(rendered.contains("Old backups remain"));
        assert!(rendered.contains("Encrypted vault versions are retained"));
        assert!(rendered.contains("external downloads"));
    }
}
