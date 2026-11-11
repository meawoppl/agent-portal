//! Fresh entry only: this component has no saved-value prop or retrieval path.
//! Its keyed local draft keeps typing stable while page metadata refreshes.
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

pub struct FreshValue {
    pub name: String,
    pub description: String,
    pub organization: bool,
    pub value: String,
}

#[derive(Properties, PartialEq)]
pub struct ValueFormProps {
    pub replacing: bool,
    pub can_publish: bool,
    pub on_submit: Callback<FreshValue>,
    pub on_cancel: Callback<()>,
}

#[function_component(ValueForm)]
pub fn value_form(props: &ValueFormProps) -> Html {
    let name = use_state(String::new);
    let description = use_state(String::new);
    let organization = use_state(|| false);
    let value = use_node_ref();
    let confirmed = use_state(|| false);
    let error = use_state(|| None::<String>);
    let onsubmit = {
        let (name, description, organization, confirmed, value) = (
            name.clone(),
            description.clone(),
            organization.clone(),
            confirmed.clone(),
            value.clone(),
        );
        let submit = props.on_submit.clone();
        let replacing = props.replacing;
        let error = error.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            if *organization && !*confirmed {
                return;
            }
            let Some(input) = value.cast::<HtmlTextAreaElement>() else {
                return;
            };
            let fresh = input.value();
            if fresh.is_empty() || fresh.len() > 65_536 || (!replacing && name.trim().is_empty()) {
                error.set(Some(
                    "Enter a name and a nonempty value of at most 64 KiB.".into(),
                ));
                return;
            }
            error.set(None);
            // Clear before handing bytes to the asynchronous request. Neither a
            // failed request nor a page reload can re-seed this field.
            input.set_value("");
            submit.emit(FreshValue {
                name: name.trim().into(),
                description: (*description).clone(),
                organization: *organization,
                value: fresh,
            });
        })
    };
    let cancel = {
        let callback = props.on_cancel.clone();
        Callback::from(move |_| callback.emit(()))
    };
    html! {
        <form class="secrets-form" {onsubmit} autocomplete="off">
            <h2>{ if props.replacing { "Replace value" } else { "Create secret" } }</h2>
            if !props.replacing {
                <label>{"Name"}<input required=true maxlength="128" value={(*name).clone()} oninput={Callback::from(move |event: InputEvent| name.set(event.target_unchecked_into::<HtmlInputElement>().value()))} /></label>
                <label>{"Description"}<input maxlength="1000" value={(*description).clone()} oninput={Callback::from(move |event: InputEvent| description.set(event.target_unchecked_into::<HtmlInputElement>().value()))} /></label>
                <p>{"Names and descriptions are visible metadata. Keep credentials in the value field."}</p>
                if props.can_publish {
                    <label class="secrets-check"><input type="checkbox" checked={*organization} onchange={{ let organization = organization.clone(); Callback::from(move |event: Event| organization.set(event.target_unchecked_into::<HtmlInputElement>().checked())) }} />{"Available to the organization"}</label>
                    if *organization {
                        <label class="secrets-check"><input type="checkbox" checked={*confirmed} onchange={Callback::from(move |event: Event| confirmed.set(event.target_unchecked_into::<HtmlInputElement>().checked()))} />{"I understand every active member, including future members, can read and use this secret."}</label>
                    }
                }
            }
            <label>{"Fresh value"}<textarea ref={value} required=true autocomplete="off" spellcheck="false" aria-label="Fresh secret value" rows="4" /></label>
            <p>{"Up to 64 KiB. Whitespace and newlines are preserved. Saved values cannot be viewed or copied here. Replacing this value does not rotate the provider credential."}</p>
            <p>{"Portal hides matching text in your records. Short or common values may hide unrelated content."}</p>
            if let Some(error)=&*error { <p role="alert">{error}</p> }
            <div class="secrets-actions"><button type="submit">{"Save value"}</button><button type="button" onclick={cancel}>{"Cancel"}</button></div>
        </form>
    }
}
