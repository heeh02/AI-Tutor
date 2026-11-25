//! Source-checked historical comparison context alongside a remembered report.
use crate::{evidence_labels::source_label, view::icon};
use serde_json::Value;
use yew::prelude::*;

fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub comparison_context: Value,
    pub ui_language: String,
    pub namespace: String,
}
enum DisplayContext<'a> {
    None,
    Unavailable,
    Complete(&'a [Value]),
}
fn display_context(value: &Value) -> DisplayContext<'_> {
    if value.is_null() || value["status"] == "none" {
        return DisplayContext::None;
    }
    if value["status"] == "complete" {
        if let Some(items) = value["items"].as_array().filter(|items| !items.is_empty()) {
            return DisplayContext::Complete(items);
        }
    }
    DisplayContext::Unavailable
}
fn nonempty_list(value: &Value) -> bool {
    value.as_array().is_some_and(|items| !items.is_empty())
}
fn limits(item: &Value) -> (bool, bool, bool) {
    let trials = item["trials"].as_array().into_iter().flatten();
    let untested = nonempty_list(&item["limits"]["untested_labels"])
        || trials.clone().any(|trial| trial["status"] == "not_tested");
    let count_unknown = nonempty_list(&item["limits"]["unknown_count_labels"])
        || trials.clone().any(|trial| {
            trial["status"] == "reported"
                && trial["trial_count"]
                    .as_u64()
                    .filter(|count| *count > 0)
                    .is_none()
        });
    let conditions_unknown = nonempty_list(&item["limits"]["unconfirmed_conditions"])
        || item["conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|condition| condition["status"] != "reported_same");
    (untested, count_unknown, conditions_unknown)
}
fn trial_count(language: &str, trial: &Value) -> String {
    if trial["status"] != "reported" {
        return tr(
            language,
            "尚未测试，无已报告次数",
            "Not tested; no completed trial count",
        )
        .into();
    }
    match trial["trial_count"].as_u64().filter(|count| *count > 0) {
        Some(count) => {
            if language == "en" {
                format!("Reported trials: {count}")
            } else {
                format!("已报告次数：{count}")
            }
        }
        None => tr(
            language,
            "次数未知（原记录未填写）",
            "Trial count unknown (not recorded)",
        )
        .into(),
    }
}

#[function_component(ComparisonMemory)]
pub fn comparison_memory(props: &Props) -> Html {
    render_context(
        &props.comparison_context,
        &props.ui_language,
        &props.namespace,
    )
}
fn render_context(value: &Value, language: &str, namespace: &str) -> Html {
    let context = display_context(value);
    if matches!(context, DisplayContext::None) {
        return Html::default();
    }
    html! {
        <details class="comparison-memory">
            <summary><span>{icon("activity")}{tr(language,"实验条件与未测项","Experiment conditions & untested items")}</span>{icon("chevron")}</summary>
            <div class="comparison-memory-content">
            {match context {
                DisplayContext::Unavailable => html! {
                    <p class="comparison-memory-unavailable" role="status">{tr(language,
                        "原比较来源已更正或失效，当前教学不会沿用旧比较条件。",
                        "The original comparison sources were corrected or are no longer valid. Current teaching will not reuse those old comparison conditions.")}</p>
                },
                DisplayContext::Complete(items) => html! {<>
                    <p class="comparison-memory-note">{tr(language,
                        "这是历史比较记录。当前活动的搭建与条件需要重新确认；报告不等于硬件核验。",
                        "This is a historical comparison. The setup and conditions need confirmation for the current activity; a report is not hardware verification.")}</p>
                    {for items.iter().map(|item|render_item(item,language,namespace))}
                </>},
                DisplayContext::None => Html::default(),
            }}
            </div>
        </details>
    }
}
fn render_item(item: &Value, language: &str, namespace: &str) -> Html {
    let grade = if namespace == "simulator" {
        "simulated"
    } else {
        text(&item["source_type"])
    };
    html! {
        <article class="comparison-memory-item">
            <div class="comparison-memory-heading"><h5>{text(&item["question"])}</h5><span class={classes!("source",(grade=="simulated").then_some("simulation"))}>{source_label(language,grade)}</span></div>
            <p class="comparison-memory-variable"><small>{tr(language,"比较的变量","Variable being compared")}</small><span>{text(&item["changed_variable"])}</span></p>
            <div class="comparison-memory-conditions"><h6>{tr(language,"共同条件的原始记录","Original record of shared conditions")}</h6><ul>
                {for item["conditions"].as_array().into_iter().flatten().map(|condition|html!{
                    <li><span>{text(&condition["name"])}</span><small class={classes!((condition["status"]!="reported_same").then_some("condition-unconfirmed"))}>{if condition["status"]=="reported_same"{tr(language,"据报告保持一致","Reported unchanged")}else{tr(language,"尚未确认","Not confirmed")}}</small></li>
                })}
            </ul></div>
            <div class="comparison-memory-trials">{for item["trials"].as_array().into_iter().flatten().map(|trial|render_trial(trial,language))}</div>
            {render_limits(item,language)}
            {if item["pending_plan"].is_object(){let plan=&item["pending_plan"];html!{
                <div class="comparison-memory-plan"><h6>{tr(language,"原记录中的待做计划","Pending plan in the original record")}</h6><small>{if plan["accepted"]==true{tr(language,"记录时已接受 · 尚未实施","Recorded as accepted · not yet performed")}else{tr(language,"记录时未接受 · 尚未实施","Recorded as not accepted · not yet performed")}}</small><p>{text(&plan["text"])}</p></div>
            }}else{Html::default()}}
        </article>
    }
}
fn render_limits(item: &Value, language: &str) -> Html {
    let (untested, count_unknown, conditions_unknown) = limits(item);
    html! {<div class={classes!("comparison-memory-limits",(untested||count_unknown||conditions_unknown).then_some("incomplete"))}>
        {if untested {html!{<p>{tr(language,"仍有未测试项，不能据此判断比较效果。","Some items remain untested, so this record cannot establish the comparison's effect.")}</p>}}else{Html::default()}}
        {if count_unknown {html!{<p>{tr(language,"部分已报告项目的次数未知，不能据此判断重复性。","Some reported items have unknown trial counts; repeatability cannot be established from them.")}</p>}}else{Html::default()}}
        {if conditions_unknown {html!{<p>{tr(language,"部分共同条件尚未确认，即使出现结果差异，也不能直接归因于改变的条件。","Some shared conditions remain unconfirmed; any result difference cannot be attributed to the changed variable.")}</p>}}else{Html::default()}}
        <p>{tr(language,"有报告也不等于因果关系已证实。","Having reports does not establish causation.")}</p>
    </div>}
}
fn render_trial(trial: &Value, language: &str) -> Html {
    let reported = trial["status"] == "reported";
    html! {
        <section class={classes!("comparison-memory-trial",(!reported).then_some("untested"))}>
            <div class="comparison-memory-trial-heading"><strong>{text(&trial["label"])}</strong><span>{if reported{tr(language,"已有报告","Reported")}else{tr(language,"尚未测试","Not tested")}}</span></div>
            <p class="comparison-memory-setting">{text(&trial["setting"])}</p>
            <div class="comparison-memory-result"><small>{if reported{tr(language,"原始报告","Original report")}else{tr(language,"结果","Result")}}</small><p>{if reported{
                trial["result"].as_str().filter(|result|!result.is_empty()).unwrap_or_else(||tr(language,"未记录可展示的结果","No result text was recorded"))
            }else{tr(language,"尚无测试结果","No test result yet")}}</p></div>
            <small class="comparison-memory-count">{trial_count(language,trial)}</small>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn item() -> Value {
        json!({
            "comparison_id":"opaque-comparison","learner_id":"opaque-learner","session_id":"opaque-session",
            "historical":true,"hardware_verified":false,"source_type":"learner_report",
            "question":"改变电阻后会怎样？","changed_variable":"220 ohms / 330 ohms",
            "conditions":[{"name":"同一个 LED","status":"reported_same"},{"name":"Supply voltage","status":"not_confirmed"}],
            "trials":[{"label":"A","setting":"220 ohms, 3 V","status":"reported","result":"亮了。Brightness was not measured.","trial_count":null},
                {"label":"B","setting":"330 ohms; voltage undecided","status":"not_tested","result":null,"trial_count":null}],
            "pending_plan":{"text":"下次再试；条件尚未决定。","accepted":false},
            "source_links":[{"role":"observation","event_id":"opaque-event","event_version":1}],
            "limits":{"comparison_incomplete":true,"reported_labels":["A"],"untested_labels":["B"],"unknown_count_labels":["A"],"unconfirmed_conditions":[{"name":"Supply voltage","status":"not_confirmed"}],"causal_effect_established":false}
        })
    }
    #[test]
    fn unavailable_or_absent_context_never_renders_cached_comparison_fields() {
        let mut context = json!({"status":"unavailable","items":[item()],"reason":"internal-only"});
        let rendered = format!("{:?}", render_context(&context, "zh", "real"));
        assert!(rendered.contains("当前教学不会沿用旧比较条件"));
        assert!(!rendered.contains("220 ohms") && !rendered.contains("internal-only"));
        context["status"] = json!("none");
        assert!(matches!(display_context(&context), DisplayContext::None));
        assert!(matches!(
            display_context(&Value::Null),
            DisplayContext::None
        ));
    }
    #[test]
    fn incomplete_comparison_preserves_original_values_unknowns_and_source_grade() {
        let context = json!({"status":"complete","items":[item()]});
        for (language, count_label, condition_label, plan_label, boundary) in [
            (
                "zh",
                "次数未知（原记录未填写）",
                "尚未确认",
                "记录时未接受 · 尚未实施",
                "仍有未测试项，不能据此判断比较效果",
            ),
            (
                "en",
                "Trial count unknown (not recorded)",
                "Not confirmed",
                "Recorded as not accepted · not yet performed",
                "Some items remain untested",
            ),
        ] {
            let rendered = format!("{:?}", render_context(&context, language, "simulator"));
            for original in [
                "220 ohms, 3 V",
                "330 ohms; voltage undecided",
                "亮了。Brightness was not measured.",
                "下次再试；条件尚未决定。",
            ] {
                assert!(rendered.contains(original));
            }
            for label in [
                count_label,
                condition_label,
                plan_label,
                boundary,
                source_label(language, "simulated"),
            ] {
                assert!(rendered.contains(label));
            }
            for private in [
                "opaque-comparison",
                "opaque-learner",
                "opaque-session",
                "opaque-event",
            ] {
                assert!(!rendered.contains(private));
            }
        }
    }
    #[test]
    fn untested_trial_does_not_display_a_stray_result_or_completed_count() {
        let trial = json!({"label":"B","setting":"Original setting","status":"not_tested","result":"stale result must stay hidden","trial_count":88});
        let rendered = format!("{:?}", render_trial(&trial, "en"));
        assert!(rendered.contains("No test result yet"));
        assert!(!rendered.contains("stale result") && !rendered.contains("88"));
        assert_eq!(
            trial_count("zh", &json!({"status":"reported","trial_count":1})),
            "已报告次数：1"
        );
    }
    #[test]
    fn unknown_counts_or_conditions_do_not_mislabel_two_reported_items_as_untested() {
        let mut record = item();
        record["trials"][1]["status"] = json!("reported");
        record["trials"][1]["result"] = json!("Another reported result");
        record["trials"][1]["trial_count"] = json!(2);
        record["limits"]["untested_labels"] = json!([]);
        assert_eq!(limits(&record), (false, true, true));
        for language in ["zh", "en"] {
            let rendered = format!("{:?}", render_limits(&record, language));
            assert!(
                !rendered.contains("仍有未测试项")
                    && !rendered.contains("Some items remain untested")
            );
            assert!(rendered.contains(if language == "en" {
                "unknown trial counts"
            } else {
                "次数未知"
            }));
            assert!(rendered.contains(if language == "en" {
                "conditions remain unconfirmed"
            } else {
                "共同条件尚未确认"
            }));
        }
    }
    #[test]
    fn complete_reports_and_an_accepted_plan_still_do_not_establish_causation_or_completion() {
        let mut record = item();
        record["trials"][0]["trial_count"] = json!(1);
        record["trials"][1]["status"] = json!("reported");
        record["trials"][1]["result"] = json!("Another reported result");
        record["trials"][1]["trial_count"] = json!(2);
        record["conditions"][1]["status"] = json!("reported_same");
        record["limits"] = json!({"comparison_incomplete":false,"untested_labels":[],"unknown_count_labels":[],"unconfirmed_conditions":[],"causal_effect_established":false});
        record["pending_plan"]["accepted"] = json!(true);
        assert_eq!(limits(&record), (false, false, false));
        let rendered = format!("{:?}", render_item(&record, "en", "real"));
        assert!(rendered.contains("Having reports does not establish causation."));
        assert!(
            rendered.contains("Recorded as accepted · not yet performed")
                && rendered.contains("Reported trials: 2")
        );
        assert!(
            rendered.contains("Learner report") && !rendered.contains("Human-confirmed report")
        );
    }
}
