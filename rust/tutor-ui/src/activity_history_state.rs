//! Bounded activity browsing and the identity of one explicit continuation.
use crate::api::ApiResult;
use serde_json::{json, Value};
use std::collections::VecDeque;

pub const PAGE_SIZE: usize = 20;
const MAX_BACK_PAGES: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct HistoricalActivity {
    pub learner: String,
    pub session: String,
    pub activity: String,
    pub goal: String,
    pub title_stale: bool,
    pub created_at: String,
    pub finished: bool,
    pub continued_from: Value,
}
impl HistoricalActivity {
    pub(crate) fn invalidate_cached_title(&mut self) {
        self.goal.clear();
        self.title_stale = true;
        if let Some(source) = self.continued_from.as_object_mut() {
            source.remove("goal");
        }
    }
    pub(crate) fn from_finished_session(session: &Value) -> Option<Self> {
        if session["status"] != "finished" {
            return None;
        }
        let learner = session["learner_id"].as_str().filter(|id| !id.is_empty())?;
        Self::parse(
            learner,
            &json!({
                "session_id": session["session_id"],
                "activity_id": session["activity_id"],
                "goal": crate::model::text(&session["state"]["goal"]),
                "created_at": session["created_at"].as_str().unwrap_or(""),
                "status": "finished",
                "continued_from": session["continued_from"],
            }),
        )
    }
    fn parse(learner: &str, value: &Value) -> Option<Self> {
        Some(Self {
            learner: learner.into(),
            session: value["session_id"]
                .as_str()
                .filter(|s| !s.is_empty())?
                .into(),
            activity: value["activity_id"]
                .as_str()
                .filter(|s| !s.is_empty())?
                .into(),
            goal: value["goal"].as_str()?.into(),
            title_stale: false,
            created_at: value["created_at"].as_str()?.into(),
            finished: value["status"] == "finished",
            continued_from: value["continued_from"].clone(),
        })
    }
}

pub struct ActivityPages {
    pub learner: String,
    learner_epoch: i64,
    pub items: Vec<HistoricalActivity>,
    pub before: Option<i64>,
    pub next_before: Option<i64>,
    pub previous: VecDeque<Option<i64>>,
    pub page: usize,
    pub loading: bool,
    pub loaded: bool,
    pub error: bool,
    revision: u64,
}
impl ActivityPages {
    pub fn new(learner: String, learner_epoch: i64) -> Self {
        Self {
            learner,
            learner_epoch,
            items: vec![],
            before: None,
            next_before: None,
            previous: VecDeque::new(),
            page: 1,
            loading: false,
            loaded: false,
            error: false,
            revision: 0,
        }
    }
    pub fn bind(&mut self, learner: &str, learner_epoch: i64) {
        if self.learner != learner || self.learner_epoch != learner_epoch {
            let revision = self.revision.wrapping_add(1);
            *self = Self::new(learner.into(), learner_epoch);
            self.revision = revision;
        }
    }
    pub fn begin(&mut self) -> Option<u64> {
        if self.loading || self.learner.is_empty() || self.loaded || self.error {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        self.loading = true;
        Some(self.revision)
    }
    pub fn complete(&mut self, revision: u64, result: ApiResult) -> bool {
        if revision != self.revision || !self.loading {
            return false;
        }
        self.loading = false;
        let parsed = result.ok().and_then(|value| {
            if value["learner_id"] != self.learner {
                return None;
            }
            let next = &value["next_before"];
            if !next.is_null()
                && !next
                    .as_i64()
                    .is_some_and(|n| n > 0 && self.before.is_none_or(|before| n < before))
            {
                return None;
            }
            let values = value["items"].as_array()?;
            if values.len() > PAGE_SIZE {
                return None;
            }
            let items: Option<Vec<_>> = values
                .iter()
                .map(|v| HistoricalActivity::parse(&self.learner, v))
                .collect();
            Some((items?, next.as_i64()))
        });
        if let Some((items, next)) = parsed {
            self.items = items;
            self.next_before = next;
            self.loaded = true;
        } else {
            self.error = true;
        }
        true
    }
    pub fn reset_page(&mut self) {
        self.items.clear();
        self.next_before = None;
        self.loaded = false;
        self.error = false;
    }
    pub fn refresh(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.loading = false;
        self.before = None;
        self.previous.clear();
        self.page = 1;
        self.reset_page();
    }
    pub fn older(&mut self) -> bool {
        let Some(next) = self.next_before.filter(|_| !self.loading) else {
            return false;
        };
        self.previous.push_back(self.before);
        if self.previous.len() > MAX_BACK_PAGES {
            self.previous.pop_front();
        }
        self.before = Some(next);
        self.page += 1;
        self.reset_page();
        true
    }
    pub fn newer(&mut self) -> bool {
        if self.loading {
            return false;
        }
        let Some(before) = self.previous.pop_back() else {
            return false;
        };
        self.before = before;
        self.page -= 1;
        self.reset_page();
        true
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContinueRequest {
    pub source: HistoricalActivity,
    pub current_session: String,
    pub generation: u64,
    pub nonce: String,
}
impl ContinueRequest {
    pub fn path(&self) -> String {
        format!(
            "/api/learners/{}/sessions/{}/continue",
            self.source.learner, self.source.session
        )
    }
    pub fn payload(&self) -> Value {
        json!({"client_submission_id":self.nonce,"expected_current_session_id":self.current_session,"expected_generation":self.generation})
    }
    pub fn recognizes(&self, session: &Value) -> bool {
        session["learner_id"] == self.source.learner
            && session["session_id"].as_str().is_some_and(|s| {
                !s.is_empty() && s != self.source.session && s != self.current_session
            })
            && session["activity_id"]
                .as_str()
                .is_some_and(|a| !a.is_empty() && a != self.source.activity)
            && session["continuation"]["client_submission_id"] == self.nonce
            && session["continuation"]["source_session_id"] == self.source.session
            && session["continuation"]["source_activity_id"] == self.source.activity
    }
}

#[derive(Default)]
pub struct ContinuationIntent {
    pub request: Option<ContinueRequest>,
    pub in_flight: bool,
    pub uncertain: bool,
}
impl ContinuationIntent {
    pub fn begin(&mut self, request: ContinueRequest) -> bool {
        if self.request.is_some() || !request.source.finished {
            return false;
        }
        self.request = Some(request);
        self.in_flight = true;
        self.uncertain = false;
        true
    }
    pub fn retry(&mut self) -> Option<ContinueRequest> {
        if self.in_flight || !self.uncertain {
            return None;
        }
        self.in_flight = true;
        self.request.clone()
    }
    pub fn unknown(&mut self, request: &ContinueRequest) -> bool {
        if self.request.as_ref() != Some(request) {
            return false;
        }
        self.in_flight = false;
        self.uncertain = true;
        true
    }
    pub fn clear(&mut self, request: &ContinueRequest) -> bool {
        if self.request.as_ref() != Some(request) {
            return false;
        }
        *self = Self::default();
        true
    }
    /// A state snapshot can arrive before (or instead of) the POST response.
    /// Only the precise nonce plus historical owner may resolve that operation.
    pub fn observe(&mut self, session: &Value) -> bool {
        if self.request.as_ref().is_some_and(|r| r.recognizes(session)) {
            *self = Self::default();
            return true;
        }
        // A different learner is a new UI owner; no old callback may navigate it.
        if self.request.as_ref().is_some_and(|r| {
            session["learner_id"].is_string() && session["learner_id"] != r.source.learner
        }) {
            *self = Self::default();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(sid: &str) -> Value {
        json!({"session_id":sid,"activity_id":"old-activity","goal":"原始目标 Original goal","created_at":"2026-11-04","status":"finished","continued_from":null})
    }
    #[test]
    fn finished_session_entry_preserves_identity_without_adopting_historical_facts() {
        let mut session = json!({"learner_id":"learner","session_id":"finished-session","activity_id":"finished-activity",
            "status":"finished","created_at":"2026-11-10","state":{"goal":"今天的原文目标"},
            "continued_from":{"session_id":"ancestor","goal":"older goal"},"continuation":{"pending_step":{"text":"Historical plan"}}});
        let entry = HistoricalActivity::from_finished_session(&session).unwrap();
        assert_eq!(entry.learner, "learner");
        assert_eq!(entry.session, "finished-session");
        assert_eq!(entry.activity, "finished-activity");
        assert_eq!(entry.goal, "今天的原文目标");
        assert!(entry.finished);
        session["status"] = json!("idle");
        assert!(HistoricalActivity::from_finished_session(&session).is_none());
        session["status"] = json!("finished");
        session["activity_id"] = Value::Null;
        assert!(HistoricalActivity::from_finished_session(&session).is_none());
    }
    fn page(learner: &str, next: Option<i64>) -> ApiResult {
        Ok(json!({"learner_id":learner,"items":[row("old")],"next_before":next}))
    }
    fn request() -> ContinueRequest {
        ContinueRequest {
            source: HistoricalActivity::parse("l", &row("old")).unwrap(),
            current_session: "current".into(),
            generation: 4,
            nonce: "one-click".into(),
        }
    }
    fn created(r: &ContinueRequest) -> Value {
        json!({"learner_id":"l","session_id":"new","activity_id":"new-activity","continuation":{"client_submission_id":r.nonce,"source_session_id":"old","source_activity_id":"old-activity"}})
    }
    #[test]
    fn late_directory_reply_cannot_cross_learner_or_aba() {
        let mut pages = ActivityPages::new("a".into(), 0);
        let old = pages.begin().unwrap();
        pages.bind("b", 0);
        pages.bind("a", 0);
        let current = pages.begin().unwrap();
        assert!(!pages.complete(old, page("a", None)));
        assert!(pages.items.is_empty());
        assert!(pages.complete(current, page("a", Some(90))));
        assert_eq!(pages.items[0].goal, "原始目标 Original goal");
    }
    #[test]
    fn directory_epoch_change_clears_titles_and_rejects_old_same_learner_pages() {
        let mut pages = ActivityPages::new("a".into(), 0);
        let loaded = pages.begin().unwrap();
        assert!(pages.complete(loaded, page("a", Some(90))));
        assert!(!pages.items.is_empty());
        pages.bind("a", 1);
        assert!(pages.items.is_empty());
        assert!(pages.previous.is_empty());
        assert!(pages.next_before.is_none());
        assert_eq!(pages.page, 1);
        let late = pages.begin().unwrap();
        pages.bind("a", 2);
        let current = pages.begin().unwrap();
        assert!(!pages.complete(late, page("a", Some(50))));
        assert!(pages.items.is_empty());
        assert!(pages.complete(current, page("a", None)));
        assert!(!pages.items[0].title_stale);
    }
    #[test]
    fn directory_paging_drops_text_and_rejects_nonprogressing_cursor() {
        let mut pages = ActivityPages::new("a".into(), 0);
        let v = pages.begin().unwrap();
        pages.complete(v, page("a", Some(90)));
        assert!(pages.older());
        assert!(pages.items.is_empty());
        let v = pages.begin().unwrap();
        pages.complete(v, page("a", Some(90)));
        assert!(pages.error);
        let mut long = ActivityPages::new("a".into(), 0);
        for next in (800..880).rev() {
            let revision = long.begin().unwrap();
            long.complete(revision, page("a", Some(next)));
            assert!(long.older());
            assert!(long.items.is_empty());
            assert!(long.previous.len() <= MAX_BACK_PAGES);
        }
        assert!(pages.newer());
        assert_eq!(pages.before, None);
        let v = pages.begin().unwrap();
        pages.complete(
            v,
            Ok(json!({"learner_id":"a","items":vec![row("x");21],"next_before":null})),
        );
        assert!(pages.error);
    }
    #[test]
    fn unknown_continuation_retries_the_identical_operation_and_blocks_new_nonce() {
        let mut intent = ContinuationIntent::default();
        let r = request();
        let mut unfinished = r.clone();
        unfinished.source.finished = false;
        assert!(!intent.begin(unfinished));
        assert!(intent.begin(r.clone()));
        assert!(!intent.begin(request()));
        assert!(intent.retry().is_none());
        intent.unknown(&r);
        let retried = intent.retry().unwrap();
        assert_eq!(retried.payload(), r.payload());
        assert_eq!(retried.path(), r.path());
        assert!(intent.observe(&created(&r)));
        assert!(intent.request.is_none());
        assert!(!intent.unknown(&r));
    }
    #[test]
    fn recognition_requires_new_ids_and_exact_nonce_source_owner() {
        let r = request();
        assert!(r.recognizes(&created(&r)));
        for (field, value) in [
            ("session_id", "current"),
            ("activity_id", "old-activity"),
            ("learner_id", "other"),
        ] {
            let mut s = created(&r);
            s[field] = json!(value);
            assert!(!r.recognizes(&s));
        }
        let mut s = created(&r);
        s["continuation"]["client_submission_id"] = json!("another-click");
        assert!(!r.recognizes(&s));
        s = created(&r);
        s["continuation"]["source_session_id"] = json!("other-old");
        assert!(!r.recognizes(&s));
        let mut intent = ContinuationIntent::default();
        intent.begin(r.clone());
        intent.observe(&json!({"learner_id":"other"}));
        assert!(!intent.unknown(&r));
    }
    #[test]
    fn a_rejected_retry_releases_old_intent_without_navigating_or_claiming_rollback() {
        let r = request();
        let mut intent = ContinuationIntent::default();
        intent.begin(r.clone());
        intent.unknown(&r);
        // Another tab has selected a different session. This snapshot alone
        // may predate the POST, so only an explicit 4xx reply clears the request.
        assert!(!intent.observe(&json!({"learner_id":"l","session_id":"elsewhere"})));
        assert_eq!(intent.retry().unwrap(), r);
        assert!(intent.clear(&r));
        let mut next = request();
        next.nonce = "second-click".into();
        assert!(intent.begin(next.clone()));
        assert!(!intent.unknown(&r));
        assert!(!intent.clear(&r));
        assert_eq!(intent.request, Some(next));
    }
}
