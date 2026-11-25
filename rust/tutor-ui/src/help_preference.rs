//! An explicitly saved preference, separate from an ordinary chat instruction.
use crate::{
    api::{api, ApiResult},
    view::icon,
};
use serde_json::{json, Value};
use web_sys::HtmlTextAreaElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub learner_id: String,
    pub namespace: String,
    pub session_id: String,
    pub epoch: i64,
    pub generation: i64,
    pub ui_language: String,
    pub enabled: bool,
    pub on_saved: Callback<()>,
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    learner: String,
    namespace: String,
    session: String,
    epoch: i64,
    generation: i64,
}
impl Binding {
    fn from_props(props: &Props) -> Self {
        Self {
            learner: props.learner_id.clone(),
            namespace: props.namespace.clone(),
            session: props.session_id.clone(),
            epoch: props.epoch,
            generation: props.generation,
        }
    }
    fn same_activity(&self, other: &Self) -> bool {
        self.learner == other.learner
            && self.namespace == other.namespace
            && self.session == other.session
            && self.generation == other.generation
    }
    fn valid(&self) -> bool {
        !self.learner.is_empty()
            && !self.session.is_empty()
            && matches!(self.namespace.as_str(), "real" | "simulator")
            && self.epoch >= 0
            && self.generation >= 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Submission {
    binding: Binding,
    request_id: String,
    content: String,
}
impl Submission {
    fn path(&self) -> String {
        format!(
            "/api/learners/{}/sessions/{}/help-preference",
            self.binding.learner, self.binding.session
        )
    }
    fn body(&self) -> Value {
        json!({"content":self.content,"request_id":self.request_id,
            "expected_epoch":self.binding.epoch,"expected_generation":self.binding.generation})
    }
}

#[derive(Debug, PartialEq)]
enum Received {
    Ignored,
    Updated,
    Saved,
}

struct FormState {
    binding: Binding,
    draft: String,
    pending: Option<Submission>,
    retry: Option<Submission>,
    saved: bool,
    saved_epoch: Option<i64>,
    error: Option<String>,
}
impl FormState {
    fn new(binding: Binding) -> Self {
        Self {
            binding,
            draft: String::new(),
            pending: None,
            retry: None,
            saved: false,
            saved_epoch: None,
            error: None,
        }
    }
    fn clear_draft(&mut self) {
        self.draft.clear();
        self.retry = None;
        self.saved = false;
        self.saved_epoch = None;
        self.error = None;
    }
    fn bind(&mut self, next: Binding) {
        if self.binding == next {
            return;
        }
        let same_activity = self.binding.same_activity(&next);
        let own_refresh = same_activity && self.saved && self.saved_epoch == Some(next.epoch);
        let unresolved = same_activity && self.retry.is_some();
        if !own_refresh && !unresolved {
            self.clear_draft();
        }
        // Keep the physical request slot until its callback returns. Its owner
        // is immutable even if the user has already selected another profile.
        self.binding = next;
    }
    fn edit(&mut self, text: String) {
        if self.pending.is_some() || text == self.draft {
            return;
        }
        self.draft = text;
        self.retry = None;
        self.saved = false;
        self.saved_epoch = None;
        self.error = None;
    }
    fn valid_draft(&self) -> bool {
        !self.draft.trim().is_empty() && self.draft.chars().count() <= 800
    }
    fn begin(&mut self, key: impl FnOnce() -> String) -> Option<Submission> {
        if self.pending.is_some() || !self.binding.valid() || !self.valid_draft() {
            return None;
        }
        let submission = self.retry.clone().unwrap_or_else(|| Submission {
            binding: self.binding.clone(),
            request_id: key(),
            content: self.draft.clone(),
        });
        self.retry = Some(submission.clone());
        self.pending = Some(submission.clone());
        self.saved = false;
        self.saved_epoch = None;
        self.error = None;
        Some(submission)
    }
    fn receive(&mut self, request: &Submission, result: ApiResult) -> Received {
        if self.pending.as_ref() != Some(request) {
            return Received::Ignored;
        }
        self.pending = None;
        if !self.binding.same_activity(&request.binding) || self.retry.as_ref() != Some(request) {
            return Received::Updated;
        }
        match result {
            Ok(value) => {
                let saved_epoch = value["learner_epoch"].as_i64();
                let epoch_matches = self.binding.epoch == request.binding.epoch
                    || saved_epoch == Some(self.binding.epoch);
                if !epoch_matches {
                    self.clear_draft();
                    return Received::Updated;
                }
                let valid = value["learner_id"] == request.binding.learner
                    && value["session_id"] == request.binding.session
                    && value["memory_id"].as_str().is_some_and(|id| !id.is_empty())
                    && value["event_id"].as_str().is_some_and(|id| !id.is_empty())
                    && value["replayed"].is_boolean();
                if valid {
                    self.draft.clear();
                    self.retry = None;
                    self.saved = true;
                    self.saved_epoch = saved_epoch;
                    self.error = None;
                    Received::Saved
                } else {
                    self.error = Some(String::new());
                    Received::Updated
                }
            }
            Err(detail) => {
                self.error = Some(detail);
                Received::Updated
            }
        }
    }
}

pub enum Msg {
    Edit(String),
    Save,
    Clear,
    Result(Submission, ApiResult),
}
pub struct HelpPreference {
    form: FormState,
}
impl Component for HelpPreference {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            form: FormState::new(Binding::from_props(ctx.props())),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Props) -> bool {
        self.form.bind(Binding::from_props(ctx.props()));
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        self.form.bind(Binding::from_props(ctx.props()));
        match msg {
            Msg::Edit(text) => self.form.edit(text),
            Msg::Clear => {
                if self.form.pending.is_none() {
                    self.form.clear_draft();
                }
            }
            Msg::Save => {
                if !ctx.props().enabled {
                    return false;
                }
                let Some(request) = self.form.begin(|| {
                    format!(
                        "help-pref-{}-{:016x}",
                        js_sys::Date::now() as u64,
                        (js_sys::Math::random() * u64::MAX as f64) as u64
                    )
                }) else {
                    return false;
                };
                let path = request.path();
                let body = request.body();
                ctx.link().send_future(async move {
                    Msg::Result(request, api(&path, "POST", Some(body)).await)
                });
            }
            Msg::Result(request, result) => match self.form.receive(&request, result) {
                Received::Ignored => return false,
                Received::Saved => ctx.props().on_saved.emit(()),
                Received::Updated => (),
            },
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let en = ctx.props().ui_language == "en";
        let sending = self.form.pending.is_some();
        let disabled = !ctx.props().enabled || sending;
        let length = self.form.draft.chars().count();
        html! {
            <details class="help-preference">
                <summary><span>{icon("sparkle")}{if en {"How you'd like Xiao Yu to explain"} else {"希望小宇记住的讲解偏好"}}</span>{icon("chevron")}</summary>
                <div class="help-preference-content">
                    <p>{if en {"Save an explanation preference to use in future activities too. Saving replaces your previous lasting explanation preference."} else {"保存后，以后的活动也使用这个讲解偏好，并替换此前的长期讲解偏好。"}}</p>
                    <label class="help-preference-label">{if en {"Your preference, in your own words"} else {"用你自己的话写下偏好"}}
                        <textarea value={self.form.draft.clone()} disabled={disabled}
                            placeholder={if en {"For example: explain one step at a time, then wait for my answer."} else {"例如：一次只讲一个步骤，等我回答后再继续。"}}
                            oninput={ctx.link().callback(|event: InputEvent| Msg::Edit(event.target_unchecked_into::<HtmlTextAreaElement>().value()))}/>
                    </label>
                    <div class="help-preference-meta"><small>{if en {"For this activity only? Just say it in the chat."} else {"只想本次这样讲？直接在对话里告诉小宇就好。"}}</small><span class={classes!((length > 800).then_some("preference-too-long"))}>{format!("{length}/800")}</span></div>
                    {if !ctx.props().enabled {html!{<p class="help-preference-note">{if en {"Open a learning activity and wait for the current operation to finish before saving."} else {"请先开启学习活动，待当前操作结束后再保存。"}}</p>}} else {Html::default()}}
                    {if let Some(detail) = &self.form.error {html!{<div class="help-preference-error" role="status"><p>{if en {"Saving is not confirmed. Retry the same submission, or refresh the memories below to check."} else {"保存结果尚未确认。可以重试同一次提交，或刷新下方记忆核对。"}}</p>{if !detail.is_empty(){html!{<details><summary>{if en {"View service details"} else {"查看服务详情"}}</summary><p>{detail}</p></details>}}else{Html::default()}}</div>}} else {Html::default()}}
                    {if self.form.saved {html!{<p class="help-preference-saved" role="status">{icon("check")}{if en {"Saved for future activities. You can correct or forget it in the memories below."} else {"已记住，以后活动也会使用。你可以在下方记忆中更正或忘记。"}}</p>}} else {Html::default()}}
                    <div class="help-preference-actions"><button class="primary" disabled={disabled || !self.form.valid_draft() || !self.form.binding.valid()} onclick={ctx.link().callback(|_| Msg::Save)}>{if sending {if en {"Saving…"} else {"正在保存…"}} else if self.form.error.is_some() {if en {"Retry saving"} else {"重试保存"}} else {if en {"Remember this preference"} else {"记住这个偏好"}}}</button>{if !self.form.draft.is_empty(){html!{<button class="text-button" disabled={sending} onclick={ctx.link().callback(|_|Msg::Clear)}>{if en {"Clear draft"} else {"清空草稿"}}</button>}}else{Html::default()}}</div>
                </div>
            </details>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding {
            learner: "a".into(),
            namespace: "simulator".into(),
            session: "s".into(),
            epoch: 2,
            generation: 4,
        }
    }
    fn state() -> FormState {
        FormState::new(binding())
    }
    fn saved(request: &Submission) -> ApiResult {
        Ok(
            json!({"learner_id":request.binding.learner,"session_id":request.binding.session,"memory_id":"m","event_id":"e","replayed":false,"learner_epoch":request.binding.epoch+1}),
        )
    }
    #[test]
    fn explicit_submission_preserves_original_and_enforces_length() {
        let mut state = state();
        state.edit("  ".into());
        assert!(state.begin(|| "a".into()).is_none());
        state.edit("字".repeat(801));
        assert!(state.begin(|| "a".into()).is_none());
        state.edit("字".repeat(800));
        assert!(state.valid_draft());
        let original = "  一次一步。\nPlease wait.  ";
        state.edit(original.into());
        let request = state.begin(|| "explicit".into()).unwrap();
        assert_eq!(
            request.body(),
            json!({"content":original,"request_id":"explicit","expected_epoch":2,"expected_generation":4})
        );
        assert_eq!(request.path(), "/api/learners/a/sessions/s/help-preference");
    }
    #[test]
    fn pending_request_blocks_edit_and_double_submit() {
        let mut state = state();
        state.edit("Wait for my answer".into());
        let request = state.begin(|| "one".into()).unwrap();
        state.edit("replacement".into());
        assert_eq!(state.draft, request.content);
        assert!(state
            .begin(|| panic!("must not allocate a second key"))
            .is_none());
    }
    #[test]
    fn uncertain_result_reuses_original_key_and_body_until_edited() {
        let mut state = state();
        state.edit("one step".into());
        let first = state.begin(|| "one".into()).unwrap();
        assert_eq!(
            state.receive(&first, Err("timeout".into())),
            Received::Updated
        );
        let retry = state.begin(|| panic!("retry must reuse its key")).unwrap();
        assert_eq!(retry, first);
        state.receive(&retry, Err("timeout".into()));
        state.edit("two examples".into());
        assert_eq!(
            state.begin(|| "new-content".into()).unwrap().request_id,
            "new-content"
        );
    }
    #[test]
    fn old_activity_responses_never_set_saved_or_invoke_success() {
        for change in 0..4 {
            let mut state = state();
            state.edit("old text".into());
            let request = state.begin(|| "old".into()).unwrap();
            let mut next = binding();
            match change {
                0 => next.learner = "b".into(),
                1 => next.namespace = "real".into(),
                2 => next.session = "other".into(),
                _ => next.generation += 1,
            }
            state.bind(next);
            assert!(state.draft.is_empty());
            assert_eq!(state.receive(&request, saved(&request)), Received::Updated);
            assert!(!state.saved);
            assert!(state.pending.is_none());
            assert!(state.error.is_none());
        }
    }
    #[test]
    fn own_epoch_refresh_can_arrive_before_or_after_save_response() {
        for poll_first in [false, true] {
            let mut state = state();
            state.edit("one step".into());
            let request = state.begin(|| "save".into()).unwrap();
            let mut next = binding();
            next.epoch += 1;
            if poll_first {
                state.bind(next.clone());
            }
            assert_eq!(state.receive(&request, saved(&request)), Received::Saved);
            state.bind(next);
            assert!(state.saved);
            assert!(state.draft.is_empty());
            assert!(state.error.is_none());
        }
    }
    #[test]
    fn later_epoch_change_or_wrong_owner_response_cannot_confirm_old_save() {
        let mut form = state();
        form.edit("one step".into());
        let request = form.begin(|| "save".into()).unwrap();
        let mut next = binding();
        next.epoch += 2;
        form.bind(next);
        assert_eq!(form.receive(&request, saved(&request)), Received::Updated);
        assert!(!form.saved);
        let mut form = state();
        form.edit("one step".into());
        let request = form.begin(|| "second".into()).unwrap();
        let mut result = saved(&request).unwrap();
        result["learner_id"] = json!("someone-else");
        assert_eq!(form.receive(&request, Ok(result)), Received::Updated);
        assert!(!form.saved);
        assert_eq!(form.draft, "one step");
    }
    #[test]
    fn lost_success_retries_same_receipt_even_after_its_epoch_refresh() {
        let mut form = state();
        form.edit("one step".into());
        let request = form.begin(|| "save".into()).unwrap();
        form.receive(&request, Err("lost response".into()));
        let mut next = binding();
        next.epoch += 1;
        form.bind(next);
        let retry = form
            .begin(|| panic!("same unconfirmed submission"))
            .unwrap();
        assert_eq!(retry, request);
        assert_eq!(form.receive(&retry, saved(&retry)), Received::Saved);
    }
}
