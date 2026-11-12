//! Vault management. Saved plaintext never enters page state or rendered markup;
//! the explicit verified copy action sends it directly to the clipboard.
mod api;
mod copy;
mod value_form;
use crate::{
    utils::{self, On401},
    Route,
};
use shared::api::{
    SecretCreateRequest, SecretExpungeJob, SecretExpungeState, SecretGrantRequest,
    SecretListResponse, SecretMember, SecretMembersResponse, SecretMetadata, SecretMutationRequest,
    SecretNamespace, SecretRestoreRequest, SecretRight, SecretValue, SecretVersionsResponse,
    SecretWriteRequest,
};
use uuid::Uuid;
use value_form::{FreshValue, ValueForm};
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_router::prelude::*;

fn deleted(secret: &SecretMetadata) -> bool {
    secret.deleted_at.is_some() || secret.tags.iter().any(|tag| tag == "deleted")
}
fn visible(secret: &SecretMetadata, show_deleted: bool) -> bool {
    show_deleted || !deleted(secret)
}
fn sorted_secrets(response: &SecretListResponse, show_deleted: bool) -> Vec<&SecretMetadata> {
    let mut rows: Vec<_> = response
        .secrets
        .iter()
        .filter(|s| visible(s, show_deleted))
        .collect();
    rows.sort_by_cached_key(|s| (s.name.to_ascii_lowercase(), s.name.clone(), s.id));
    rows
}
async fn load_secrets(show_deleted: bool) -> Result<SecretListResponse, String> {
    let fetch = |deleted| async move {
        utils::fetch_json::<SecretListResponse>(
            &format!("/api/secrets?deleted={deleted}"),
            On401::Ignore,
        )
        .await
        .map_err(|_| "Could not load secrets. Check your login and retry.".to_string())
    };
    if show_deleted {
        let (active, removed) = futures_util::future::join(fetch(false), fetch(true)).await;
        let mut active = active?;
        let removed = removed?;
        // A deletion between the two reads must not produce duplicate keyed rows.
        active.secrets.retain(|secret| {
            !removed
                .secrets
                .iter()
                .any(|deleted| deleted.id == secret.id)
        });
        active.secrets.extend(removed.secrets);
        Ok(active)
    } else {
        fetch(false).await
    }
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
    CheckAccess,
    AccessChecked(Result<(), &'static str>),
    ShowDeleted(bool),
    Loaded(u64, Result<SecretListResponse, String>),
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
    Job(Result<SecretExpungeJob, String>),
    RefreshJob,
    Members(Result<SecretMembersResponse, utils::FetchError>),
}
pub struct SecretsPage {
    verified: bool,
    show_deleted: bool,
    checking_access: bool,
    list_loading: bool,
    load_generation: u64,
    response: Option<SecretListResponse>,
    selected: Option<Uuid>,
    versions: Option<SecretVersionsResponse>,
    editing: Option<bool>,
    busy: bool,
    error: Option<String>,
    confirm_delete: bool,
    confirm_restore: bool,
    members: Vec<SecretMember>,
    directory_restricted: bool,
    share_user: Option<Uuid>,
    job: Option<SecretExpungeJob>,
}
impl SecretsPage {
    fn current(&self) -> Option<&SecretMetadata> {
        self.response
            .as_ref()?
            .secrets
            .iter()
            .find(|s| Some(s.id) == self.selected && visible(s, self.show_deleted))
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
    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            verified: false,
            show_deleted: false,
            checking_access: false,
            list_loading: false,
            load_generation: 0,
            response: None,
            selected: None,
            versions: None,
            editing: None,
            busy: false,
            error: None,
            confirm_delete: false,
            confirm_restore: false,
            members: vec![],
            directory_restricted: false,
            share_user: None,
            job: None,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if ctx.props().job_id != old.job_id {
            self.job = None;
            if self.verified {
                ctx.link().send_message(Msg::RefreshJob);
            }
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        if !self.verified && !matches!(msg, Msg::CheckAccess | Msg::AccessChecked(_)) {
            return false;
        }
        match msg {
            Msg::CheckAccess => {
                if self.checking_access || self.verified {
                    return false;
                }
                self.checking_access = true;
                self.error = None;
                ctx.link().send_future(async {
                    Msg::AccessChecked(
                        crate::components::secret_verification::verify_session().await,
                    )
                });
                true
            }
            Msg::AccessChecked(result) => {
                self.checking_access = false;
                match result {
                    Err(error) => self.error = Some(error.into()),
                    Ok(()) => {
                        self.verified = true;
                        self.error = None;
                        ctx.link().send_message(Msg::Load);
                        if ctx.props().job_id.is_some() {
                            ctx.link().send_message(Msg::RefreshJob);
                        }
                        let link = ctx.link().clone();
                        spawn_local(async move {
                            link.send_message(Msg::Members(
                                utils::fetch_json::<SecretMembersResponse>(
                                    "/api/secrets/members",
                                    On401::Ignore,
                                )
                                .await,
                            ));
                        });
                    }
                }
                true
            }
            Msg::Load => {
                self.confirm_restore = false;

                self.confirm_delete = false;
                self.list_loading = true;
                self.load_generation += 1;
                let generation = self.load_generation;
                let show_deleted = self.show_deleted;
                ctx.link().send_future(async move {
                    Msg::Loaded(generation, load_secrets(show_deleted).await)
                });
                true
            }
            Msg::ShowDeleted(show) => {
                self.show_deleted = show;
                if self.current().is_none() {
                    self.selected = None;
                    self.editing = None;
                    self.versions = None;
                }
                ctx.link().send_message(Msg::Load);
                true
            }
            Msg::Loaded(generation, result) => {
                if generation != self.load_generation {
                    return false;
                }
                self.list_loading = false;
                self.confirm_restore = false;

                self.confirm_delete = false;
                match result {
                    Ok(data) => {
                        self.response = Some(data);
                        self.error = None;
                        if self.current().is_none() {
                            self.selected = None;
                            self.versions = None;
                            if self.editing == Some(true) {
                                self.editing = None;
                            }
                        }
                    }
                    Err(error) => self.error = Some(error),
                }
                true
            }
            Msg::Select(id) => {
                if self.busy {
                    return false;
                }
                if self.selected == Some(id) {
                    self.selected = None;
                    self.versions = None;
                    self.editing = None;
                    return true;
                }
                self.selected = Some(id);
                self.versions = None;
                self.editing = None;
                self.confirm_delete = false;
                self.confirm_restore = false;

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
        html! {<section class={classes!("secrets-page",ctx.props().embedded.then_some("secrets-embedded"))} aria-label="Secrets management">
            <header class="secrets-heading"><div>
                if ctx.props().embedded {<h2>{"Secrets"}</h2>} else {<h1>{"Secrets"}</h1>}
                <p>{if self.verified {"Select a secret to view its details and actions."} else {"Check your Portal access to view secrets."}}</p>
            </div>
            if !ctx.props().embedded {<Link<Route> to={Route::Dashboard}>{"Back to sessions"}</Link<Route>>}
            if self.verified {<div class="secrets-toolbar">
                <button type="button" class="secrets-add" disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Create)}>{"Add secret"}</button>
                <button type="button" disabled={self.busy || self.list_loading} onclick={ctx.link().callback(|_|Msg::Load)}>{"Refresh"}</button>
            </div>}
            </header>
            if let Some(error)=&self.error {<p role="alert" class="secrets-error">{error}</p>}
            if !self.verified {
                <div class="secrets-access-gate">
                    <button type="button" class="secret-button secret-button--primary" disabled={self.checking_access} onclick={ctx.link().callback(|_|Msg::CheckAccess)}>{"Check Portal access"}</button>
                    if self.checking_access {<p role="status"><span class="secret-verification-spinner" aria-hidden="true"/>{"Checking Portal access…"}</p>}
                </div>
            } else {
                <label class="secrets-check secrets-filter"><input type="checkbox" checked={self.show_deleted} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ShowDeleted(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"Show deleted secrets"}</label>
                if self.editing == Some(false) {<ValueForm replacing={false} can_publish={can_publish} on_submit={ctx.link().callback(Msg::Save)} on_cancel={ctx.link().callback(|_|Msg::Cancel)} />}
                {self.inventory(ctx)}
                if let Some(job)=&self.job {{render_job(job,ctx.link().callback(|_|Msg::RefreshJob))}}
            }
        </section>}
    }
}

impl SecretsPage {
    fn inventory(&self, ctx: &Context<Self>) -> Html {
        let rows = self
            .response
            .as_ref()
            .map(|r| sorted_secrets(r, self.show_deleted));
        html! {<div class="secrets-table-scroll"><table class="secrets-table" aria-label="Secrets">
            <thead><tr><th scope="col">{"Name"}</th><th scope="col">{"Scope"}</th><th scope="col">{"Version"}</th><th scope="col" class="secrets-owner">{"Owner"}</th><th scope="col" class="secrets-updated">{"Updated"}</th></tr></thead>
            <tbody>{match rows {
                None => html!{<tr><td colspan="5">{if self.error.is_some(){"Secrets could not be loaded."}else{"Loading secrets…"}}</td></tr>},
                Some(rows) if rows.is_empty() => html!{<tr><td colspan="5" class="secrets-empty">{"No secrets to show."}</td></tr>},
                Some(rows) => html!{<>{for rows.into_iter().map(|secret| {
                    let id = secret.id;
                    let owner = self.members.iter().find(|member|member.user_id==secret.owner_user_id).map(compact_member_label).unwrap_or_else(||"Unknown member".into());
                    html!{<key={id.to_string()}>
                        {metadata_row(secret,&owner,self.selected==Some(id),&utils::format_timestamp(&secret.updated_at),ctx.link().callback(move |_|Msg::Select(id)))}
                        if self.selected == Some(id) {<tr class="secret-expanded-row"><td colspan="5">{self.detail(ctx)}</td></tr>}
                    </>}
                })}</>},
            }}</tbody>
        </table></div>}
    }
    fn detail(&self, ctx: &Context<Self>) -> Html {
        let Some(secret) = self.current() else {
            return html! {};
        };
        html! {<section class="secrets-detail" id={format!("secret-details-{}",secret.id)} aria-label={format!("Details for {}",secret.name)}>
            if !deleted(secret) {
                <div class="secret-detail-actions">
                    if secret.can_read {<copy::SecretCopy key={format!("{}-{}",secret.id,secret.current_version)} id={secret.id} version={secret.current_version} verified={self.verified} disabled={self.busy} />}
                    if secret.can_write {<button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Replace)}>{"Replace value"}</button>}
                </div>
                if self.editing == Some(true) {<ValueForm key={secret.id.to_string()} replacing={true} can_publish={false} on_submit={ctx.link().callback(Msg::Save)} on_cancel={ctx.link().callback(|_|Msg::Cancel)} />}
            }
            if !secret.description.is_empty() {<p>{&secret.description}</p>}
            <dl><dt>{"Audience"}</dt><dd>{audience(secret)}</dd><dt>{"Owner"}</dt><dd>{self.members.iter().find(|member|member.user_id==secret.owner_user_id).map(compact_member_label).unwrap_or_else(||"Unknown member".into())}</dd><dt>{"Updated"}</dt><dd>{utils::format_timestamp(&secret.updated_at)}</dd><dt>{"Reference"}</dt><dd><code>{format!("id/{}",secret.id)}</code></dd></dl>
            if deleted(secret) {
                <p>{format!("Deleted {} by {}",secret.deleted_at.as_deref().map(utils::format_timestamp).unwrap_or_else(||"at an unknown time".into()),secret.deleted_by.map(|id|id.to_string()).unwrap_or_else(||"an unavailable user".into()))}</p>
                if !secret.grants.is_empty() { <ul aria-label="Retained individual access">{for secret.grants.iter().map(|grant|html!{<li key={grant.user_id.to_string()}>{self.member_label(grant.user_id)}{format!(" — {}",grant.rights.iter().map(right_label).collect::<Vec<_>>().join(", "))}</li>})}</ul> }
                if secret.can_manage {
                    <p>{"Restoring keeps all versions and the reserved name. Previous execution tickets remain invalid."}</p>
                    <label class="secrets-check"><input type="checkbox" checked={self.confirm_restore} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ConfirmRestore(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{format!("Restore access for: {}. I confirm this audience.",audience(secret))}</label>
                    <button disabled={self.busy||!self.confirm_restore} onclick={ctx.link().callback(|_|Msg::Restore)}>{"Restore secret"}</button>
                }
            } else {
                if secret.can_manage {
                    if secret.namespace==SecretNamespace::Personal {
                    <h3>{"Individual access"}</h3><p>{"Sharing grants read and use. Recipients cannot re-share through Portal. Revocation cannot recall a value already copied or delivered to a process."}</p>
                    <ul>{for secret.grants.iter().map(|grant|{let user=grant.user_id;html!{<li key={user.to_string()}>{self.member_label(user)}{format!(" — {}",grant.rights.iter().map(right_label).collect::<Vec<_>>().join(", "))}<button disabled={self.busy} onclick={ctx.link().callback(move |_|Msg::Unshare(user))}>{"Revoke"}</button></li>}})}</ul>
                    if self.directory_restricted {
                        <p>{"Member discovery requires a domain-locked Portal and a verified identity in that domain."}</p>
                    } else {
                    <label>{"Existing active member"}<select value={self.share_user.map(|id|id.to_string()).unwrap_or_default()} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ShareUser(event.target_unchecked_into::<web_sys::HtmlSelectElement>().value()))}><option value="">{"Choose a member"}</option>{for self.members.iter().filter(|member|member.user_id!=secret.owner_user_id).map(|member|html!{<option value={member.user_id.to_string()}>{self.member_label(member.user_id)}</option>})}</select></label>
                    <button disabled={self.busy||self.share_user.is_none()} onclick={ctx.link().callback(|_|Msg::Share)}>{"Grant read and use"}</button>}
                    }
                    <h3>{"Delete"}</h3><p>{"Delete adds the deleted tag, disables reads and use, and keeps all encrypted versions. It does not clean up leaked copies. The name stays reserved."}</p>
                    <label class="secrets-check"><input type="checkbox" checked={self.confirm_delete} disabled={self.busy} onchange={ctx.link().callback(|event:Event|Msg::ConfirmDelete(event.target_unchecked_into::<HtmlInputElement>().checked()))}/>{"I understand deletion is reversible and retains the vault history."}</label><button disabled={self.busy||!self.confirm_delete} onclick={ctx.link().callback(|_|Msg::Delete)}>{"Delete secret"}</button>
                }
            }
            <h3>{"Retained versions"}</h3><p>{"All versions remain encrypted. This page shows metadata only, including for deleted secrets."}</p>
            {match &self.versions{None=>html!{<p>{"Loading version history…"}</p>},Some(history)=>html!{<ol class="secrets-versions">{for history.versions.iter().map(|version|html!{<li key={version.version}><strong>{format!("Version {}{}",version.version,if version.current{" · current"}else{""})}</strong><small>{format!("{} · {}",utils::format_timestamp(&version.created_at),version.created_by)}</small></li>})}</ol>}}}
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
        <th scope="row"><button type="button" class="secret-name" aria-expanded={selected.to_string()} aria-controls={format!("secret-details-{}",secret.id)} {onclick}><span class="secret-disclosure" aria-hidden="true">{if selected {"▾"} else {"▸"}}</span>{&secret.name}</button>
            if deleted(secret) {<span class="secret-deleted-badge">{"Deleted"}</span>}
            if !secret.description.is_empty(){<small class="secrets-description">{&secret.description}</small>}
        </th>
        <td>{if secret.namespace==SecretNamespace::Personal {"Personal"} else {"Organization"}}</td>
        <td>{secret.current_version}</td><td class="secrets-owner">{owner}</td>
        <td class="secrets-updated"><time datetime={secret.updated_at.clone()}>{updated}</time></td>
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
    fn inventory_sorts_names_across_scopes_and_optionally_includes_deleted() {
        let mut z = fixture();
        z.name = "Zulu".into();
        z.id = Uuid::new_v4();
        let mut a = fixture();
        a.name = "alpha".into();
        a.id = Uuid::new_v4();
        a.namespace = SecretNamespace::Organization;
        let mut b = fixture();
        b.name = "Bravo".into();
        b.id = Uuid::new_v4();
        b.deleted_at = Some("2026-11-12T00:00:00Z".into());
        let response = SecretListResponse {
            secrets: vec![z, b, a],
            can_manage_organization: false,
        };
        assert_eq!(
            sorted_secrets(&response, false)
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "Zulu"]
        );
        assert_eq!(
            sorted_secrets(&response, true)
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "Bravo", "Zulu"]
        );
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
    fn summary_rows_show_scope_and_deletion_without_fake_value_indicators() {
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
        assert!(removed.contains("Deleted"));
        assert!(!removed.contains("Configured"));
        assert!(!active.contains("Configured"));
        assert!(active.contains("Personal"));
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
        assert!(!visible(&secret, false));
        assert!(visible(&secret, true));
        secret.tags = vec!["not-deleted".into()];
        assert!(visible(&secret, true));
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
