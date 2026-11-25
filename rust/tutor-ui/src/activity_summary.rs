use crate::{
    evidence_labels::source_label,
    i18n::t,
    model::{MODULES, fallback, list, text},
    view::icon,
};
use serde_json::Value;
use yew::prelude::*;

fn field_source(state: &Value, field: &str) -> Value {
    state[field]
        .get("source_type")
        .or_else(|| state.get(format!("{field}_source")))
        .cloned()
        .unwrap_or(Value::Null)
}
fn badge(language: &str, value: &Value) -> Html {
    let raw = if value.is_object() {
        text(&value["source_type"])
    } else {
        text(value)
    };
    if raw.is_empty() {
        return Html::default();
    }
    html! {<span class={classes!("source",(raw=="simulated").then_some("simulation"))}>{source_label(language,&raw)}</span>}
}
fn stage_label(language: &str, value: &Value) -> String {
    let label = match value.as_str().unwrap_or("") {
        "goal_setting" => "提出想法",
        "inventory_check" => "确认模块",
        "capability_gap" => "寻找能力",
        "build_guidance" => "搭建检查",
        "test_preparation" => "准备测试",
        "observation" => "观察结果",
        "comparison" => "比较与修改",
        "reflection" => "回顾学习",
        "paused" => "已暂停",
        "completed" => "活动结束",
        _ => "等待活动",
    };
    t(language, label).into()
}
#[derive(Properties, PartialEq)]
pub struct ActivitySummaryProps {
    pub state: Value,
    pub ui_language: String,
    pub finished: bool,
    pub pending_step_needs_confirmation: bool,
}
pub fn pending_needs_confirmation(session: &Value) -> bool {
    // An older response without read validation is not support for a saved step.
    session["state"]
        .get("pending_step")
        .is_some_and(|v| v.as_str() != Some(""))
        && session["pending_step_needs_confirmation"] != false
}
fn pending_current_text(props: &ActivitySummaryProps) -> String {
    if props.pending_step_needs_confirmation {
        if props.ui_language == "en" {
            "The saved pending step needs confirmation."
        } else {
            "已保存的待办步骤需要重新确认。"
        }
        .into()
    } else {
        fallback(
            &props.state["pending_step"],
            t(&props.ui_language, "从当前目标决定下一步"),
        )
    }
}
fn pending_detail(props: &ActivitySummaryProps) -> Html {
    if props.pending_step_needs_confirmation {
        html! {<details class="pending-original"><summary>{if props.ui_language=="en" {"View original saved step (unverified)"}else{"查看原始待办记录（尚未核实）"}}</summary><p>{text(&props.state["pending_step"])}</p></details>}
    } else {
        badge(
            &props.ui_language,
            &field_source(&props.state, "pending_step"),
        )
    }
}
#[function_component(ActivitySummary)]
pub fn activity_summary(props: &ActivitySummaryProps) -> Html {
    render_summary(props)
}
fn render_summary(props: &ActivitySummaryProps) -> Html {
    let state = &props.state;
    let language = props.ui_language.as_str();
    let inventory = if state["modules"].is_array() {
        list(&state["modules"])
    } else {
        list(&state["modules"]["value"])
    };
    let attempts = list(&state["attempts"]);
    let latest = if text(&state["reported_result"]).is_empty() {
        fallback(&state["latest_input"], t(language, "还没有记录本次观察"))
    } else {
        text(&state["reported_result"])
    };
    html! {<>
        <div class="panel-title"><div><span class="section-number">{"03"}</span><h2>{t(language,if props.finished{"本次学习回顾"}else{"学习正在发生"})}</h2></div><span class="small-label">{if props.finished{t(language,"已结束").into()}else{stage_label(language,&state["stage"])}}</span></div>
        <div class="goal-card"><span class="gold-icon">{icon("flag")}</span><small>{t(language,"当前目标")}</small><h3>{fallback(&state["goal"],t(language,"等待学习者提出想法"))}</h3>{badge(language,&field_source(state,"goal"))}</div>
        <div class="module-section"><div class="subsection-label"><h3>{t(language,"功能模块")}</h3>{badge(language,&field_source(state,"modules"))}</div><div class="module-grid">{for MODULES.iter().map(|(id,name)|{let reported=inventory.iter().any(|v|v.as_str()==Some(*id));html!{<div class={classes!("module-tile",reported.then_some("reported"))}><span>{icon(if *id=="light"{"light"}else if *id=="distance"{"radio"}else{"circuit"})}</span><strong>{t(language,name)}</strong><small>{t(language,if reported{"已报告"}else if state["modules"].is_null(){"当前未知"}else{"未报告连接"})}</small></div>}})}</div><p class="fine-print">{t(language,"连接状态来自报告；人脸摄像头不识别积木。")}</p></div>
        <div class="learning-timeline"><div class="timeline-item"><span>{icon("activity")}</span><div><small>{t(language,"最新观察 / 报告")}</small><p>{latest}</p>{badge(language,&field_source(state,"reported_result"))}</div></div>
        <div class="timeline-item"><span>{icon("history")}</span><div><small>{t(language,"已有尝试与观察")}</small>{if attempts.is_empty(){html!{<p class="muted">{t(language,"等待记录本次尝试或观察")}</p>}}else{html!{<ul>{for attempts.iter().skip(attempts.len().saturating_sub(3)).map(|attempt|html!{<li>{text(attempt)}</li>})}</ul>}}}</div></div>
        <div class="timeline-item next"><span>{icon("arrow")}</span><div><small>{t(language,"待确认 / 下一次实验")}</small><p>{pending_current_text(props)}</p>{pending_detail(props)}</div></div></div>
    </>}
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn english_activity_labels_do_not_translate_verbatim_reports_or_goals() {
        let rendered = format!(
            "{:?}",
            render_summary(&ActivitySummaryProps {
                ui_language: "en".into(),
                finished: false,
                pending_step_needs_confirmation: false,
                state: json!({"stage":"comparison","goal":"发送","reported_result":"还没有记录本次观察","modules":["motor"],"reported_result_source":"operator_confirmed"})
            })
        );
        assert!(rendered.contains("Compare and revise"));
        assert!(rendered.contains("Motor"));
        assert!(rendered.contains("发送"));
        assert!(rendered.contains("还没有记录本次观察"));
        assert!(rendered.contains("Human-confirmed report"));
        assert!(!rendered.contains("最新观察 / 报告"));
    }
    #[test]
    fn unsupported_saved_step_is_only_an_inspectable_unverified_original() {
        let raw = "先在不靠近运动部件的情况下，使用设备设计的停止按钮或电源开关停车；若做不到，请现场熟悉设备的人协助。";
        for (language, warning, original) in [
            (
                "zh",
                "已保存的待办步骤需要重新确认。",
                "查看原始待办记录（尚未核实）",
            ),
            (
                "en",
                "The saved pending step needs confirmation.",
                "View original saved step (unverified)",
            ),
        ] {
            let props = ActivitySummaryProps {
                state: json!({"pending_step":raw,"pending_step_source":"learner_report","field_evidence":{"pending_step":{"source_event_ids":["source"]}}}),
                ui_language: language.into(),
                finished: false,
                pending_step_needs_confirmation: true,
            };
            let before = props.state.clone();
            assert_eq!(pending_current_text(&props), warning);
            let detail = format!("{:?}", pending_detail(&props));
            assert!(
                detail.contains("details") && detail.contains(original) && detail.contains(raw)
            );
            assert!(!detail.contains(source_label(language, "learner_report")));
            assert_eq!(props.state, before);
        }
    }
    #[test]
    fn supported_step_and_empty_default_keep_their_existing_display() {
        let mut props = ActivitySummaryProps {
            state: json!({"pending_step":"Stop the rover first."}),
            ui_language: "en".into(),
            finished: false,
            pending_step_needs_confirmation: false,
        };
        assert_eq!(pending_current_text(&props), "Stop the rover first.");
        assert!(!format!("{:?}", pending_detail(&props)).contains("details"));
        props.state["pending_step"] = json!("");
        assert_eq!(
            pending_current_text(&props),
            t("en", "从当前目标决定下一步")
        );
        assert!(!format!("{:?}", pending_detail(&props)).contains("unverified"));
    }
    #[test]
    fn nonempty_step_needs_explicit_read_validation_even_in_an_older_payload() {
        let mut session = json!({"state":{"pending_step":"Stop the rover first."}});
        assert!(pending_needs_confirmation(&session));
        session["pending_step_needs_confirmation"] = json!(false);
        assert!(!pending_needs_confirmation(&session));
        session["pending_step_needs_confirmation"] = json!(true);
        assert!(pending_needs_confirmation(&session));
        session["state"]["pending_step"] = json!("");
        assert!(!pending_needs_confirmation(&session));
        assert!(!pending_needs_confirmation(&json!({"state":{}})));
    }
}
