//! On-demand provenance for one current memory. Only explicit display fields
//! leave the evidence response; source IDs and worker internals stay hidden.
use crate::{
    api::{api, ApiResult},
    comparison_memory::ComparisonMemory,
    evidence_labels::{source_description, source_label},
};
use serde_json::Value;
use yew::prelude::*;

fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}

pub fn kind_label(language: &str, kind: &str) -> &'static str {
    let (zh, en) = match kind {
        "learning_goal" => ("学习目标", "Learning goal"),
        "context_constraint" => ("任务条件与推断边界", "Task conditions and inference limits"),
        "trial_observation" => ("试验观察（报告）", "Reported trial observation"),
        "concept_evidence" => ("具体任务中的回答", "Task-specific response"),
        "revisable_misconception" => ("待核对的理解", "Interpretation to revisit"),
        "help_preference" => ("明确帮助偏好", "Explicit help preference"),
        "accepted_pending_experiment" | "pending_experiment" => {
            ("待做实验计划", "Pending experiment plan")
        }
        "correction" => ("更正记录", "Correction"),
        "reported_progress" => ("已报告进展", "Reported progress"),
        _ => ("学习记录", "Learning record"),
    };
    tr(language, zh, en)
}

pub fn scope_label(language: &str, scope: &str) -> &'static str {
    let (zh, en) = match scope {
        "current_session" => ("本次活动", "This session"),
        "current_activity" => ("当前学习活动", "Current learning activity"),
        "cross_session_explicit" => (
            "明确保留至后续活动",
            "Explicitly retained for later sessions",
        ),
        _ => ("原记录范围", "Original record scope"),
    };
    tr(language, zh, en)
}

fn support_label(language: &str, support: &str) -> &'static str {
    let (zh, en) = match support {
        "independent" => ("记录为独立作答", "Recorded as independent"),
        "hinted" => ("记录为接受提示", "Recorded with hints"),
        "scaffolded" => ("记录为逐步支持", "Recorded with step-by-step support"),
        _ => ("未记录", "Not recorded"),
    };
    tr(language, zh, en)
}

fn event_label(language: &str, kind: &str) -> &'static str {
    let (zh, en) = match kind {
        "learner_statement" => ("学习者表述", "Learner statement"),
        "learner_answer" => ("学习者回答", "Learner answer"),
        "observation" => ("观察报告", "Reported observation"),
        "plan" => ("计划（尚未证明完成）", "Plan (completion not established)"),
        "teacher_setting" => ("教师设置", "Teacher setting"),
        "correction" => ("更正原句", "Correction"),
        _ => ("历史输入", "Historical input"),
    };
    tr(language, zh, en)
}

fn source_grade<'a>(namespace: &str, value: &'a Value) -> &'a str {
    if namespace == "simulator" {
        "simulated"
    } else {
        value.as_str().unwrap_or("")
    }
}

/// Never serialize a source object: it can contain IDs, settings or internals.
fn original_text(content: &Value) -> Vec<&str> {
    let mut quotes = Vec::new();
    for key in ["text", "report", "correction"] {
        if let Some(value) = content[key]
            .as_str()
            .filter(|value| !value.trim().is_empty())
        {
            if !quotes.contains(&value) {
                quotes.push(value);
            }
        }
    }
    quotes
}

fn date(value: &Value) -> Option<&str> {
    let date = value.as_str()?.get(..10)?;
    date.bytes()
        .enumerate()
        .all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
        .then_some(date)
}

#[derive(Properties, PartialEq)]
pub struct EvidenceProps {
    pub learner_id: String,
    pub learner_epoch: i64,
    pub namespace: String,
    pub memory: Value,
    pub ui_language: String,
    pub reading_level: String,
    pub enabled: bool,
}

#[derive(Clone, PartialEq)]
struct EvidenceContext {
    learner_id: String,
    learner_epoch: i64,
    namespace: String,
    memory: Value,
    enabled: bool,
}
impl EvidenceContext {
    fn from_props(props: &EvidenceProps) -> Self {
        Self {
            learner_id: props.learner_id.clone(),
            learner_epoch: props.learner_epoch,
            namespace: props.namespace.clone(),
            memory: props.memory.clone(),
            enabled: props.enabled
                && !props.learner_id.is_empty()
                && props.memory["learner_id"] == props.learner_id
                && props.memory["status"] == "active",
        }
    }
}

struct EvidenceState {
    context: EvidenceContext,
    revision: u64,
    open: bool,
    value: Option<Value>,
    unavailable: bool,
}
impl EvidenceState {
    fn new(context: EvidenceContext) -> Self {
        Self {
            context,
            revision: 0,
            open: false,
            value: None,
            unavailable: false,
        }
    }
    fn clear(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.open = false;
        self.value = None;
        self.unavailable = false;
    }
    fn bind(&mut self, context: EvidenceContext) {
        if self.context != context {
            self.clear();
            self.context = context;
        }
    }
    fn toggle(&mut self) {
        let open = !self.open && self.context.enabled;
        self.clear();
        self.open = open;
    }
    fn needs_request(&self) -> bool {
        self.context.enabled && self.open && self.value.is_none() && !self.unavailable
    }
    fn receive(&mut self, revision: u64, result: ApiResult) -> bool {
        if revision != self.revision || !self.needs_request() {
            return false;
        }
        match result {
            Ok(value)
                if value["memory"] == self.context.memory
                    && value["namespace"] == self.context.namespace =>
            {
                self.value = Some(value)
            }
            _ => self.unavailable = true,
        }
        true
    }
}

pub enum EvidenceMsg {
    Toggle,
    Fetch,
    Result(u64, ApiResult),
}

pub struct MemoryEvidence {
    evidence: EvidenceState,
    pending: Option<u64>,
}
impl Component for MemoryEvidence {
    type Message = EvidenceMsg;
    type Properties = EvidenceProps;
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            evidence: EvidenceState::new(EvidenceContext::from_props(ctx.props())),
            pending: None,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Self::Properties) -> bool {
        self.evidence.bind(EvidenceContext::from_props(ctx.props()));
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: EvidenceMsg) -> bool {
        self.evidence.bind(EvidenceContext::from_props(ctx.props()));
        match msg {
            EvidenceMsg::Toggle => {
                self.evidence.toggle();
                if self.evidence.needs_request() {
                    ctx.link().send_message(EvidenceMsg::Fetch);
                }
                true
            }
            EvidenceMsg::Fetch => {
                if self.pending.is_some() || !self.evidence.needs_request() {
                    return false;
                }
                let revision = self.evidence.revision;
                let context = &self.evidence.context;
                let mid = context.memory["memory_id"].as_str().unwrap_or("");
                let path = format!(
                    "/api/learners/{}/memories/{mid}/evidence",
                    context.learner_id
                );
                self.pending = Some(revision);
                ctx.link().send_future(async move {
                    EvidenceMsg::Result(revision, api(&path, "GET", None).await)
                });
                false
            }
            EvidenceMsg::Result(revision, result) => {
                if self.pending != Some(revision) {
                    return false;
                }
                self.pending = None;
                let changed = self.evidence.receive(revision, result);
                // A collapse/reopen may have invalidated this request. Keep one
                // request in flight and then load the newly opened context.
                if self.evidence.needs_request() {
                    ctx.link().send_message(EvidenceMsg::Fetch);
                }
                changed
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let language = &ctx.props().ui_language;
        html! {
            <div class="memory-evidence">
                <button class="text-button evidence-toggle" type="button"
                    aria-expanded={self.evidence.open.to_string()}
                    disabled={!self.evidence.context.enabled}
                    onclick={ctx.link().callback(|_| EvidenceMsg::Toggle)}>
                    {if self.evidence.open { tr(language,"收起来源说明","Hide sources") }
                        else { tr(language,"为什么记住","Why this was remembered") }}
                </button>
                {if !self.evidence.open { Html::default() }
                else if let Some(value) = &self.evidence.value { render_evidence(language, &ctx.props().reading_level, value) }
                else if self.evidence.unavailable { html! {
                    <p class="evidence-status" role="status">{tr(language,
                        "来源暂不可用，记录可能已变更。请刷新记忆列表后重新展开。",
                        "Sources are unavailable or the record has changed. Refresh the memory list and reopen this explanation.")}</p>
                }} else { html! {
                    <p class="evidence-status" role="status">{tr(language,"正在读取原始来源…","Loading original sources…")}</p>
                }}}
            </div>
        }
    }
}

const IMPORTANCE_FIELDS: [(&str, &str, &str); 4] = [
    ("goal_relevance", "与学习目标相关", "Goal relevance"),
    ("continuity", "便于下次继续", "Continuity"),
    ("novel_change", "记录新的变化", "New information"),
    ("explicit_request", "明确要求保留", "Explicit request"),
];
fn retention_reasons(language: &str, metadata: &Value) -> Vec<&'static str> {
    IMPORTANCE_FIELDS
        .iter()
        .filter_map(|&(key, zh, en)| {
            metadata["importance"][key]
                .as_u64()
                .filter(|score| (1..=3).contains(score))
                .map(|_| tr(language, zh, en))
        })
        .collect()
}
fn explicit_preference_reason(language: &str, value: &Value) -> Option<&'static str> {
    (value["memory"]["kind"] == "help_preference"
        && !value["education_metadata"]["importance"].is_object()
        && value["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|source| source["content"]["confirmation"] == "explicit_save_action"))
    .then(|| {
        tr(
            language,
            "你明确选择了在以后活动中使用这个讲解偏好。",
            "You explicitly chose to use this explanation preference in future activities.",
        )
    })
}
fn render_evidence(language: &str, reading_level: &str, value: &Value) -> Html {
    let metadata = &value["education_metadata"];
    let namespace = value["namespace"].as_str().unwrap_or("");
    let grade = source_grade(namespace, &value["memory"]["source_type"]);
    let reasons = retention_reasons(language, metadata);
    let explicit_reason = explicit_preference_reason(language, value);
    let kind = metadata["kind"]
        .as_str()
        .filter(|kind| !kind.is_empty())
        .or_else(|| value["memory"]["kind"].as_str())
        .unwrap_or("");
    let plain = reading_level == "plain";
    html! {
        <div class="evidence-content">
            <section>
                <h4>{if plain{tr(language,"谁提供了这条记录","Who reported this")}else{tr(language,"来源与可信度边界","Source and credibility limits")}}</h4>
                <span class={classes!("source", (grade == "simulated").then_some("simulation"))}>{source_label(language, grade)}</span>
                <p>{source_description(language, grade, reading_level=="plain")}</p>
            </section>
            <section class="evidence-summary">
                <h4>{tr(language,"为什么保留","Why keep it")}</h4>
                <p>{kind_label(language,kind)}</p>
                {if reasons.is_empty(){html!{<p>{explicit_reason.unwrap_or_else(||tr(language,"这条记录没有具体的保留理由分项。","No specific retention reasons were recorded."))}</p>}}else{html!{<ul>{for reasons.iter().map(|reason|html!{<li>{*reason}</li>})}</ul>}}}
                <p class="evidence-note">{tr(language,"这些理由说明记录有什么用，不是学习成绩。计划仍不表示已完成。","These reasons describe usefulness, not a learning score. A plan still does not mean it was completed.")}</p>
            </section>
            <ComparisonMemory comparison_context={value["comparison_context"].clone()}
                ui_language={language.to_owned()} namespace={namespace.to_owned()}/>
            <details class="evidence-advanced" open={reading_level=="technical"}>
                <summary>{tr(language,"查看分类、帮助程度与保留分项","Category, task support and retention factors")}</summary>
            <section>
                <h4>{tr(language,"这条记录说明什么","What this record describes")}</h4>
                {if metadata.is_object() { html! {
                    <><dl class="evidence-fields">
                        <dt>{tr(language,"教育类型","Educational category")}</dt><dd>{kind_label(language,metadata["kind"].as_str().unwrap_or(""))}</dd>
                        <dt>{tr(language,"当时的帮助程度","Support during the task")}</dt><dd>{support_label(language,metadata["support_level"].as_str().unwrap_or(""))}</dd>
                    </dl><p class="evidence-note">{tr(language,"帮助程度只描述这次任务记录，不代表固定能力。计划和意向不表示已经完成。","Support describes this task only, not a fixed ability. Plans and intentions do not establish completion.")}</p></>
                }} else { html! {<p>{tr(language,"旧记录未附教育分类和帮助程度，不作补充推断。","This older record has no educational category or support level; none is inferred.")}</p>} }}
            </section>
            <section>
                <h4>{tr(language,"为何保留（重要性）","Why retain it (importance)")}</h4>
                <p class="evidence-note">{tr(language,"分项表示记录对后续教学的保留价值，不是事实可信度或学习成绩。","These factors describe usefulness for future teaching, not factual confidence or learning performance.")}</p>
                <dl class="evidence-fields">{for IMPORTANCE_FIELDS.iter().map(|&(key, zh, en)| {
                    let score = metadata["importance"][key].as_u64().filter(|score| *score <= 3);
                    html! {<><dt>{tr(language,zh,en)}</dt><dd>{score.map(|score|format!("{score}/3")).unwrap_or_else(||tr(language,"未记录","Not recorded").into())}</dd></>}
                })}</dl>
            </section>
            </details>
            <section>
                <h4>{tr(language,"原句与记录日期（UTC）","Original wording and record date (UTC)")}</h4>
                <ol class="evidence-sources">{for value["sources"].as_array().into_iter().flatten().map(|source| {
                    let grade = source_grade(namespace, &source["source_type"]);
                    let quotes = original_text(&source["content"]);
                    html! {<li>
                        <div class="evidence-source-heading"><span class="source">{source_label(language,grade)}</span><span>{event_label(language,source["event_kind"].as_str().unwrap_or(""))}</span><time>{date(&source["created_at"]).unwrap_or_else(||tr(language,"日期未记录","Date not recorded"))}</time></div>
                        {if quotes.is_empty() {html!{<p class="evidence-note">{tr(language,"该来源没有可展示的原句。","This source has no original wording to display.")}</p>}}
                        else {html!{for quotes.iter().map(|quote| html!{<blockquote>{*quote}</blockquote>})}}}
                    </li>}
                })}</ol>
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn simple_retention_summary_uses_recorded_factors_without_inventing_ability() {
        let metadata = json!({"importance":{"continuity":2,"goal_relevance":0,"novel_change":9,"explicit_request":1},"age":8});
        assert_eq!(
            retention_reasons("en", &metadata),
            vec!["Continuity", "Explicit request"]
        );
        assert!(retention_reasons("zh", &Value::Null).is_empty());
    }

    #[test]
    fn explicitly_saved_preference_explains_the_choice_without_inventing_scores() {
        let mut value = json!({"memory":{"kind":"help_preference","source_type":"learner_report"},
            "sources":[{"content":{"confirmation":"explicit_save_action"}}],"education_metadata":null});
        for (language, category, reason) in [
            (
                "zh",
                "明确帮助偏好",
                "你明确选择了在以后活动中使用这个讲解偏好。",
            ),
            (
                "en",
                "Explicit help preference",
                "You explicitly chose to use this explanation preference in future activities.",
            ),
        ] {
            let rendered = format!("{:?}", render_evidence(language, "plain", &value));
            assert!(rendered.contains(category) && rendered.contains(reason));
            assert!(!rendered.contains("/3"));
        }
        value["education_metadata"] =
            json!({"kind":"help_preference","importance":{"explicit_request":3}});
        assert!(explicit_preference_reason("en", &value).is_none());
        assert_eq!(
            retention_reasons("en", &value["education_metadata"]),
            vec!["Explicit request"]
        );
        value["education_metadata"] = Value::Null;
        value["memory"]["kind"] = json!("reported_progress");
        assert!(explicit_preference_reason("en", &value).is_none());
        value["memory"]["kind"] = json!("help_preference");
        value["sources"][0]["content"] = json!({"text":"An ordinary chat preference."});
        assert!(explicit_preference_reason("en", &value).is_none());
    }

    fn context() -> EvidenceContext {
        EvidenceContext {
            learner_id: "learner-a".into(),
            learner_epoch: 0,
            namespace: "real".into(),
            enabled: true,
            memory: json!({"memory_id":"memory-a","learner_id":"learner-a","status":"active","content":"I plan to test again.","updated_at":"2026-11-04","source_event_ids":["event-a"]}),
        }
    }
    fn response(context: &EvidenceContext) -> ApiResult {
        Ok(
            json!({"memory":context.memory,"namespace":context.namespace,"sources":[],"education_metadata":null}),
        )
    }
    #[test]
    fn late_evidence_cannot_cross_profiles_revisions_or_mutations() {
        for change in 0..4 {
            let original = context();
            let mut state = EvidenceState::new(original.clone());
            state.toggle();
            let revision = state.revision;
            let mut next = original.clone();
            match change {
                0 => {
                    next.learner_id = "learner-b".into();
                    next.memory["learner_id"] = json!("learner-b");
                }
                1 => next.memory["content"] = json!("Corrected: I have not tested it."),
                2 => next.enabled = false, // editing/deleting/profile mutation starts
                _ => next.namespace = "simulator".into(),
            }
            state.bind(next);
            state.toggle();
            assert!(!state.receive(revision, response(&original)));
            assert!(state.value.is_none());
        }
    }
    #[test]
    fn epoch_change_clears_loaded_comparison_even_when_memory_row_is_unchanged() {
        let original = context();
        let mut state = EvidenceState::new(original.clone());
        state.toggle();
        let old_revision = state.revision;
        let mut old_response = response(&original).unwrap();
        old_response["comparison_context"] =
            json!({"status":"complete","items":[{"question":"Old sibling-source comparison"}]});
        assert!(state.receive(old_revision, Ok(old_response.clone())));
        assert!(state.value.is_some());

        let mut next = original.clone();
        next.learner_epoch += 1;
        assert_eq!(next.memory, original.memory);
        state.bind(next);
        assert!(state.value.is_none());
        assert!(!state.open);
        state.toggle();
        assert!(!state.receive(old_revision, Ok(old_response)));
        assert!(state.value.is_none());
    }
    #[test]
    fn collapse_reopen_discards_inflight_and_already_shown_details() {
        let original = context();
        let mut state = EvidenceState::new(original.clone());
        assert!(!state.needs_request());
        state.toggle();
        let old = state.revision;
        state.toggle();
        state.toggle();
        assert!(!state.receive(old, response(&original)));
        assert!(state.receive(state.revision, response(&original)));
        assert!(state.value.is_some());
        state.toggle();
        assert!(state.value.is_none());
        assert!(!state.needs_request());
    }
    #[test]
    fn mismatching_server_snapshot_is_unavailable_not_old_evidence() {
        let original = context();
        let mut state = EvidenceState::new(original.clone());
        state.toggle();
        let mut changed = original;
        changed.memory["source_event_ids"] = json!(["replacement-event"]);
        assert!(state.receive(state.revision, response(&changed)));
        assert!(state.unavailable);
        assert!(state.value.is_none());
    }
    #[test]
    fn quote_projection_keeps_verbatim_text_and_omits_internal_fields() {
        let content = json!({"text":"I plan to try again.\n还没完成。","report":"  Second observation.  ","correction":{"text":"not a source string"},"event_id":"opaque-id","lease":"secret-lease","internal_prompt":"hidden-prompt","age":8});
        assert_eq!(
            original_text(&content),
            vec![
                "I plan to try again.\n还没完成。",
                "  Second observation.  "
            ]
        );
        assert_eq!(
            date(&json!("2026-11-04T12:32:00+00:00")),
            Some("2026-11-04")
        );
        assert_eq!(date(&json!("internal-id")), None);
    }
    #[test]
    fn labels_preserve_simulation_plans_and_unknown_support() {
        assert_eq!(
            source_grade("simulator", &json!("operator_confirmed")),
            "simulated"
        );
        assert_eq!(
            source_label("en", "operator_confirmed"),
            "Human-confirmed report"
        );
        assert_eq!(
            kind_label("en", "accepted_pending_experiment"),
            "Pending experiment plan"
        );
        assert_eq!(event_label("zh", "plan"), "计划（尚未证明完成）");
        assert_eq!(kind_label("zh", "context_constraint"), "任务条件与推断边界");
        assert_eq!(
            kind_label("en", "context_constraint"),
            "Task conditions and inference limits"
        );
        assert_eq!(support_label("en", "age-8"), "Not recorded");
        assert_eq!(support_label("zh", "unknown"), "未记录");
    }
    #[test]
    fn evidence_fixture_renders_quotes_without_opaque_source_details() {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/memory-evidence-20261104.json"))
                .unwrap();
        for record in fixture["records"].as_array().unwrap() {
            let value = &record["response"];
            let context = EvidenceContext {
                learner_id: value["memory"]["learner_id"].as_str().unwrap().into(),
                learner_epoch: 0,
                namespace: value["namespace"].as_str().unwrap().into(),
                memory: value["memory"].clone(),
                enabled: true,
            };
            let mut state = EvidenceState::new(context);
            state.toggle();
            assert!(state.receive(state.revision, Ok(value.clone())));
            for language in ["zh", "en"] {
                for reading_level in ["plain", "standard", "technical"] {
                    let rendered = format!("{:?}", render_evidence(language, reading_level, value));
                    assert!(rendered.contains(record["expected_original"].as_str().unwrap()));
                    assert!(!rendered.contains(value["memory"]["memory_id"].as_str().unwrap()));
                    assert!(!rendered.contains(value["sources"][0]["event_id"].as_str().unwrap()));
                    assert!(!rendered.contains("fixture-only-hidden"));
                    assert!(!rendered.contains("internal_prompt"));
                }
            }
        }
    }
}
