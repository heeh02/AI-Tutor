use crate::{i18n::t, view::icon};
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ConfirmForgetProps {
    pub all: bool,
    pub content: String,
    pub ui_language: String,
    pub busy: bool,
    pub on_cancel: Callback<MouseEvent>,
    pub on_confirm: Callback<MouseEvent>,
}
#[function_component(ConfirmForget)]
pub fn confirm_forget(props: &ConfirmForgetProps) -> Html {
    let language = props.ui_language.as_str();
    let cancel = use_node_ref();
    let confirm = use_node_ref();
    let previous_focus = use_mut_ref(|| {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.active_element())
    });
    {
        let cancel = cancel.clone();
        let previous_focus = previous_focus.clone();
        use_effect_with((), move |_| {
            if let Some(button) = cancel.cast::<HtmlElement>() {
                let _ = button.focus();
            }
            move || {
                if let Some(element) = previous_focus
                    .borrow()
                    .as_ref()
                    .and_then(|element| element.clone().dyn_into::<HtmlElement>().ok())
                {
                    let _ = element.focus();
                }
            }
        });
    }
    let onkeydown = {
        let cancel = cancel.clone();
        let confirm = confirm.clone();
        let busy = props.busy;
        Callback::from(move |event: KeyboardEvent| {
            if event.key() != "Tab" {
                return;
            }
            event.prevent_default();
            let cancel_focused = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.active_element())
                .is_some_and(|element| element.id() == "forget-cancel");
            let target = if !busy && cancel_focused {
                &confirm
            } else {
                &cancel
            };
            if let Some(button) = target.cast::<HtmlElement>() {
                let _ = button.focus();
            }
        })
    };
    html! {<div class="modal-backdrop" role="presentation"><div class="confirm-modal" role="dialog" aria-modal="true" aria-labelledby="forget-title" aria-describedby="forget-warning" onkeydown={onkeydown}>
        <span class="modal-icon">{icon("trash")}</span><h2 id="forget-title">{t(language,if props.all{"清除这个档案的学习数据？"}else{"忘记这条学习记忆？"})}</h2>
        {if props.all{html!{<p><strong>{t(language,"档案：")}</strong>{&props.content}</p>}}else{html!{<p>{&props.content}</p>}}}
        <small id="forget-warning">{t(language,"相关派生记录和本应用创建的对话记录也将清理。此操作无法撤销。")}</small>
        <div><button id="forget-cancel" ref={cancel} class="secondary" onclick={props.on_cancel.clone()}>{t(language,"取消")}</button><button id="forget-confirm" ref={confirm} class="danger-button" disabled={props.busy} onclick={props.on_confirm.clone()}>{t(language,if props.all{"确认清除档案数据"}else{"确认忘记这条记忆"})}</button></div>
    </div></div>}
}
