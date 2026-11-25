//! Tutor text delivery. Speech ownership and controls remain outside this component.
use crate::{
    api::{ApiResult, api},
    preview_browser::{PaintOpportunity, VisibilityListener, hidden},
    tutor_stream_state::{StreamKey, StreamState, incomplete, terminal},
};
use gloo_timers::callback::Timeout;
use serde_json::json;
use std::{cell::Cell, rc::Rc};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct StreamProps {
    pub binding: StreamKey,
    pub enabled: bool,
    pub ack_enabled: bool,
    pub viewport: NodeRef,
    pub head_revision: u64,
    pub head_phase: String,
    pub persisted_text: String,
    pub ui_language: String,
    pub on_progress: Callback<String>,
    pub on_sync: Callback<()>,
}

#[derive(Clone, Copy)]
struct DisplayRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}
impl DisplayRect {
    fn from_dom(rect: web_sys::DomRect) -> Self {
        Self {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        }
    }
    fn nonempty(self) -> bool {
        self.left < self.right && self.top < self.bottom
    }
}
fn intersects_display(
    segment: DisplayRect,
    conversation: DisplayRect,
    screen: DisplayRect,
) -> bool {
    [segment, conversation, screen].iter().all(|r| r.nonempty())
        && segment.left.max(conversation.left).max(screen.left)
            < segment.right.min(conversation.right).min(screen.right)
        && segment.top.max(conversation.top).max(screen.top)
            < segment.bottom.min(conversation.bottom).min(screen.bottom)
}
fn display_opportunity(viewport: &NodeRef, segment: &NodeRef) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let (Some(viewport), Some(segment)) = (
        viewport.cast::<web_sys::Element>(),
        segment.cast::<web_sys::Element>(),
    ) else {
        return false;
    };
    let (Some(width), Some(height)) = (
        window.inner_width().ok().and_then(|value| value.as_f64()),
        window.inner_height().ok().and_then(|value| value.as_f64()),
    ) else {
        return false;
    };
    intersects_display(
        DisplayRect::from_dom(segment.get_bounding_client_rect()),
        DisplayRect::from_dom(viewport.get_bounding_client_rect()),
        DisplayRect {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        },
    )
}

/// Visibility/ownership changes invalidate both queued frames and late replies.
#[derive(Default)]
struct DisplayGate {
    epoch: u64,
    active: bool,
}
#[derive(Clone, Copy, Default)]
struct AckGate {
    revision: u64,
    allowed: bool,
}
impl AckGate {
    fn bind(&mut self, allowed: bool) {
        if self.allowed != allowed {
            self.revision = self.revision.wrapping_add(1);
            self.allowed = allowed;
        }
    }
    fn accepts(self, revision: u64) -> bool {
        self.allowed && self.revision == revision
    }
}
impl DisplayGate {
    fn bind(&mut self, active: bool, owner_changed: bool) -> bool {
        if active == self.active && !owner_changed {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.active = active;
        true
    }
    fn accepts(&self, epoch: u64) -> bool {
        self.active && self.epoch == epoch
    }
}

pub enum StreamMsg {
    Fetch,
    Snapshot(u64, u64, StreamKey, ApiResult),
    Frame(u64, u64, usize),
    Receipt(u64, u64, StreamKey, usize, Option<ApiResult>),
    Visibility,
}
pub struct TutorStream {
    state: StreamState,
    gate: DisplayGate,
    epoch: Rc<Cell<u64>>,
    ack_gate: Rc<Cell<AckGate>>,
    sequence: u64,
    fetch_pending: Option<u64>,
    receipt_pending: Option<(u64, usize)>,
    frame: Option<(u64, u64, usize, PaintOpportunity)>,
    timer: Option<Timeout>,
    _visibility: Option<VisibilityListener>,
    unavailable: bool,
    receipt_retry_blocked: bool,
    loaded: bool,
    rendered_count: usize,
    segment_nodes: [NodeRef; 12],
}
impl TutorStream {
    fn sync_gate(&mut self, ctx: &Context<Self>) {
        let mut ack = self.ack_gate.get();
        ack.bind(ctx.props().ack_enabled);
        self.ack_gate.set(ack);
        if !ack.allowed {
            self.frame = None;
        }
        let changed = self.state.key != ctx.props().binding;
        if changed {
            self.state = StreamState::new(ctx.props().binding.clone());
            self.loaded = false;
            self.unavailable = false;
            self.receipt_retry_blocked = false;
            self.rendered_count = 0;
        }
        if self.gate.bind(ctx.props().enabled && !hidden(), changed) {
            self.epoch.set(self.gate.epoch);
            self.frame = None;
            self.timer = None;
        }
    }
    fn schedule(&mut self, ctx: &Context<Self>) {
        self.timer = None;
        if self.gate.active
            && (self.unavailable
                || !self.loaded
                || !terminal(self.state.phase())
                || self.state.next_receipt().is_some())
        {
            let link = ctx.link().clone();
            self.timer = Some(Timeout::new(
                if self.unavailable { 1000 } else { 150 },
                move || link.send_message(StreamMsg::Fetch),
            ));
        }
    }
    fn apply(&mut self, ctx: &Context<Self>, result: ApiResult) {
        let old_phase = self.state.phase().to_owned();
        match result.and_then(|value| self.state.apply(&value).map_err(str::to_owned)) {
            Ok(_) => {
                self.loaded = true;
                self.unavailable = false;
                self.receipt_retry_blocked = false;
                if old_phase != self.state.phase() && terminal(self.state.phase()) {
                    ctx.props().on_sync.emit(());
                }
            }
            Err(_) => {
                self.unavailable = true;
                self.receipt_retry_blocked = true;
            }
        }
    }
}
impl Component for TutorStream {
    type Message = StreamMsg;
    type Properties = StreamProps;
    fn create(ctx: &Context<Self>) -> Self {
        let mut component = Self {
            state: StreamState::new(ctx.props().binding.clone()),
            gate: DisplayGate::default(),
            epoch: Rc::new(Cell::new(0)),
            ack_gate: Rc::new(Cell::new(AckGate::default())),
            sequence: 0,
            fetch_pending: None,
            receipt_pending: None,
            frame: None,
            timer: None,
            _visibility: VisibilityListener::new(ctx.link().callback(|_| StreamMsg::Visibility)),
            unavailable: false,
            receipt_retry_blocked: false,
            loaded: false,
            rendered_count: 0,
            segment_nodes: std::array::from_fn(|_| NodeRef::default()),
        };
        component.sync_gate(ctx);
        ctx.link().send_message(StreamMsg::Fetch);
        component
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        self.sync_gate(ctx);
        if self.gate.active
            && (old.binding != ctx.props().binding
                || old.enabled != ctx.props().enabled
                || old.head_revision != ctx.props().head_revision)
        {
            ctx.link().send_message(StreamMsg::Fetch);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, message: StreamMsg) -> bool {
        self.sync_gate(ctx);
        match message {
            StreamMsg::Visibility => {
                if self.gate.active {
                    ctx.link().send_message(StreamMsg::Fetch);
                }
            }
            StreamMsg::Fetch => {
                if !self.gate.active || self.fetch_pending.is_some() {
                    return false;
                }
                self.timer = None;
                self.sequence = self.sequence.wrapping_add(1);
                let sequence = self.sequence;
                let epoch = self.gate.epoch;
                let key = self.state.key.clone();
                self.fetch_pending = Some(sequence);
                ctx.link().send_future(async move {
                    let result = api(&key.path(), "GET", None).await;
                    StreamMsg::Snapshot(sequence, epoch, key, result)
                });
                return false;
            }
            StreamMsg::Snapshot(sequence, epoch, key, result) => {
                if self.fetch_pending != Some(sequence) {
                    return false;
                }
                self.fetch_pending = None;
                if self.gate.accepts(epoch) && self.state.key == key {
                    self.apply(ctx, result);
                }
                self.schedule(ctx);
            }
            StreamMsg::Frame(epoch, ack_revision, sequence) => {
                if self.frame.as_ref().map(|(e, a, s, _)| (*e, *a, *s))
                    != Some((epoch, ack_revision, sequence))
                {
                    return false;
                }
                self.frame = None;
                if !self.gate.accepts(epoch)
                    || !self.ack_gate.get().accepts(ack_revision)
                    || hidden()
                    || self.receipt_pending.is_some()
                    || self.receipt_retry_blocked
                    || !display_opportunity(&ctx.props().viewport, &self.segment_nodes[sequence])
                    || !self.state.commit(sequence)
                {
                    return false;
                }
                self.sequence = self.sequence.wrapping_add(1);
                let request = self.sequence;
                let key = self.state.key.clone();
                let current_epoch = self.epoch.clone();
                let ack_gate = self.ack_gate.clone();
                let viewport = ctx.props().viewport.clone();
                let segment = self.segment_nodes[sequence].clone();
                self.receipt_pending = Some((request, sequence));
                ctx.link().send_future(async move {
                    let result = if current_epoch.get() != epoch
                        || !ack_gate.get().accepts(ack_revision)
                        || hidden()
                        || !display_opportunity(&viewport, &segment)
                    {
                        None
                    } else {
                        Some(
                            api(
                                &format!("{}/display", key.path()),
                                "POST",
                                Some(json!({
                                    "generation":key.generation,"sequence":sequence
                                })),
                            )
                            .await,
                        )
                    };
                    StreamMsg::Receipt(request, epoch, key, sequence, result)
                });
                return false;
            }
            StreamMsg::Receipt(request, epoch, key, sequence, result) => {
                if self.receipt_pending != Some((request, sequence)) {
                    return false;
                }
                self.receipt_pending = None;
                if self.gate.accepts(epoch) && self.state.key == key {
                    // Revoked before sending is a local control decision,
                    // not a failed network connection or a display fact.
                    if let Some(result) = result {
                        self.apply(ctx, result);
                        if self.state.confirmed() <= sequence {
                            self.unavailable = true;
                            self.receipt_retry_blocked = true;
                        } else if self.state.next_receipt() == Some(sequence) {
                            // Final transaction is still pending. Poll before its
                            // idempotent retry instead of resending at frame rate.
                            self.receipt_retry_blocked = true;
                        }
                    }
                }
                self.schedule(ctx);
            }
        }
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _: bool) {
        self.sync_gate(ctx);
        let count = self.state.display_count();
        if count != self.rendered_count {
            self.rendered_count = count;
            ctx.props().on_progress.emit(self.state.key.turn.clone());
        }
        if self.gate.active
            && self.ack_gate.get().allowed
            && self.frame.is_none()
            && self.receipt_pending.is_none()
            && !self.receipt_retry_blocked
        {
            if let Some(sequence) = self.state.next_receipt() {
                let epoch = self.gate.epoch;
                let ack_revision = self.ack_gate.get().revision;
                self.frame = PaintOpportunity::new(
                    ctx.link()
                        .callback(move |_| StreamMsg::Frame(epoch, ack_revision, sequence)),
                )
                .map(|frame| (epoch, ack_revision, sequence, frame));
            }
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        let mut ack = self.ack_gate.get();
        ack.bind(false);
        self.ack_gate.set(ack);
        self.frame = None;
        self.timer = None;
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let en = ctx.props().ui_language == "en";
        let phase = if self.loaded {
            self.state.phase()
        } else {
            &ctx.props().head_phase
        };
        let status = if incomplete(phase) {
            if en {
                "Reply incomplete · displayed text retained"
            } else {
                "回复未完成 · 已显示文字保留"
            }
        } else if phase == "completed" {
            if en {
                "Reply complete"
            } else {
                "回复已完成"
            }
        } else {
            if en {
                "Reply arriving in segments"
            } else {
                "回复逐段显示中"
            }
        };
        html! {<>
            <div class="message-bubble">
                {if self.loaded {html!{for self.state.visible_segments().iter().enumerate().map(|(sequence,text)|html!{<span key={sequence.to_string()} ref={self.segment_nodes[sequence].clone()}>{text}</span>})}}
                 else {html!{&ctx.props().persisted_text}}}
            </div>
            <small class={classes!("stream-status", incomplete(phase).then_some("stream-incomplete"))} role="status">{status}</small>
            {if self.unavailable {html!{<small class="stream-status">{if en {"Connection unavailable. Displayed text is retained; reconnecting…"} else {"连接暂不可用，保留已显示文字，正在重连…"}}</small>}} else {Html::default()}}
        </>}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_audio_revokes_queued_ack_but_can_resume_text_with_a_new_receipt() {
        let mut gate = AckGate::default();
        gate.bind(true);
        let old = gate.revision;
        assert!(gate.accepts(old));
        gate.bind(false);
        assert!(!gate.accepts(old));
        gate.bind(true);
        assert!(!gate.accepts(old));
        assert!(gate.accepts(gate.revision));
    }
    #[test]
    fn display_requires_the_segment_to_intersect_both_scrollport_and_screen() {
        let rect = |left, top, right, bottom| DisplayRect {
            left,
            top,
            right,
            bottom,
        };
        let screen = rect(0.0, 0.0, 800.0, 600.0);
        let conversation = rect(100.0, 100.0, 500.0, 400.0);
        assert!(intersects_display(
            rect(120.0, 300.0, 400.0, 340.0),
            conversation,
            screen
        ));
        assert!(!intersects_display(
            rect(120.0, 410.0, 400.0, 450.0),
            conversation,
            screen
        ));
        assert!(!intersects_display(
            rect(120.0, 50.0, 400.0, 90.0),
            conversation,
            screen
        ));
        assert!(!intersects_display(
            rect(510.0, 300.0, 600.0, 340.0),
            conversation,
            screen
        ));
        assert!(!intersects_display(
            rect(120.0, 700.0, 400.0, 740.0),
            rect(100.0, 650.0, 500.0, 900.0),
            screen
        ));
        assert!(!intersects_display(
            rect(0.0, 0.0, 0.0, 0.0),
            conversation,
            screen
        ));
        // Intersection is only an opportunity to display part of the element;
        // it is deliberately not evidence that the learner read the text.
        assert!(intersects_display(
            rect(120.0, 380.0, 400.0, 450.0),
            conversation,
            screen
        ));
    }
    #[test]
    fn hidden_history_and_owner_changes_revoke_old_display_callbacks() {
        let mut gate = DisplayGate::default();
        gate.bind(true, false);
        let first = gate.epoch;
        assert!(gate.accepts(first));
        gate.bind(false, false);
        assert!(!gate.accepts(first));
        gate.bind(true, false);
        assert!(!gate.accepts(first));
        let resumed = gate.epoch;
        gate.bind(true, true);
        assert!(!gate.accepts(resumed));
        assert!(gate.accepts(gate.epoch));
    }
}
