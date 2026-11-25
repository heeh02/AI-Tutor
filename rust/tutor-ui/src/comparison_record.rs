//! Shared read-only projection of a saved comparison record.
use crate::evidence_labels::source_label;
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
fn date(value: &Value) -> &str {
    value
        .as_str()
        .and_then(|value| value.get(..10))
        .filter(|value| {
            value.bytes().enumerate().all(|(index, byte)| {
                if index == 4 || index == 7 {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
        })
        .unwrap_or("")
}

pub(crate) fn render_record(language: &str, item: &Value, historical: bool) -> Html {
    html! {
        <details class="comparison-record">
            <summary><span class="comparison-original">{text(&item["question"])}</span><small>{date(&item["created_at"])}</small></summary>
            <p class="fine-print">{source_label(language,text(&item["source_type"]))}{" · "}{tr(language,"未经硬件验证","Not hardware verified")}</p>
            {if historical {html!{<p class="fine-print">{tr(language,"这是当时的记录。当前条件，以及计划后来是否完成，都需要重新核对。","This is the record from that time. Current conditions and any later completion need to be checked separately.")}</p>}} else {Html::default()}}
            <p><strong>{tr(language,"改变的条件：","What changed: ")}</strong><span class="comparison-original">{text(&item["changed_variable"])}</span></p>
            <ul>{for item["conditions"].as_array().into_iter().flatten().map(|condition|html!{
                <li><span class="comparison-original">{text(&condition["name"])}</span>{" — "}{if condition["status"]=="reported_same"{tr(language,"报告为相同","Reported as the same")}else{tr(language,"尚未核对","Not checked")}}</li>
            })}</ul>
            <div class="comparison-trials">{for item["trials"].as_array().into_iter().flatten().map(|trial|html!{
                <div class="comparison-trial"><strong class="comparison-original">{format!("{} · {}",text(&trial["label"]),text(&trial["setting"]))}</strong>
                    {if trial["status"]=="reported" {html!{<>
                        <small>{tr(language,"观察报告","Reported observation")}</small>
                        <p class="comparison-original">{text(&trial["result"])}</p>
                        <small>{trial["trial_count"].as_u64().filter(|count|*count>0).map(|count|if language=="en"{format!("Trials: {count}")}else{format!("次数：{count}")}).unwrap_or_else(||tr(language,"次数：未知","Trial count: unknown").into())}</small>
                    </>}} else {html!{<p>{if historical{tr(language,"记录时尚未测试；没有结果","Not tested at the time; no recorded result")}else{tr(language,"尚未测试；没有结果","Not tested; no result")}}</p>}}}
                </div>
            })}</div>
            {if item["pending_plan"].is_object(){html!{
                <div class="comparison-plan"><strong>{tr(language,"记录中的待测计划","Pending plan in the record")}</strong>
                    <p class="comparison-original">{text(&item["pending_plan"]["text"])}</p>
                    <small>{if item["pending_plan"]["accepted"]==true{tr(language,"记录时已接受 · 尚未实施","Recorded as accepted · not yet performed")}else{tr(language,"记录时未接受 · 尚未实施","Recorded as not accepted · not yet performed")}}</small>
                </div>
            }}else{Html::default()}}
        </details>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn saved_record_keeps_original_values_unknowns_and_historical_plan_status() {
        let mut item = json!({"comparison_id":"private-comparison","submission_id":"private-submission",
            "created_at":"2026-11-10T02:00:00Z","question":"原问题：What changes?","changed_variable":"220 / 330 ohms",
            "source_type":"simulated","hardware_verified":false,
            "conditions":[{"name":"原条件\nSupply undecided","status":"not_confirmed"}],
            "trials":[{"label":"A","setting":"220 ohms, 3 V","status":"reported","result":"原结果\nLED lit once.","trial_count":null},
                {"label":"B","setting":"330 ohms","status":"not_tested","result":"stale result must not show","trial_count":99}],
            "pending_plan":{"text":"原计划，尚待核对。","accepted":false}});
        for language in ["zh", "en"] {
            let rendered = format!("{:?}", render_record(language, &item, true));
            for original in [
                "原问题：What changes?",
                "220 ohms, 3 V",
                "330 ohms",
                "原结果",
                "LED lit once.",
                "原计划，尚待核对。",
            ] {
                assert!(rendered.contains(original));
            }
            assert!(rendered.contains(tr(language, "次数：未知", "Trial count: unknown")));
            assert!(rendered.contains(tr(
                language,
                "记录时尚未测试；没有结果",
                "Not tested at the time; no recorded result"
            )));
            assert!(rendered.contains(tr(
                language,
                "记录时未接受 · 尚未实施",
                "Recorded as not accepted · not yet performed"
            )));
            assert!(rendered.contains(source_label(language, "simulated")));
            for hidden in [
                "private-comparison",
                "private-submission",
                "stale result must not show",
                "99",
            ] {
                assert!(!rendered.contains(hidden));
            }
        }
        item["pending_plan"]["accepted"] = json!(true);
        let rendered = format!("{:?}", render_record("en", &item, false));
        assert!(rendered.contains("Recorded as accepted · not yet performed"));
        assert!(!rendered.contains("still pending"));
    }
}
