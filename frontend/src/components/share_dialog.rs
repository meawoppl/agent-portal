use gloo::events::EventListener;
use gloo_net::http::Request;
use shared::api::{AddMemberRequest, UpdateMemberRoleRequest};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::utils;

/// Member info returned from API
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct MemberInfo {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct MembersResponse {
    members: Vec<MemberInfo>,
}

#[derive(Properties, PartialEq)]
pub struct ShareDialogProps {
    pub session_id: Uuid,
    pub on_close: Callback<()>,
}

pub enum ShareDialogMsg {
    LoadMembers,
    MembersLoaded(Vec<MemberInfo>),
    UpdateEmail(String),
    UpdateRole(String),
    AddMember,
    MemberAdded,
    RemoveMember(Uuid),
    MemberRemoved(Uuid),
    ChangeRole(Uuid, String),
    RoleChanged(Uuid, String),
    SetError(String),
}

pub struct ShareDialog {
    members: Vec<MemberInfo>,
    loading: bool,
    email_input: String,
    new_role: String,
    error: Option<String>,
    #[allow(dead_code)] // RAII guard — must be held to keep listener active
    _escape_listener: Option<EventListener>,
}

impl Component for ShareDialog {
    type Message = ShareDialogMsg;
    type Properties = ShareDialogProps;

    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(ShareDialogMsg::LoadMembers);
        let on_close = ctx.props().on_close.clone();
        let listener = EventListener::new(&gloo::utils::document(), "keydown", move |event| {
            let e: &web_sys::KeyboardEvent = event.unchecked_ref();
            if e.key() == "Escape" {
                on_close.emit(());
            }
        });
        Self {
            members: Vec::new(),
            loading: true,
            email_input: String::new(),
            new_role: "viewer".to_string(),
            error: None,
            _escape_listener: Some(listener),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            ShareDialogMsg::LoadMembers => {
                self.loading = true;
                let session_id = ctx.props().session_id;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let url = utils::api_url(&format!("/api/sessions/{}/members", session_id));
                    match Request::get(&url).send().await {
                        Ok(response) if response.ok() => {
                            if let Ok(data) = response.json::<MembersResponse>().await {
                                link.send_message(ShareDialogMsg::MembersLoaded(data.members));
                            }
                        }
                        Ok(response) => {
                            log::error!("Failed to load members: {}", response.status());
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to load members".to_string(),
                            ));
                        }
                        Err(e) => {
                            log::error!("Failed to load members: {:?}", e);
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to load members".to_string(),
                            ));
                        }
                    }
                });
                true
            }
            ShareDialogMsg::MembersLoaded(members) => {
                self.members = members;
                self.loading = false;
                true
            }
            ShareDialogMsg::UpdateEmail(email) => {
                self.email_input = email;
                true
            }
            ShareDialogMsg::UpdateRole(role) => {
                self.new_role = role;
                true
            }
            ShareDialogMsg::AddMember => {
                if self.email_input.trim().is_empty() {
                    return false;
                }
                let session_id = ctx.props().session_id;
                let email = self.email_input.trim().to_string();
                let role = self.new_role.clone();
                let link = ctx.link().clone();

                spawn_local(async move {
                    let url = utils::api_url(&format!("/api/sessions/{}/members", session_id));
                    let body = AddMemberRequest { email, role };
                    match Request::post(&url).json(&body).unwrap().send().await {
                        Ok(response) if response.status() == 201 => {
                            link.send_message(ShareDialogMsg::MemberAdded);
                        }
                        Ok(response) if response.status() == 404 => {
                            link.send_message(ShareDialogMsg::SetError(
                                "User not found".to_string(),
                            ));
                        }
                        Ok(response) if response.status() == 409 => {
                            link.send_message(ShareDialogMsg::SetError(
                                "User is already a member".to_string(),
                            ));
                        }
                        Ok(response) => {
                            log::error!("Failed to add member: {}", response.status());
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to add member".to_string(),
                            ));
                        }
                        Err(e) => {
                            log::error!("Failed to add member: {:?}", e);
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to add member".to_string(),
                            ));
                        }
                    }
                });
                true
            }
            ShareDialogMsg::MemberAdded => {
                self.email_input.clear();
                self.error = None;
                ctx.link().send_message(ShareDialogMsg::LoadMembers);
                true
            }
            ShareDialogMsg::RemoveMember(user_id) => {
                let session_id = ctx.props().session_id;
                let link = ctx.link().clone();
                spawn_local(async move {
                    let url = utils::api_url(&format!(
                        "/api/sessions/{}/members/{}",
                        session_id, user_id
                    ));
                    match Request::delete(&url).send().await {
                        Ok(response) if response.status() == 204 => {
                            link.send_message(ShareDialogMsg::MemberRemoved(user_id));
                        }
                        Ok(response) => {
                            log::error!("Failed to remove member: {}", response.status());
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to remove member".to_string(),
                            ));
                        }
                        Err(e) => {
                            log::error!("Failed to remove member: {:?}", e);
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to remove member".to_string(),
                            ));
                        }
                    }
                });
                true
            }
            ShareDialogMsg::MemberRemoved(user_id) => {
                self.members.retain(|m| m.user_id != user_id);
                self.error = None;
                true
            }
            ShareDialogMsg::ChangeRole(user_id, new_role) => {
                let session_id = ctx.props().session_id;
                let link = ctx.link().clone();
                let role = new_role.clone();
                spawn_local(async move {
                    let url = utils::api_url(&format!(
                        "/api/sessions/{}/members/{}",
                        session_id, user_id
                    ));
                    let body = UpdateMemberRoleRequest { role: role.clone() };
                    match Request::patch(&url).json(&body).unwrap().send().await {
                        Ok(response) if response.ok() => {
                            link.send_message(ShareDialogMsg::RoleChanged(user_id, role));
                        }
                        Ok(response) => {
                            log::error!("Failed to change role: {}", response.status());
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to change role".to_string(),
                            ));
                        }
                        Err(e) => {
                            log::error!("Failed to change role: {:?}", e);
                            link.send_message(ShareDialogMsg::SetError(
                                "Failed to change role".to_string(),
                            ));
                        }
                    }
                });
                true
            }
            ShareDialogMsg::RoleChanged(user_id, new_role) => {
                if let Some(member) = self.members.iter_mut().find(|m| m.user_id == user_id) {
                    member.role = new_role;
                }
                self.error = None;
                true
            }
            ShareDialogMsg::SetError(error) => {
                self.error = Some(error);
                self.loading = false;
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_close = ctx.props().on_close.clone();
        let on_overlay_click = {
            let on_close = on_close.clone();
            Callback::from(move |_| on_close.emit(()))
        };
        let on_dialog_click = Callback::from(|e: MouseEvent| {
            e.stop_propagation();
        });

        let on_email_input = ctx.link().callback(|e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            ShareDialogMsg::UpdateEmail(input.value())
        });

        let on_role_change = ctx.link().callback(|e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            ShareDialogMsg::UpdateRole(select.value())
        });

        let on_add_click = ctx.link().callback(|_| ShareDialogMsg::AddMember);

        let on_keypress = ctx.link().batch_callback(|e: KeyboardEvent| {
            if e.key() == "Enter" {
                Some(ShareDialogMsg::AddMember)
            } else {
                None
            }
        });

        html! {
            <div class="share-dialog-overlay" onclick={on_overlay_click}>
                <div class="share-dialog" onclick={on_dialog_click}>
                    <div class="share-dialog-header">
                        <h2>{ "Share Session" }</h2>
                        <button class="share-dialog-close" onclick={move |_| on_close.emit(())}>
                            { "×" }
                        </button>
                    </div>

                    {
                        if let Some(error) = &self.error {
                            html! {
                                <div class="share-dialog-error">
                                    { error }
                                </div>
                            }
                        } else {
                            html! {}
                        }
                    }

                    <div class="share-dialog-add">
                        <input
                            type="email"
                            placeholder="Email address"
                            value={self.email_input.clone()}
                            oninput={on_email_input}
                            onkeypress={on_keypress}
                        />
                        <select value={self.new_role.clone()} onchange={on_role_change}>
                            <option value="viewer">{ "Viewer" }</option>
                            <option value="editor">{ "Editor" }</option>
                        </select>
                        <button onclick={on_add_click}>{ "Add" }</button>
                    </div>

                    <div class="share-dialog-members">
                        {
                            if self.loading {
                                html! { <div class="share-dialog-loading">{ "Loading..." }</div> }
                            } else if self.members.is_empty() {
                                html! { <div class="share-dialog-empty">{ "No members yet" }</div> }
                            } else {
                                html! {
                                    <ul>
                                        { for self.members.iter().map(|member| self.view_member(ctx, member)) }
                                    </ul>
                                }
                            }
                        }
                    </div>
                </div>
            </div>
        }
    }
}

impl ShareDialog {
    fn view_member(&self, ctx: &Context<Self>, member: &MemberInfo) -> Html {
        let is_owner = member.role == "owner";
        let user_id = member.user_id;
        let display_name = member
            .name
            .as_ref()
            .map(|n| format!("{} ({})", n, member.email))
            .unwrap_or_else(|| member.email.clone());

        let on_remove = ctx
            .link()
            .callback(move |_| ShareDialogMsg::RemoveMember(user_id));

        let on_role_change = {
            let link = ctx.link().clone();
            Callback::from(move |e: Event| {
                let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
                link.send_message(ShareDialogMsg::ChangeRole(user_id, select.value()));
            })
        };

        html! {
            <li class="share-dialog-member">
                <span class="member-name">{ display_name }</span>
                {
                    if is_owner {
                        html! { <span class="member-role owner">{ "Owner" }</span> }
                    } else {
                        html! {
                            <>
                                <select class="member-role-select" value={member.role.clone()} onchange={on_role_change}>
                                    <option value="viewer" selected={member.role == "viewer"}>{ "Viewer" }</option>
                                    <option value="editor" selected={member.role == "editor"}>{ "Editor" }</option>
                                </select>
                                <button class="member-remove" onclick={on_remove} title="Remove member">
                                    { "×" }
                                </button>
                            </>
                        }
                    }
                }
            </li>
        }
    }
}
