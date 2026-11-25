//! Explicit comparison notes. No model, memory-promotion or completion actions.
use crate::{
    api::{api, ApiResult},
    comparison_state::{
        ComparisonGuard, ComparisonOwner, ComparisonState, ConditionDraft, DraftError, Submission,
    },
    evidence_labels::source_label,
    view::SelectControl,
};
use web_sys::{HtmlElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}
fn error_label(language: &str, error: DraftError) -> &'static str {
    let (zh, en) = match error {
        DraftError::Question => (
            "请填写比较问题（最多 200 字）。",
            "Enter a comparison question (up to 200 characters).",
        ),
        DraftError::Variable => (
            "请填写改变的条件（最多 200 字）。",
            "Describe what changes (up to 200 characters).",
        ),
        DraftError::Condition => (
            "请填写每项共同条件，并选择是否已报告相同。",
            "Name each shared condition and explicitly select its status.",
        ),
        DraftError::Setting => (
            "请分别填写 A、B 的设置（各最多 200 字）。",
            "Enter both A and B settings (up to 200 characters each).",
        ),
        DraftError::Status => (
            "请分别选择 A、B 是已有观察还是尚未测试。",
            "Choose whether A and B have a reported observation or have not been tested.",
        ),
        DraftError::Result => (
            "已有观察需要填写原始结果（最多 1000 字）。",
            "A reported observation needs its original result (up to 1,000 characters).",
        ),
        DraftError::Count => (
            "次数留空表示未知；填写时须为 1–10000 的整数。",
            "Leave the count blank if unknown, or enter a whole number from 1 to 10,000.",
        ),
        DraftError::PendingResult => (
            "尚未测试不能带有结果或次数。请明确清空这些内容，或改回已有观察。",
            "An untested setup cannot have a result or count. Clear these fields explicitly, or restore reported observation.",
        ),
        DraftError::Plan => (
            "请填写待测计划后再接受；计划最多 1000 字。",
            "Enter the pending plan before accepting it (up to 1,000 characters).",
        ),
        DraftError::Source => (
            "请选择当前档案允许的报告来源。",
            "Choose a report source allowed for this profile.",
        ),
    };
    tr(language, zh, en)
}

#[derive(Properties, PartialEq)]
pub struct ComparisonProps {
    pub owner: ComparisonOwner,
    pub ui_language: String,
    pub reading_level: String,
    pub writable: bool,
    pub enabled: bool,
    pub save_enabled: bool,
    pub generation: u64,
    pub focus_revision: u64,
    pub on_guard: Callback<ComparisonGuard>,
    pub on_settled: Callback<(String, String)>,
}
#[derive(Clone)]
pub enum Field {
    Question,
    Variable,
    ConditionName(usize),
    ConditionStatus(usize),
    Setting(usize),
    Status(usize),
    Result(usize),
    Count(usize),
    Plan,
    Source,
}
pub enum Msg {
    Load,
    Loaded(ComparisonOwner, u64, u64, ApiResult),
    Save,
    Saved(Submission, ApiResult),
    Edit(Field, String),
    Accepted(bool),
    AddCondition,
    RemoveCondition(usize),
    Clear,
    Toggle,
}
pub struct ExperimentComparison {
    state: ComparisonState,
    loading: bool,
    saving: bool,
    needs_load: bool,
    open: bool,
    notice: Option<&'static str>,
    error: Option<DraftError>,
    list_failed: bool,
    save_failed: bool,
    node: NodeRef,
    focus_pending: bool,
}
impl ExperimentComparison {
    fn notify(&self, ctx: &Context<Self>) {
        ctx.props().on_guard.emit(ComparisonGuard {
            owner: self.state.owner.clone(),
            dirty: self.state.draft.dirty(),
            saving: self.saving,
        });
    }
    fn queue_load(&mut self, ctx: &Context<Self>) {
        if !self.needs_load || self.loading || !self.state.owner.exists() || !ctx.props().enabled {
            return;
        }
        self.needs_load = false;
        self.loading = true;
        let owner = self.state.owner.clone();
        let epoch = self.state.epoch;
        let revision = self.state.list_revision;
        ctx.link().send_future(async move {
            let result = api(&owner.path(), "GET", None).await;
            Msg::Loaded(owner, epoch, revision, result)
        });
    }
    fn editable(&self, ctx: &Context<Self>) -> bool {
        ctx.props().writable && ctx.props().enabled
    }
    fn input(&self, ctx: &Context<Self>, field: Field) -> Callback<InputEvent> {
        ctx.link().callback(move |event: InputEvent| {
            Msg::Edit(
                field.clone(),
                event.target_unchecked_into::<HtmlInputElement>().value(),
            )
        })
    }
    fn textarea(&self, ctx: &Context<Self>, field: Field) -> Callback<InputEvent> {
        ctx.link().callback(move |event: InputEvent| {
            Msg::Edit(
                field.clone(),
                event.target_unchecked_into::<HtmlTextAreaElement>().value(),
            )
        })
    }
    fn select(&self, ctx: &Context<Self>, field: Field) -> Callback<Event> {
        ctx.link().callback(move |event: Event| {
            Msg::Edit(
                field.clone(),
                event.target_unchecked_into::<HtmlSelectElement>().value(),
            )
        })
    }
}
impl Component for ExperimentComparison {
    type Message = Msg;
    type Properties = ComparisonProps;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Load);
        Self {
            state: ComparisonState::new(ctx.props().owner.clone()),
            loading: false,
            saving: false,
            needs_load: true,
            open: false,
            notice: None,
            error: None,
            list_failed: false,
            save_failed: false,
            node: NodeRef::default(),
            focus_pending: false,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if self.state.bind(ctx.props().owner.clone()) {
            // Preserve physical request slots until their callbacks return.
            self.needs_load = true;
            self.open = false;
            self.notice = None;
            self.error = None;
            self.list_failed = false;
            self.save_failed = false;
            self.notify(ctx);
        } else if old.generation != ctx.props().generation {
            self.state.invalidate_list(true);
            self.needs_load = true;
        }
        if old.focus_revision != ctx.props().focus_revision {
            self.open = true;
            self.focus_pending = true;
        }
        self.queue_load(ctx);
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Load => {
                self.needs_load = true;
                self.queue_load(ctx);
            }
            Msg::Loaded(owner, epoch, revision, result) => {
                self.loading = false;
                if owner == self.state.owner
                    && epoch == self.state.epoch
                    && revision == self.state.list_revision
                {
                    self.list_failed = match result {
                        Ok(value) => !self.state.loaded(&owner, epoch, revision, &value),
                        Err(_) => true,
                    };
                }
                self.queue_load(ctx);
            }
            Msg::Save => {
                if self.saving || !self.editable(ctx) || !ctx.props().save_enabled {
                    return false;
                }
                let submission = match self.state.submission(|| {
                    format!(
                        "comparison-{}-{:016x}",
                        js_sys::Date::now() as u64,
                        (js_sys::Math::random() * u64::MAX as f64) as u64
                    )
                }) {
                    Ok(submission) => submission,
                    Err(error) => {
                        self.error = Some(error);
                        self.focus_pending = true;
                        return true;
                    }
                };
                self.saving = true;
                self.error = None;
                self.save_failed = false;
                self.notice = None;
                self.notify(ctx);
                // Ignore any older list response even if it returns after save.
                self.state.invalidate_list(false);
                ctx.link().send_future(async move {
                    let result = api(
                        &submission.owner.path(),
                        "POST",
                        Some(submission.payload.clone()),
                    )
                    .await;
                    Msg::Saved(submission, result)
                });
            }
            Msg::Saved(submission, result) => {
                self.saving = false;
                if submission.owner == self.state.owner && submission.epoch == self.state.epoch {
                    self.save_failed = match result {
                        Ok(value) => !self.state.saved(&submission, &value),
                        Err(_) => true,
                    };
                    if !self.save_failed {
                        self.notice = Some(if self.state.draft.dirty() {
                            "newer"
                        } else {
                            "saved"
                        });
                    }
                    // A lost response can still have committed. Refresh the
                    // parent generation before releasing display/speech gates.
                    ctx.props().on_settled.emit((
                        self.state.owner.learner.clone(),
                        self.state.owner.session.clone(),
                    ));
                    self.state.invalidate_list(false);
                    self.needs_load = true;
                }
                self.notify(ctx);
                self.queue_load(ctx);
            }
            Msg::Edit(field, value) => {
                if !self.editable(ctx) {
                    return false;
                }
                self.state.edit(move |draft| match field {
                    Field::Question => draft.question = value,
                    Field::Variable => draft.changed_variable = value,
                    Field::ConditionName(i) => {
                        if let Some(c) = draft.conditions.get_mut(i) {
                            c.name = value;
                        }
                    }
                    Field::ConditionStatus(i) => {
                        if let Some(c) = draft.conditions.get_mut(i) {
                            c.status = value;
                        }
                    }
                    Field::Setting(i) => draft.trials[i].setting = value,
                    Field::Status(i) => draft.trials[i].status = value,
                    Field::Result(i) => draft.trials[i].result = value,
                    Field::Count(i) => draft.trials[i].count = value,
                    Field::Plan => draft.plan = value,
                    Field::Source => draft.source = value,
                });
                self.error = None;
                self.notice = None;
                self.notify(ctx);
            }
            Msg::Accepted(value) => {
                if !self.editable(ctx) {
                    return false;
                }
                self.state.edit(|d| d.accepted = value);
                self.error = None;
                self.notice = None;
                self.notify(ctx);
            }
            Msg::AddCondition => {
                if !self.editable(ctx) || self.state.draft.conditions.len() >= 8 {
                    return false;
                }
                self.state
                    .edit(|d| d.conditions.push(ConditionDraft::default()));
                self.notify(ctx);
            }
            Msg::RemoveCondition(index) => {
                if !self.editable(ctx)
                    || self.state.draft.conditions.len() <= 1
                    || index >= self.state.draft.conditions.len()
                {
                    return false;
                }
                self.state.edit(|d| {
                    d.conditions.remove(index);
                });
                self.notify(ctx);
            }
            Msg::Clear => {
                if self.saving {
                    return false;
                }
                self.state.clear();
                self.error = None;
                self.notice = None;
                self.save_failed = false;
                self.notify(ctx);
            }
            Msg::Toggle => {
                self.open = !self.open;
            }
        }
        true
    }
    fn rendered(&mut self, _ctx: &Context<Self>, _first: bool) {
        if self.focus_pending {
            self.focus_pending = false;
            if let Some(node) = self.node.cast::<HtmlElement>() {
                // A navigation guard can request this form while its enclosing
                // journal is collapsed. Reveal ancestors only for this explicit
                // focus request; ordinary renders preserve the user's choice.
                let mut ancestor = node.parent_element();
                while let Some(element) = ancestor {
                    if element.tag_name().eq_ignore_ascii_case("details") {
                        let _ = element.set_attribute("open", "");
                    }
                    ancestor = element.parent_element();
                }
                node.scroll_into_view();
                let _ = node.focus();
            }
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        if !self.state.owner.exists() {
            return Html::default();
        }
        let language = ctx.props().ui_language.as_str();
        let draft = &self.state.draft;
        let disabled = !self.editable(ctx);
        let variable = if ctx.props().reading_level == "technical" {
            tr(language, "比较变量", "Variable being compared")
        } else {
            tr(language, "改变什么", "What changes")
        };
        html! {<section class="experiment-comparison" ref={self.node.clone()} tabindex="-1" aria-label={tr(language,"比较两种做法","Compare two setups")}>
            <div class="comparison-heading"><h3>{tr(language,"比较两种做法","Compare two setups")}</h3><button class="text-button" aria-expanded={(self.open||draft.dirty()).to_string()} onclick={ctx.link().callback(|_|Msg::Toggle)}>{if self.open||draft.dirty(){tr(language,"记录表","Record form")}else{tr(language,"记录比较","Record comparison")}}</button></div>
            <p class="fine-print">{tr(language,"记录观察与待测项。保存不代表实验完成，也不会自动提问或写入长期记忆。","Record observations and untested ideas. Saving does not mark an experiment complete, send a question, or add long-term memory.")}</p>
            {if self.open||draft.dirty(){html!{<div class="comparison-form">
                {if !ctx.props().writable{html!{<p>{tr(language,"本次活动已结束，只能回看记录。","This session has ended. Saved records are read-only.")}</p>}}else{Html::default()}}
                <label>{tr(language,"想比较的问题","Comparison question")}<input value={draft.question.clone()} maxlength="200" disabled={disabled} oninput={self.input(ctx,Field::Question)}/></label>
                <label>{variable}<input value={draft.changed_variable.clone()} maxlength="200" disabled={disabled} oninput={self.input(ctx,Field::Variable)}/></label>
                <fieldset><legend>{tr(language,"其他条件","Other conditions")}</legend><p class="fine-print">{tr(language,"例如同一小车、速度、地面；不确定时选“尚未核对”。","For example: the same rover, speed or floor. Choose “Not checked” when unsure.")}</p>
                    {for draft.conditions.iter().enumerate().map(|(i,condition)|html!{<div class="comparison-condition">
                        <label>{tr(language,"条件","Condition")}<input value={condition.name.clone()} maxlength="200" disabled={disabled} oninput={self.input(ctx,Field::ConditionName(i))}/></label>
                        <label>{tr(language,"是否相同","Same condition?")}<SelectControl value={condition.status.clone()} disabled={disabled} onchange={self.select(ctx,Field::ConditionStatus(i))} label={tr(language,"共同条件状态","Shared-condition status")}><option value="">{tr(language,"请选择","Choose")}</option><option value="reported_same">{tr(language,"报告为相同","Reported as the same")}</option><option value="not_confirmed">{tr(language,"尚未核对","Not checked")}</option></SelectControl></label>
                        <button class="text-button" disabled={disabled||draft.conditions.len()<=1} onclick={ctx.link().callback(move |_|Msg::RemoveCondition(i))}>{tr(language,"移除条件","Remove condition")}</button>
                    </div>})}
                    <button class="text-button" disabled={disabled||draft.conditions.len()>=8} onclick={ctx.link().callback(|_|Msg::AddCondition)}>{tr(language,"添加条件","Add condition")}</button>
                </fieldset>
                <div class="comparison-trials">{for draft.trials.iter().enumerate().map(|(i,trial)|html!{<fieldset class="comparison-trial"><legend>{if i==0{"A"}else{"B"}}</legend>
                    <label>{tr(language,"设置 / 做法","Setting / setup")}<input value={trial.setting.clone()} maxlength="200" disabled={disabled} oninput={self.input(ctx,Field::Setting(i))}/></label>
                    <label>{tr(language,"目前状态","Current status")}<SelectControl value={trial.status.clone()} disabled={disabled} onchange={self.select(ctx,Field::Status(i))} label={tr(language,"试验状态","Trial status")}><option value="">{tr(language,"请选择","Choose")}</option><option value="reported">{tr(language,"已有观察（报告）","Reported observation")}</option><option value="not_tested">{tr(language,"尚未测试","Not tested")}</option></SelectControl></label>
                    <label>{tr(language,"原始观察","Original observation")}<textarea value={trial.result.clone()} maxlength="1000" rows="3" disabled={disabled} oninput={self.textarea(ctx,Field::Result(i))}/></label>
                    <label>{tr(language,"次数（未知可留空）","Trial count (leave blank if unknown)")}<input value={trial.count.clone()} inputmode="numeric" maxlength="5" disabled={disabled} oninput={self.input(ctx,Field::Count(i))}/></label>
                    {if trial.status=="not_tested"{html!{<small>{tr(language,"尚未测试时，观察和次数需留空。","Leave observation and count empty for an untested setup.")}</small>}}else{Html::default()}}
                </fieldset>})}</div>
                <label>{tr(language,"下一项待测计划（可选）","Next plan to test (optional)")}<textarea value={draft.plan.clone()} maxlength="1000" rows="3" disabled={disabled} oninput={self.textarea(ctx,Field::Plan)}/></label>
                <label class="comparison-check"><input type="checkbox" checked={draft.accepted} disabled={disabled} onchange={ctx.link().callback(|event:Event|Msg::Accepted(event.target_unchecked_into::<HtmlInputElement>().checked()))}/><span>{tr(language,"我明确接受这项待测计划（仍未实施）","I explicitly accept this plan (still pending)")}</span></label>
                {if self.state.owner.simulated{html!{<p>{source_label(language,"simulated")}</p>}}else{html!{<label>{tr(language,"报告来源","Report source")}<SelectControl value={draft.source.clone()} disabled={disabled} onchange={self.select(ctx,Field::Source)} label={tr(language,"报告来源","Report source")}><option value="learner_report">{source_label(language,"learner_report")}</option><option value="operator_confirmed">{source_label(language,"operator_confirmed")}</option></SelectControl></label>}}}
                <p class="fine-print">{tr(language,"人工确认只说明报告经过确认，不等于硬件验证。原始观察保持原文。","Human confirmation applies to the report, not hardware verification. Original observations keep their wording.")}</p>
                {self.error.map(|error|html!{<p class="comparison-error" role="alert">{error_label(language,error)}</p>}).unwrap_or_default()}
                {if self.save_failed{html!{<p class="comparison-error" role="alert">{tr(language,"保存未确认，草稿仍保留。可重试同一内容；查看记录不会清空草稿。","Save was not confirmed; the draft is preserved. You can retry the same content. Reviewing saved records does not clear it.")}</p>}}else{Html::default()}}
                <div class="comparison-actions"><button class="primary" disabled={disabled||self.saving||!ctx.props().save_enabled||!draft.dirty()} onclick={ctx.link().callback(|_|Msg::Save)}>{if self.saving{tr(language,"正在保存…","Saving…")}else{tr(language,"保存比较记录","Save comparison")}}</button><button class="text-button" disabled={self.saving||!draft.dirty()} onclick={ctx.link().callback(|_|Msg::Clear)}>{tr(language,"清空草稿","Clear draft")}</button><small>{if draft.dirty(){tr(language,"尚有未保存内容","Unsaved changes")}else{""}}</small></div>
                <p class="fine-print">{tr(language,"切换档案或活动前，请先保存或清空。草稿只保留在当前页面，刷新会丢失。","Save or clear before changing profiles or sessions. This draft lives on this page; refreshing discards it.")}</p>
            </div>}}else{Html::default()}}
            {self.notice.map(|notice|html!{<p role="status">{if notice=="newer"{tr(language,"提交的版本已保存；之后的编辑仍在草稿中。","The submitted version was saved; your newer edits remain in the draft.")}else{tr(language,"比较记录已保存。","Comparison saved.")}}</p>}).unwrap_or_default()}
            <div class="comparison-heading"><h4>{tr(language,"本次比较记录（最近 10 组）","Session comparisons (latest 10)")}</h4><button class="text-button" disabled={self.loading||self.saving||!ctx.props().enabled} onclick={ctx.link().callback(|_|Msg::Load)}>{tr(language,"刷新记录","Refresh records")}</button></div>
            {if self.list_failed{html!{<p role="status">{tr(language,"暂时无法读取比较记录，请重试。草稿不受影响。","Comparison records could not be loaded. Retry; your draft is unchanged.")}</p>}}else if self.loading&&self.state.items.is_empty(){html!{<p>{tr(language,"正在读取…","Loading…")}</p>}}else if self.state.items.is_empty(){html!{<p class="muted">{tr(language,"还没有已保存的比较记录。","No comparisons have been saved yet.")}</p>}}else{Html::default()}}
            {for self.state.items.iter().map(|item|crate::comparison_record::render_record(language,item,false))}
        </section>}
    }
}
