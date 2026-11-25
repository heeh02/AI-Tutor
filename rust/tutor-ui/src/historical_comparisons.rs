//! One bounded, read-only comparison-history page for an explicitly selected activity.
use crate::{
    api::{api, ApiResult},
    comparison_record::render_record,
};
use serde_json::Value;
use std::collections::VecDeque;
use yew::prelude::*;

const PAGE_SIZE: usize = 10;
const MAX_BACK_PAGES: usize = 64;
fn tr<'a>(language: &str, zh: &'a str, en: &'a str) -> &'a str {
    if language == "en" {
        en
    } else {
        zh
    }
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub learner_id: String,
    pub namespace: String,
    pub session_id: String,
    pub learner_epoch: i64,
    pub ui_language: String,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq)]
struct Owner {
    learner: String,
    namespace: String,
    session: String,
    epoch: i64,
    enabled: bool,
}
impl Owner {
    fn from_props(props: &Props) -> Self {
        Self {
            learner: props.learner_id.clone(),
            namespace: props.namespace.clone(),
            session: props.session_id.clone(),
            epoch: props.learner_epoch,
            enabled: props.enabled,
        }
    }
    fn valid(&self) -> bool {
        self.enabled
            && !self.learner.is_empty()
            && !self.session.is_empty()
            && self.epoch >= 0
            && matches!(self.namespace.as_str(), "real" | "simulator")
    }
    fn path(&self, before: Option<i64>) -> String {
        let mut path = format!(
            "/api/learners/{}/sessions/{}/comparison-history?limit={PAGE_SIZE}",
            self.learner, self.session
        );
        if let Some(before) = before {
            path.push_str(&format!("&before={before}"));
        }
        path
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    owner: Owner,
    revision: u64,
    before: Option<i64>,
}

struct Pages {
    owner: Owner,
    revision: u64,
    pending: Option<Ticket>,
    items: Vec<Value>,
    before: Option<i64>,
    next_before: Option<i64>,
    previous: VecDeque<Option<i64>>,
    page: usize,
    loaded: bool,
    error: bool,
}
impl Pages {
    fn new(owner: Owner) -> Self {
        Self {
            owner,
            revision: 0,
            pending: None,
            items: vec![],
            before: None,
            next_before: None,
            previous: VecDeque::new(),
            page: 1,
            loaded: false,
            error: false,
        }
    }
    fn clear_page(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.items.clear();
        self.next_before = None;
        self.loaded = false;
        self.error = false;
    }
    fn bind(&mut self, owner: Owner) {
        if self.owner != owner {
            self.owner = owner;
            self.before = None;
            self.previous.clear();
            self.page = 1;
            self.clear_page();
            // Keep the physical request slot until its old owner replies.
        }
    }
    fn needs_request(&self) -> bool {
        self.owner.valid() && self.pending.is_none() && !self.loaded && !self.error
    }
    fn begin(&mut self) -> Option<Ticket> {
        if !self.needs_request() {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        let ticket = Ticket {
            owner: self.owner.clone(),
            revision: self.revision,
            before: self.before,
        };
        self.pending = Some(ticket.clone());
        Some(ticket)
    }
    fn complete(&mut self, ticket: &Ticket, result: ApiResult) -> bool {
        if self.pending.as_ref() != Some(ticket) {
            return false;
        }
        self.pending = None;
        if ticket.owner != self.owner
            || ticket.revision != self.revision
            || ticket.before != self.before
            || !self.owner.valid()
        {
            return false;
        }
        let valid = result.ok().and_then(|value| {
            if value["learner_id"] != self.owner.learner
                || value["session_id"] != self.owner.session
                || value["namespace"] != self.owner.namespace
            {
                return None;
            }
            let next = value.get("next_before")?;
            if !next.is_null()
                && !next
                    .as_i64()
                    .is_some_and(|next| next > 0 && self.before.is_none_or(|before| next < before))
            {
                return None;
            }
            let items = value["items"].as_array()?;
            if items.len() > PAGE_SIZE
                || items.iter().any(|item| {
                    item["hardware_verified"] != false
                        || if self.owner.namespace == "simulator" {
                            item["source_type"] != "simulated"
                        } else {
                            !matches!(
                                item["source_type"].as_str(),
                                Some("learner_report" | "operator_confirmed")
                            )
                        }
                })
            {
                return None;
            }
            Some((items.clone(), next.as_i64()))
        });
        match valid {
            Some((items, next)) => {
                self.items = items;
                self.next_before = next;
                self.loaded = true;
                self.error = false;
            }
            None => {
                self.items.clear();
                self.next_before = None;
                self.error = true;
                self.loaded = false;
            }
        }
        true
    }
    fn refresh(&mut self) {
        self.before = None;
        self.previous.clear();
        self.page = 1;
        self.clear_page();
    }
    fn older(&mut self) -> bool {
        if !self.owner.valid() || self.pending.is_some() || !self.loaded {
            return false;
        }
        let Some(next) = self.next_before else {
            return false;
        };
        self.previous.push_back(self.before);
        if self.previous.len() > MAX_BACK_PAGES {
            self.previous.pop_front();
        }
        self.before = Some(next);
        self.page += 1;
        self.clear_page();
        true
    }
    fn newer(&mut self) -> bool {
        if !self.owner.valid() || self.pending.is_some() {
            return false;
        }
        let Some(before) = self.previous.pop_back() else {
            return false;
        };
        self.before = before;
        self.page = self.page.saturating_sub(1);
        self.clear_page();
        true
    }
}

pub enum Msg {
    Fetch,
    Result(Ticket, ApiResult),
    Refresh,
    Older,
    Newer,
}
pub struct HistoricalComparisons {
    pages: Pages,
}
impl Component for HistoricalComparisons {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Fetch);
        Self {
            pages: Pages::new(Owner::from_props(ctx.props())),
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, _: &Props) -> bool {
        self.pages.bind(Owner::from_props(ctx.props()));
        if self.pages.needs_request() {
            ctx.link().send_message(Msg::Fetch);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        self.pages.bind(Owner::from_props(ctx.props()));
        match msg {
            Msg::Fetch => {
                let Some(ticket) = self.pages.begin() else {
                    return false;
                };
                let path = ticket.owner.path(ticket.before);
                ctx.link()
                    .send_future(async move { Msg::Result(ticket, api(&path, "GET", None).await) });
            }
            Msg::Result(ticket, result) => {
                self.pages.complete(&ticket, result);
            }
            Msg::Refresh => {
                if !self.pages.owner.valid() || self.pages.pending.is_some() {
                    return false;
                }
                self.pages.refresh();
            }
            Msg::Older => {
                if !self.pages.older() {
                    return false;
                }
            }
            Msg::Newer => {
                if !self.pages.newer() {
                    return false;
                }
            }
        }
        if self.pages.needs_request() {
            ctx.link().send_message(Msg::Fetch);
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let language = ctx.props().ui_language.as_str();
        let pending = self.pages.pending.is_some();
        let available = self.pages.owner.valid();
        html! {
            <section class="historical-comparisons" aria-label={tr(language,"历史实验比较","Read-only past comparisons")}>
                <div class="comparison-heading"><h3>{tr(language,"历史实验比较","Read-only past comparisons")}</h3><button class="text-button" disabled={!available||pending} onclick={ctx.link().callback(|_|Msg::Refresh)}>{tr(language,"刷新并回最新","Refresh to latest")}</button></div>
                <p class="fine-print">{tr(language,"只读查看所选活动的比较记录，不会改变当前活动或未保存草稿。","View comparisons from the selected activity without changing your current activity or unsaved drafts.")}</p>
                {if !available{html!{<p class="fine-print">{tr(language,"当前操作结束后可查看历史比较。","Past comparisons are available when the current operation has finished.")}</p>}}else if pending||self.pages.needs_request(){html!{<p role="status">{tr(language,"正在读取历史比较…","Loading past comparisons…")}</p>}}else if self.pages.error{html!{<p role="status">{tr(language,"历史比较暂不可用，请刷新重试。","Past comparisons are unavailable. Refresh to try again.")}</p>}}else if self.pages.loaded&&self.pages.items.is_empty(){html!{<p class="empty-note">{tr(language,"这页没有可用的比较记录。","No available comparison records on this page.")}</p>}}else{Html::default()}}
                {for self.pages.items.iter().map(|item|render_record(language,item,true))}
                <div class="history-controls"><button class="text-button" disabled={!available||pending||self.pages.previous.is_empty()} onclick={ctx.link().callback(|_|Msg::Newer)}>{tr(language,"较新一页","Newer page")}</button><span>{if language=="en"{format!("Page {}",self.pages.page)}else{format!("第 {} 页",self.pages.page)}}</span><button class="text-button" disabled={!available||pending||self.pages.next_before.is_none()} onclick={ctx.link().callback(|_|Msg::Older)}>{tr(language,"更早一页","Older page")}</button></div>
            </section>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn owner() -> Owner {
        Owner {
            learner: "l".into(),
            namespace: "simulator".into(),
            session: "s".into(),
            epoch: 0,
            enabled: true,
        }
    }
    fn response(next: Option<i64>) -> ApiResult {
        Ok(
            json!({"learner_id":"l","session_id":"s","namespace":"simulator","items":[{"source_type":"simulated","hardware_verified":false,"question":"original"}],"next_before":next}),
        )
    }
    #[test]
    fn owner_namespace_epoch_and_enabled_changes_revoke_loaded_and_late_pages() {
        for change in 0..5 {
            let mut pages = Pages::new(owner());
            let first = pages.begin().unwrap();
            assert!(pages.complete(&first, response(Some(100))));
            assert!(!pages.items.is_empty());
            let mut next = owner();
            match change {
                0 => next.learner = "other".into(),
                1 => next.session = "other".into(),
                2 => next.namespace = "real".into(),
                3 => next.epoch += 1,
                _ => next.enabled = false,
            }
            pages.bind(next.clone());
            assert!(pages.items.is_empty());
            pages.bind(owner());
            let late = pages.begin().unwrap();
            pages.bind(next);
            assert!(pages.items.is_empty());
            assert!(!pages.complete(&late, response(Some(100))));
            assert!(pages.items.is_empty());
            pages.bind(owner());
            assert!(pages.begin().is_some());
        }
    }
    #[test]
    fn bounded_pages_restore_cursors_and_never_accumulate_records() {
        let mut pages = Pages::new(owner());
        for index in 0..70 {
            let ticket = pages.begin().unwrap();
            assert!(pages.complete(&ticket, response(Some(1000 - index))));
            assert_eq!(pages.items.len(), 1);
            assert!(pages.older());
            assert!(pages.items.is_empty());
        }
        assert_eq!(pages.previous.len(), MAX_BACK_PAGES);
        assert_eq!(pages.page, 71);
        let before = pages.before;
        assert!(pages.newer());
        assert_ne!(pages.before, before);
        let ticket = pages.begin().unwrap();
        assert!(ticket
            .owner
            .path(ticket.before)
            .contains("comparison-history?limit=10&before="));
        assert!(pages.complete(&ticket, response(None)));
        assert!(!pages.older());
        pages.refresh();
        assert_eq!(pages.before, None);
        assert!(pages.previous.is_empty());
        assert_eq!(pages.page, 1);
    }
    #[test]
    fn malformed_owner_grade_size_and_cursor_are_rejected_without_retaining_old_text() {
        for problem in 0..7 {
            let mut pages = Pages::new(owner());
            pages.before = Some(100);
            let ticket = pages.begin().unwrap();
            let mut value = response(Some(50)).unwrap();
            match problem {
                0 => value["learner_id"] = json!("other"),
                1 => value["session_id"] = json!("other"),
                2 => value["namespace"] = json!("real"),
                3 => value["items"][0]["source_type"] = json!("operator_confirmed"),
                4 => value["items"] = json!(vec![value["items"][0].clone(); 11]),
                5 => value["next_before"] = json!(100),
                _ => value["next_before"] = json!(0),
            }
            assert!(pages.complete(&ticket, Ok(value)));
            assert!(pages.error);
            assert!(pages.items.is_empty());
            assert!(pages.begin().is_none());
        }
    }
    #[test]
    fn refresh_during_an_old_page_request_rejects_its_late_cursor_and_only_reads_explicitly() {
        let mut pages = Pages::new(owner());
        let first = pages.begin().unwrap();
        pages.complete(&first, response(Some(50)));
        assert!(pages.older());
        let old = pages.begin().unwrap();
        pages.refresh();
        assert!(pages.begin().is_none());
        assert!(!pages.complete(&old, response(Some(25))));
        assert_eq!(pages.before, None);
        assert_eq!(pages.next_before, None);
        assert!(pages.items.is_empty());
        let new = pages.begin().unwrap();
        assert_eq!(new.before, None);
        pages.complete(&new, Err("offline".into()));
        assert!(pages.error);
        assert!(pages.begin().is_none());
        pages.refresh();
        assert!(pages.begin().is_some());
    }
}
