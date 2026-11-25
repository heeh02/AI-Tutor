use crate::{i18n::t, view::icon};
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
    html! {<div class="modal-backdrop" role="presentation"><div class="confirm-modal" role="dialog" aria-modal="true" aria-labelledby="forget-title">
        <span class="modal-icon">{icon("trash")}</span><h2 id="forget-title">{t(language,if props.all{"清除这个档案的学习数据？"}else{"忘记这条学习记忆？"})}</h2>
        {if props.all{html!{<p><strong>{t(language,"档案：")}</strong>{&props.content}</p>}}else{html!{<p>{&props.content}</p>}}}
        <small>{t(language,"相关派生记录和本应用创建的对话记录也将清理。此操作无法撤销。")}</small>
        <div><button class="secondary" onclick={props.on_cancel.clone()}>{t(language,"取消")}</button><button class="danger-button" disabled={props.busy} onclick={props.on_confirm.clone()}>{t(language,if props.all{"确认清除档案数据"}else{"确认忘记这条记忆"})}</button></div>
    </div></div>}
}
