//! Historical activity selection never changes the current session by itself.
use crate::{
    activity_history_state::{ActivityPages, HistoricalActivity, PAGE_SIZE},
    api::{api, ApiResult},
    model::text,
};
use serde_json::Value;
use yew::prelude::*;

pub(crate) fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}
pub(crate) fn title(activity: &HistoricalActivity, language: &str) -> String {
    if activity.title_stale {
        tr(
            language,
            "历史活动 · 来源已更新，请从列表查看最新目标",
            "Past activity · sources updated; check the list for its latest goal",
        )
        .into()
    } else if activity.goal.trim().is_empty() {
        tr(
            language,
            "未记录目标的活动",
            "Activity with no recorded goal",
        )
        .into()
    } else {
        activity.goal.clone()
    }
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub learner_id: String,
    pub learner_epoch: i64,
    pub current_session: String,
    pub current_finished: bool,
    pub ui_language: String,
    pub busy: bool,
    pub on_view: Callback<HistoricalActivity>,
    pub on_continue: Callback<HistoricalActivity>,
}
pub enum Msg {
    Toggle,
    Fetch,
    Result(u64, ApiResult),
    Older,
    Newer,
    Refresh,
}
pub struct ActivityHistory {
    open: bool,
    pages: ActivityPages,
    pending: Option<u64>,
}
impl Component for ActivityHistory {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        Self {
            open: false,
            pages: ActivityPages::new(ctx.props().learner_id.clone(), ctx.props().learner_epoch),
            pending: None,
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        self.pages
            .bind(&ctx.props().learner_id, ctx.props().learner_epoch);
        if old.current_session != ctx.props().current_session
            || old.current_finished != ctx.props().current_finished
        {
            self.pages.refresh();
        }
        if self.open {
            ctx.link().send_message(Msg::Fetch);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Toggle => {
                self.open = !self.open;
                if self.open {
                    ctx.link().send_message(Msg::Fetch);
                }
            }
            Msg::Fetch => {
                if !self.open || self.pending.is_some() {
                    return false;
                }
                let Some(revision) = self.pages.begin() else {
                    return false;
                };
                let mut path = format!(
                    "/api/learners/{}/sessions?limit={PAGE_SIZE}",
                    self.pages.learner
                );
                if let Some(before) = self.pages.before {
                    path.push_str(&format!("&before={before}"));
                }
                self.pending = Some(revision);
                ctx.link().send_future(async move {
                    Msg::Result(revision, api(&path, "GET", None).await)
                });
            }
            Msg::Result(revision, result) => {
                if self.pending != Some(revision) {
                    return false;
                }
                self.pending = None;
                let changed = self.pages.complete(revision, result);
                if self.open {
                    ctx.link().send_message(Msg::Fetch);
                }
                return changed;
            }
            Msg::Older => {
                if !self.pages.older() {
                    return false;
                }
                ctx.link().send_message(Msg::Fetch);
            }
            Msg::Newer => {
                if !self.pages.newer() {
                    return false;
                }
                ctx.link().send_message(Msg::Fetch);
            }
            Msg::Refresh => {
                if self.pending.is_some() {
                    return false;
                }
                self.pages.refresh();
                ctx.link().send_message(Msg::Fetch);
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let p = ctx.props();
        let lang = &p.ui_language;
        html! {<section class="activity-history" aria-label={tr(lang,"过去的活动","Past activities")}>
            <button class="text-button" aria-expanded={self.open.to_string()} disabled={p.learner_id.is_empty()} onclick={ctx.link().callback(|_|Msg::Toggle)}>{tr(lang,"查看过去的活动","Browse past activities")}</button>
            {if self.open {html!{<>
                <p class="fine-print">{tr(lang,"查看记录不会切换当前活动。只有已结束的活动可以选择续做；续做将新建一次活动。","Viewing records keeps your current activity. Finished activities can be continued as a new activity.")}</p>
                <div class="history-controls"><button class="text-button" disabled={self.pending.is_some()} onclick={ctx.link().callback(|_|Msg::Refresh)}>{tr(lang,"刷新列表","Refresh list")}</button><span>{if lang=="en" {format!("Page {}",self.pages.page)} else {format!("第 {} 页",self.pages.page)}}</span></div>
                {if self.pages.loading {html!{<p role="status">{tr(lang,"正在读取活动…","Loading activities…")}</p>}}else if self.pages.error {html!{<p role="status">{tr(lang,"活动列表暂不可用，请刷新重试。","Activities are unavailable. Refresh to retry.")}</p>}}else if self.pages.loaded&&self.pages.items.is_empty(){html!{<p>{tr(lang,"还没有活动记录。","No activities recorded yet.")}</p>}}else{Html::default()}}
                <ol class="activity-history-list">{for self.pages.items.iter().map(|item| {
                    let view=p.on_view.clone();let view_item=item.clone();let continue_cb=p.on_continue.clone();let continue_item=item.clone();
                    let current=item.session==p.current_session;
                    html!{<li key={item.session.clone()}><strong>{title(item,lang)}</strong><div class="activity-history-meta"><time>{&item.created_at}</time><span>{if current{tr(lang,"当前活动","Current activity")}else if item.finished{tr(lang,"已结束","Finished")}else{tr(lang,"未结束 · 只读查看","Unfinished · read only")}}</span></div>
                        {if !item.continued_from.is_null(){html!{<p class="fine-print">{tr(lang,"续自：","Continued from: ")}{historical_goal(&item.continued_from,lang)}</p>}}else{Html::default()}}
                        <div class="history-controls"><button class="text-button" disabled={p.busy} onclick={Callback::from(move |_|view.emit(view_item.clone()))}>{tr(lang,"查看记录","View records")}</button><button class="secondary" disabled={p.busy||!item.finished} onclick={Callback::from(move |_|continue_cb.emit(continue_item.clone()))}>{tr(lang,"继续这次活动","Continue this activity")}</button></div>
                    </li>}
                })}</ol>
                <div class="history-controls"><button class="text-button" disabled={self.pages.previous.is_empty()||self.pending.is_some()} onclick={ctx.link().callback(|_|Msg::Newer)}>{tr(lang,"较新一页","Newer page")}</button><button class="text-button" disabled={self.pages.next_before.is_none()||self.pending.is_some()} onclick={ctx.link().callback(|_|Msg::Older)}>{tr(lang,"更早一页","Older page")}</button></div>
            </>}}else{Html::default()}}
        </section>}
    }
}
fn historical_goal(source: &Value, lang: &str) -> String {
    let goal = text(&source["goal"]);
    if goal.trim().is_empty() {
        tr(
            lang,
            "先前活动（目标来源不可核对）",
            "Earlier activity (goal source unavailable)",
        )
        .into()
    } else {
        goal
    }
}

#[derive(Properties, PartialEq)]
pub struct SourceProps {
    pub session: Value,
    pub ui_language: String,
    pub on_view: Callback<HistoricalActivity>,
}
#[function_component(ContinuationSource)]
pub fn continuation_source(props: &SourceProps) -> Html {
    let source = &props.session["continued_from"];
    if source.is_null() {
        return Html::default();
    }
    let lang = &props.ui_language;
    let activity = HistoricalActivity {
        learner: text(&props.session["learner_id"]),
        session: text(&source["session_id"]),
        activity: text(&source["activity_id"]),
        goal: text(&source["goal"]),
        title_stale: false,
        created_at: String::new(),
        finished: true,
        continued_from: Value::Null,
    };
    let view = props.on_view.clone();
    let plan = text(&props.session["continuation"]["pending_step"]["text"]);
    html! {<div class="continuation-source"><strong>{tr(lang,"本次续自一项已结束的活动","This activity continues a finished activity")}</strong><p class="continuation-goal"><small>{tr(lang,"当时的目标 · 历史记录","Earlier goal · historical record")}</small><br/>{historical_goal(source,lang)}</p>
    {if !plan.is_empty(){html!{<p><strong>{tr(lang,"当时的待做记录 · 需确认","Earlier plan · confirm its current status")}</strong><br/>{plan}</p>}}else{Html::default()}}
    <p>{tr(lang,"以上是历史记录。请重新确认今天的目标、模块与设置，以及旧计划后来是否已完成；不会自动成为本次事实。","These are historical records. Confirm today's goal, modules and setup, and whether earlier plans have since been completed; they are not current facts.")}</p><button class="text-button" onclick={Callback::from(move |_|view.emit(activity.clone()))}>{tr(lang,"查看来源活动","View source activity")}</button></div>}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stale_selected_title_keeps_activity_identity_and_does_not_claim_no_goal_was_recorded() {
        let mut activity = HistoricalActivity::from_finished_session(&json!({
            "learner_id":"learner","session_id":"selected","activity_id":"activity",
            "status":"finished","state":{"goal":"cached private goal"},
            "continued_from":{"session_id":"ancestor","goal":"cached ancestor goal"}
        }))
        .unwrap();
        assert_eq!(title(&activity, "en"), "cached private goal");
        activity.invalidate_cached_title();
        assert_eq!(activity.learner, "learner");
        assert_eq!(activity.session, "selected");
        assert_eq!(activity.activity, "activity");
        assert_eq!(activity.continued_from["session_id"], "ancestor");
        assert!(activity.finished && activity.goal.is_empty());
        assert!(activity.continued_from.get("goal").is_none());
        for language in ["zh", "en"] {
            let label = title(&activity, language);
            assert!(label.contains(if language == "en" {
                "sources updated"
            } else {
                "来源已更新"
            }));
            assert!(
                !label.contains("cached")
                    && !label.contains("no recorded goal")
                    && !label.contains("未记录目标")
            );
        }
    }
}
