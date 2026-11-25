# Activity history and explicit continuation

Implementation slice: 2026-11-04. Rust/Yew UI only; Store and HTTP ownership are implemented separately. This document records the UI contract, not a browser acceptance result.

## Interaction

The activity panel offers **查看过去的活动 / Browse past activities**. The directory keeps at most 20 rows and 64 previous-page cursors; moving pages drops the old page text. Goals remain in their original language. UI labels follow the independently selected interface language. IDs are used for requests and component ownership, not displayed to the learner.

**查看记录 / View records** opens the selected session in the existing read-only `ConversationHistory`, with its own 50-message pages. The current session and composer do not change. The caption explicitly says that the draft still belongs to the current activity. Hiding current messages uses the existing history display gate, closes automatic segment speech, clears queued whole-reply reading and pauses voice rotation. Returning to current messages does not reopen the microphone or reopen a closed automatic speech stream.

Only a finished activity offers **继续这次活动 / Continue this activity**. This is an explicit request for a new session and a new activity, with an association to the selected source. An unfinished row is view-only; this does not change the existing latest-unfinished profile-selection behavior.

Before continuing, App checks the current send, input draft, held transcript, uncertain submission, comparison draft and in-flight save. A comparison draft keeps its existing focus-and-save/clear recovery. An active recording or transcription must be ended and reviewed first. Unsaved module/observation input must be explicitly saved in the teacher workbench. The new continuation does not automatically submit reports, accept plans, apply hardware settings or send a teaching question.

The new activity displays its historical source and a link back to the source records. A supplied historical plan is labeled **当时的待做记录 · 需确认 / Earlier plan · confirm its current status**. It is not presented as currently incomplete. The notice asks the user to confirm today's goal, modules and setup, and whether the earlier plan was subsequently completed.

## Frozen API contract

- `GET /api/learners/{lid}/sessions?limit=20&before=<positive i64>` returns `{learner_id,namespace,items,next_before}`. Each item has `session_id`, `activity_id`, `created_at`, `status`, original `goal`, and `continued_from:null|{session_id,activity_id,goal}`. The UI rejects a non-progressing cursor, oversized page or mismatched learner; late requests cannot populate a different learner or a leave-and-return window.
- `POST /api/learners/{lid}/sessions/{old_sid}/continue` sends `{client_submission_id,expected_current_session_id,expected_generation}` captured for that click. The nonce is stable for all retries of that operation.
- A success returns `{learner_id,session_id,activity_id,client_submission_id,replayed,session}`. The full session and `/api/state` expose `continuation.client_submission_id`, `source_session_id`, `source_activity_id` and `continued_from`. The UI verifies the learner, nonce, source IDs and new session/activity identities. It does not match by goal text.

The continuation request has a separate owner from the general action sequence: pressing Stop does not lose its unknown-result receipt. A matching state snapshot can resolve a request whose POST response was lost. A late response cannot reopen a past session after a different learner takes ownership. The POST result itself never installs its session into App; App refreshes the authoritative current state.

The shared HTTP transport retains the existing timeouts and ordinary `ApiResult` behavior. Only the continuation helper distinguishes explicit 4xx rejection from network/5xx/invalid-response uncertainty. Rejection says **请求未被接受，请核对当前活动 / The request was not accepted. Check the current activity.** It does not claim that an earlier unknown attempt never committed or was rolled back. Unknown results keep one intent and offer a state refresh or the identical request; they do not generate a second nonce or automatically retry. A same-learner foreign-session snapshot alone may have started before the POST and does not resolve the request; an explicit replay rejection releases it and refreshes current state.

## Verification status

Added six pure regression tests: directory owner/ABA isolation; bounded paging and cursor validation; same-payload retry and unfinished-source rejection; exact new identity/nonce/source recognition; definitive-rejection cleanup and late-callback isolation; HTTP rejection versus uncertain commit. The existing busy-action test includes continuation. Root's `workspace-thirty-eighth.log` passed 760 tests, including 128 UI tests, with 7 ignored and no warnings. Native release and `wasm-continuation-first.log` builds passed. `release-smoke-sixteenth/report.json` confirms the new executable serves the staged UI assets and preserves the existing isolated HTTP workflow. This is not rendering or browser interaction verification.

Browser interaction, visual narrow/wide checks, real microphone/model/device use: **NOT_RUN** in this slice. The existing browser tool URL-verification boundary remains in effect. No backend, Store, Cargo, installed UI bundle, model, policy or user-data changes were made by this UI implementation.

Backups: `runtime/goal-backups/20261104/ui_review/activity-history/`.
