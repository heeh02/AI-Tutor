//! User-facing recovery text stays localized; service diagnostics stay verbatim.
use crate::{i18n::t, view::icon};
use yew::prelude::*;

fn known_message(message: &str) -> bool {
    matches!(
        message,
        "请求未被接受，请核对当前活动。"
            | "请先结束录音并处理转写，再继续过去的活动。"
            | "请先保存当前模块或观察报告，再继续过去的活动。"
            | "录音响应缺少有效编号，请停止后重新录音。"
            | "录音请求已过期，请重新录音。"
            | "请先处理输入草稿和未采用文字，再切换档案或活动。"
            | "请先等待本次发送结果，再切换档案或活动。"
            | "合并后超过4000字，请先精简草稿；未采用文字仍已保留。"
            | "没有识别到清楚的语音，请重录或输入文字。"
            | "已删除该档案的记忆与活动记录"
            | "请在学习记忆中选择具体条目并忘记，或发送“忘记全部记忆”。本次请求未发送远程模型。"
            | "请先保存或清空比较草稿，再切换档案或活动。"
    )
}
#[derive(Properties, PartialEq)]
pub struct OperationNoticeProps {
    pub ui_language: String,
    pub detail: String,
    pub on_dismiss: Callback<MouseEvent>,
}
#[function_component(OperationNotice)]
pub fn operation_notice(props: &OperationNoticeProps) -> Html {
    let language = props.ui_language.as_str();
    html! {<div class="error-banner" role="alert">{icon("shield")}<div class="operation-message">
    {if known_message(&props.detail){html!{<p>{t(language,&props.detail).to_owned()}</p>}}else{html!{<><p>{t(language,"操作未完成。请检查连接或设置后重试；已有输入不会因此被自动发送。")}</p><details><summary>{t(language,"查看服务详情（原文）")}</summary><p>{&props.detail}</p></details></>}}}
    </div><button aria-label={t(language,"关闭提示")} onclick={props.on_dismiss.clone()}>{icon("close")}</button></div>}
}
