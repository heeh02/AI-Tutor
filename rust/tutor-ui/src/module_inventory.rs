use crate::{
    evidence_labels::source_label,
    i18n::t,
    model::MODULES,
    view::{icon, SelectControl},
};
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct ModuleInventoryProps {
    pub ui_language: String,
    pub modules: Vec<String>,
    pub modules_dirty: bool,
    pub report: String,
    pub notice: String,
    pub source: String,
    pub simulated: bool,
    pub inactive: bool,
    pub busy: bool,
    pub can_save: bool,
    pub on_module: Callback<(String, bool)>,
    pub on_report: Callback<String>,
    pub on_source: Callback<String>,
    pub on_confirm: Callback<MouseEvent>,
    pub on_save: Callback<MouseEvent>,
}
#[function_component(ModuleInventory)]
pub fn module_inventory(props: &ModuleInventoryProps) -> Html {
    let language = props.ui_language.as_str();
    let report = props.on_report.clone();
    let source = props.on_source.clone();
    html! {<div><h3>{t(language,"当前模块与证据")}</h3>
        <div class="module-checkboxes">{for MODULES.iter().map(|(id,name)|{let id=id.to_string();let callback=props.on_module.clone();let selected=props.modules.contains(&id);html!{<label><input type="checkbox" disabled={props.inactive} checked={selected} onchange={Callback::from(move |event:Event|callback.emit((id.clone(),event.target_unchecked_into::<HtmlInputElement>().checked())))}/>{t(language,name)}</label>}})}</div>
        <label class="field-label">{t(language,"来源")}<SelectControl value={if props.simulated{"simulated".to_owned()}else{props.source.clone()}} disabled={props.simulated||props.inactive} onchange={Callback::from(move |event:Event|source.emit(event.target_unchecked_into::<HtmlSelectElement>().value()))} label={t(language,"报告来源")}>
            {if props.simulated{html!{<option value="simulated">{source_label(language,"simulated")}</option>}}else{html!{<><option value="operator_confirmed">{source_label(language,"operator_confirmed")}</option><option value="learner_report">{source_label(language,"learner_report")}</option></>}}}
        </SelectControl></label>
        <button class="secondary" disabled={props.inactive||props.busy} onclick={props.on_confirm.clone()}>{t(language,"确认当前勾选模块")}</button>
        <p class="fine-print">{t(language,"仅填写观察不会改变模块。全部取消勾选并确认，可明确记录当前未连接模块。")}</p>
        <textarea class="small-textarea" aria-label={t(language,"教师活动报告")} placeholder={t(language,"本次试验的观察，如：停车亮灯成功，想让间隙更大")} disabled={props.inactive} value={props.report.clone()} maxlength="1000" oninput={Callback::from(move |event:InputEvent|report.emit(event.target_unchecked_into::<HtmlTextAreaElement>().value()))}/>
        <button class="secondary" disabled={props.inactive||props.busy||!props.can_save} onclick={props.on_save.clone()}>{icon("check")}{t(language,if props.modules_dirty{"保存模块变更与报告"}else{"记录本次观察"})}</button>
        {if !props.notice.is_empty(){html!{<p class="report-notice" role="status">{t(language,&props.notice).to_owned()}</p>}}else{Html::default()}}
    </div>}
}
