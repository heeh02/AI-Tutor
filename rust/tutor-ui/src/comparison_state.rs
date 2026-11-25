//! One session's explicit comparison draft; saved history never enters a new submission.
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonOwner {
    pub learner: String,
    pub session: String,
    pub simulated: bool,
}
impl ComparisonOwner {
    pub fn path(&self) -> String {
        format!(
            "/api/learners/{}/sessions/{}/comparisons",
            self.learner, self.session
        )
    }
    pub fn exists(&self) -> bool {
        !self.learner.is_empty() && !self.session.is_empty()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ConditionDraft {
    pub name: String,
    pub status: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrialDraft {
    pub setting: String,
    pub status: String,
    pub result: String,
    pub count: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonDraft {
    pub question: String,
    pub changed_variable: String,
    pub conditions: Vec<ConditionDraft>,
    pub trials: [TrialDraft; 2],
    pub plan: String,
    pub accepted: bool,
    pub source: String,
}
impl Default for ComparisonDraft {
    fn default() -> Self {
        Self {
            question: String::new(),
            changed_variable: String::new(),
            conditions: vec![ConditionDraft::default()],
            trials: std::array::from_fn(|_| TrialDraft::default()),
            plan: String::new(),
            accepted: false,
            source: "learner_report".into(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftError {
    Question,
    Variable,
    Condition,
    Setting,
    Status,
    Result,
    Count,
    PendingResult,
    Plan,
    Source,
}
fn bounded(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.trim().chars().count() <= max
}
impl ComparisonDraft {
    pub fn dirty(&self) -> bool {
        self != &Self::default()
    }
    pub fn payload(&self, simulated: bool) -> Result<Value, DraftError> {
        if !bounded(&self.question, 200) {
            return Err(DraftError::Question);
        }
        if !bounded(&self.changed_variable, 200) {
            return Err(DraftError::Variable);
        }
        if !(1..=8).contains(&self.conditions.len())
            || self.conditions.iter().any(|c| {
                !bounded(&c.name, 200)
                    || !matches!(c.status.as_str(), "reported_same" | "not_confirmed")
            })
        {
            return Err(DraftError::Condition);
        }
        let mut trials = Vec::with_capacity(2);
        for (index, trial) in self.trials.iter().enumerate() {
            if !bounded(&trial.setting, 200) {
                return Err(DraftError::Setting);
            }
            let (result, count) = match trial.status.as_str() {
                "reported" => {
                    if !bounded(&trial.result, 1000) {
                        return Err(DraftError::Result);
                    }
                    let count = if trial.count.trim().is_empty() {
                        None
                    } else {
                        Some(
                            trial
                                .count
                                .trim()
                                .parse::<u32>()
                                .ok()
                                .filter(|n| (1..=10000).contains(n))
                                .ok_or(DraftError::Count)?,
                        )
                    };
                    (Some(trial.result.trim()), count)
                }
                "not_tested" => {
                    // Changing a select never silently destroys an observation.
                    // The learner must explicitly clear it or restore its status.
                    if !trial.result.trim().is_empty() || !trial.count.trim().is_empty() {
                        return Err(DraftError::PendingResult);
                    }
                    (None, None)
                }
                _ => return Err(DraftError::Status),
            };
            trials.push(
                json!({"label":if index==0 {"A"} else {"B"},"setting":trial.setting.trim(),
                "status":trial.status,"result":result,"trial_count":count}),
            );
        }
        let plan = if self.plan.trim().is_empty() {
            if self.accepted {
                return Err(DraftError::Plan);
            }
            Value::Null
        } else {
            if !bounded(&self.plan, 1000) {
                return Err(DraftError::Plan);
            }
            json!({"text":self.plan.trim(),"accepted":self.accepted})
        };
        if !simulated
            && !matches!(
                self.source.as_str(),
                "learner_report" | "operator_confirmed"
            )
        {
            return Err(DraftError::Source);
        }
        Ok(
            json!({"question":self.question.trim(),"changed_variable":self.changed_variable.trim(),
            "conditions":self.conditions.iter().map(|c|json!({"name":c.name.trim(),"status":c.status})).collect::<Vec<_>>(),
            "trials":trials,"pending_plan":plan,"source_type":if simulated {"simulated"} else {&self.source}}),
        )
    }
}

#[derive(Clone)]
pub struct Submission {
    pub owner: ComparisonOwner,
    pub epoch: u64,
    pub draft: ComparisonDraft,
    pub payload: Value,
}
pub struct ComparisonState {
    pub owner: ComparisonOwner,
    pub epoch: u64,
    pub draft: ComparisonDraft,
    pub items: Vec<Value>,
    pub list_revision: u64,
    retry: Option<Submission>,
}
impl ComparisonState {
    pub fn new(owner: ComparisonOwner) -> Self {
        Self {
            owner,
            epoch: 0,
            draft: ComparisonDraft::default(),
            items: vec![],
            list_revision: 0,
            retry: None,
        }
    }
    pub fn bind(&mut self, owner: ComparisonOwner) -> bool {
        if self.owner == owner {
            return false;
        }
        let epoch = self.epoch.wrapping_add(1);
        *self = Self::new(owner);
        self.epoch = epoch;
        true
    }
    pub fn edit(&mut self, change: impl FnOnce(&mut ComparisonDraft)) -> bool {
        let before = self.draft.clone();
        change(&mut self.draft);
        if self.draft == before {
            return false;
        }
        // An actual edit starts a different submission; a failed network call does not.
        self.retry = None;
        true
    }
    pub fn clear(&mut self) {
        self.draft = ComparisonDraft::default();
        self.retry = None;
    }
    pub fn submission(
        &mut self,
        new_id: impl FnOnce() -> String,
    ) -> Result<Submission, DraftError> {
        let mut payload = self.draft.payload(self.owner.simulated)?;
        if let Some(retry) = &self.retry {
            return Ok(retry.clone());
        }
        payload["submission_id"] = json!(new_id());
        let submission = Submission {
            owner: self.owner.clone(),
            epoch: self.epoch,
            draft: self.draft.clone(),
            payload,
        };
        self.retry = Some(submission.clone());
        Ok(submission)
    }
    fn current(&self, owner: &ComparisonOwner, epoch: u64, reply: &Value) -> bool {
        self.owner == *owner
            && self.epoch == epoch
            && reply["learner_id"] == owner.learner
            && reply["session_id"] == owner.session
    }
    fn valid_item(&self, item: &Value) -> bool {
        item["comparison_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            && item["hardware_verified"] == false
            && if self.owner.simulated {
                item["source_type"] == "simulated"
            } else {
                matches!(
                    item["source_type"].as_str(),
                    Some("learner_report" | "operator_confirmed")
                )
            }
    }
    pub fn invalidate_list(&mut self, clear: bool) {
        self.list_revision = self.list_revision.wrapping_add(1);
        if clear {
            self.items.clear();
        }
    }
    pub fn loaded(
        &mut self,
        owner: &ComparisonOwner,
        epoch: u64,
        revision: u64,
        reply: &Value,
    ) -> bool {
        let Some(items) = reply["items"].as_array() else {
            return false;
        };
        if !self.current(owner, epoch, reply)
            || revision != self.list_revision
            || items.len() > 10
            || items.iter().any(|i| !self.valid_item(i))
        {
            return false;
        }
        self.items = items.clone();
        true
    }
    pub fn saved(&mut self, submission: &Submission, reply: &Value) -> bool {
        if !self.current(&submission.owner, submission.epoch, reply)
            || !self.valid_item(&reply["item"])
        {
            return false;
        }
        let item = reply["item"].clone();
        self.items
            .retain(|existing| existing["comparison_id"] != item["comparison_id"]);
        self.items.insert(0, item);
        self.items.truncate(10);
        if self.draft == submission.draft {
            self.clear();
        }
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonGuard {
    pub owner: ComparisonOwner,
    pub dirty: bool,
    pub saving: bool,
}
impl ComparisonGuard {
    pub fn blocks_navigation(&self, owner: &ComparisonOwner) -> bool {
        self.owner == *owner && (self.dirty || self.saving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner(session: &str, simulated: bool) -> ComparisonOwner {
        ComparisonOwner {
            learner: "learner".into(),
            session: session.into(),
            simulated,
        }
    }
    fn draft() -> ComparisonDraft {
        ComparisonDraft {
            question: "比较停车位置 / Compare stops".into(),
            changed_variable: "阈值 / Threshold".into(),
            conditions: vec![ConditionDraft {
                name: "同一小车 / Same rover".into(),
                status: "reported_same".into(),
            }],
            trials: [
                TrialDraft {
                    setting: "20 cm".into(),
                    status: "reported".into(),
                    result: "碰到墙 / Hit wall".into(),
                    count: String::new(),
                },
                TrialDraft {
                    setting: "30 cm".into(),
                    status: "not_tested".into(),
                    ..Default::default()
                },
            ],
            plan: "下一次试 30 cm / Try 30 cm next".into(),
            ..Default::default()
        }
    }
    fn reply() -> Value {
        json!({"learner_id":"learner","session_id":"a","item":{"comparison_id":"c","source_type":"learner_report","hardware_verified":false}})
    }
    #[test]
    fn statuses_are_explicit_unknown_counts_stay_null_and_plan_is_not_accepted() {
        let mut value = draft();
        let payload = value.payload(false).unwrap();
        assert!(payload["trials"][0]["trial_count"].is_null());
        assert!(payload["trials"][1]["result"].is_null());
        assert_eq!(payload["pending_plan"]["accepted"], false);
        value.trials[1].status.clear();
        assert_eq!(value.payload(false), Err(DraftError::Status));
    }
    #[test]
    fn changing_observation_to_pending_requires_explicit_clearing_not_data_loss() {
        let mut value = draft();
        value.trials[0].status = "not_tested".into();
        assert_eq!(value.payload(false), Err(DraftError::PendingResult));
        assert_eq!(value.trials[0].result, "碰到墙 / Hit wall");
        value.trials[0].result.clear();
        assert!(value.payload(false).unwrap()["trials"][0]["result"].is_null());
        value.source = "simulated".into();
        assert_eq!(value.payload(false), Err(DraftError::Source));
        assert_eq!(value.payload(true).unwrap()["source_type"], "simulated");
    }
    #[test]
    fn network_retry_keeps_id_but_new_edits_survive_success_and_get_a_new_id() {
        let mut state = ComparisonState::new(owner("a", false));
        state.draft = draft();
        let first = state.submission(|| "first".into()).unwrap();
        assert_eq!(
            state
                .submission(|| panic!("retry must keep id"))
                .unwrap()
                .payload,
            first.payload
        );
        state.edit(|draft| draft.question = "newer edit".into());
        assert!(state.saved(&first, &reply()));
        assert_eq!(state.draft.question, "newer edit");
        assert_eq!(
            state.submission(|| "second".into()).unwrap().payload["submission_id"],
            "second"
        );
    }
    #[test]
    fn profile_switch_and_return_reject_late_save_and_list_responses() {
        let mut state = ComparisonState::new(owner("a", false));
        state.draft = draft();
        let submitted = state.submission(|| "first".into()).unwrap();
        state.bind(owner("b", false));
        state.bind(owner("a", false));
        assert!(!state.saved(&submitted, &reply()));
        assert!(!state.loaded(
            &submitted.owner,
            submitted.epoch,
            0,
            &json!({"learner_id":"learner","session_id":"a","items":[]})
        ));
        assert!(state.items.is_empty());
        assert!(!state.draft.dirty());
    }
    #[test]
    fn invalid_counts_and_accepted_empty_plans_are_not_submitted() {
        for count in ["0", "-1", "1.5", "10001", "unknown"] {
            let mut value = draft();
            value.trials[0].count = count.into();
            assert_eq!(value.payload(false), Err(DraftError::Count));
        }
        let mut value = draft();
        value.accepted = true;
        value.plan.clear();
        assert_eq!(value.payload(false), Err(DraftError::Plan));
    }
    #[test]
    fn older_list_cannot_erase_a_saved_record_or_restore_invalidated_sources() {
        let mut state = ComparisonState::new(owner("a", false));
        state.draft = draft();
        let pending = state.submission(|| "once".into()).unwrap();
        let revision = state.list_revision;
        state.invalidate_list(false);
        assert!(state.saved(&pending, &reply()));
        assert!(!state.loaded(
            &pending.owner,
            pending.epoch,
            revision,
            &json!({"learner_id":"learner","session_id":"a","items":[]})
        ));
        assert_eq!(state.items.len(), 1);
        let revision = state.list_revision;
        state.invalidate_list(true);
        assert!(!state.loaded(
            &pending.owner,
            pending.epoch,
            revision,
            &json!({"learner_id":"learner","session_id":"a","items":[reply()["item"]]})
        ));
        assert!(state.items.is_empty());
    }
    #[test]
    fn save_retry_deduplicates_and_list_stays_bounded_without_touching_draft() {
        let mut state = ComparisonState::new(owner("a", false));
        state.draft = draft();
        let submission = state.submission(|| "same".into()).unwrap();
        assert!(state.saved(&submission, &reply()));
        assert!(state.saved(&submission, &reply()));
        assert_eq!(state.items.len(), 1);
        state.edit(|d| d.question = "new draft".into());
        for i in 0..12 {
            let mut response = reply();
            response["item"]["comparison_id"] = json!(format!("c{i}"));
            assert!(state.saved(&submission, &response));
        }
        assert_eq!(state.items.len(), 10);
        assert_eq!(state.draft.question, "new draft");
        let mut fake = reply();
        fake["item"]["hardware_verified"] = json!(true);
        assert!(!state.saved(&submission, &fake));
        let mut cross = reply();
        cross["session_id"] = json!("b");
        assert!(!state.saved(&submission, &cross));
    }
    #[test]
    fn draft_navigation_gate_only_owns_the_current_session() {
        let mut guard = ComparisonGuard {
            owner: owner("a", false),
            dirty: true,
            saving: false,
        };
        assert!(guard.blocks_navigation(&owner("a", false)));
        assert!(!guard.blocks_navigation(&owner("b", false)));
        guard.dirty = false;
        assert!(!guard.blocks_navigation(&owner("a", false)));
        guard.saving = true;
        assert!(guard.blocks_navigation(&owner("a", false)));
        assert!(!guard.blocks_navigation(&owner("a", true)));
    }
}
