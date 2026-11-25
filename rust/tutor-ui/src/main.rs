#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod activity_history;
mod activity_history_state;
mod activity_summary;
mod api;
mod app;
mod camera;
mod comparison_memory;
mod comparison_record;
mod comparison_state;
mod confirm_forget;
mod conversation_history;
mod evidence_labels;
mod experiment_comparison;
mod help_preference;
mod historical_comparisons;
mod i18n;
mod live_transcript;
mod local_asr_status;
mod local_memory_status;
mod local_speech_status;
mod memory_evidence;
mod memory_review;
mod model;
mod module_inventory;
mod operation_notice;
mod preview_browser;
mod preview_stream;
mod recording_draft;
mod recording_review;
mod robot_companion;
mod speech_intent;
mod tutor_stream;
mod tutor_stream_state;
mod view;
mod voice_loop;
mod voice_loop_controls;

#[cfg(target_arch = "wasm32")]
fn main() {
    let root = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("root"))
        .expect("AI Tutor root element is missing");
    yew::Renderer::<app::App>::with_root(root).render();
}
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
