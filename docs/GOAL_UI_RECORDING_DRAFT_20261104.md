# Bound manual recording and preserved text drafts

This slice keeps manual push-to-talk, editable final transcription and manual Send. It does not enable automatic sending, continuous microphone capture, VAD or acoustic echo cancellation.

## Recording contract

The UI starts recording with `session_id`, `expected_generation`, and an optional selected `device_id`. It no longer sends the redundant session-cancel request before starting; the backend performs the single validated cancellation/bump. The playback-stop request remains before recording.

The start acknowledgement binds the exact `session_id`, `session_generation`, `language`, and `recording_generation`. Valid languages include `zh`, `zh-CN`, `en`, and `en-US`; payloads preserve the returned value. Stop, final-result validation and precise discard all use that complete ticket. Live-caption ownership includes the same ticket, and the 20-second UI timer is bound to its original recording instead of stopping whichever recording happens to be current later.

An ordinary state poll may see the recording's own new teaching generation before the start acknowledgement. The pending start is therefore not cancelled merely because the generation advanced. Once the acknowledgement arrives, it must match the current intent and the newest observed generation/language. Its generation is retained as a local lower bound for subsequent guarded requests, so a pre-start state response cannot roll it back. Higher generations or changed ownership revoke active recording intent.

Visibility loss revokes recording/ASR intent and sends a precise discard when a bound ticket exists. A late start acknowledgement is used only to discard its original recording. Showing the page again never reopens a microphone. Setting changes and other existing cancellation actions also discard the bound recording; no cleanup falls back to a broad session cancel.

## Draft handling

`recording_draft.rs` tracks input revision independently of audio cancellation and session ownership. Final text can fill the composer only when it was empty at the start, remains empty, and its revision has not changed. Editing and then clearing the input still prevents replacement.

Otherwise, the final text goes into one **Transcript waiting for review** slot. The user's input remains unchanged. Explicit Replace, Append or Dismiss resolves that slot; adopting text never sends it. A pending transcript must be resolved before another recording can begin. Appending beyond the existing 4,000-character message limit leaves both original texts intact and asks the user to shorten the draft.

A separate **Unconfirmed send result** slot retains the original submitted text for every Send error. The UI does not classify errors by message wording or assume that the server failed to save a request. Even if an untouched empty composer is restored, the review slot blocks another Send until explicitly resolved. If the user has edited or cleared the draft, it remains untouched. No retry is automatic.

Only one Send HTTP request may remain pending. Stop controls remain available, but changing the general busy-action sequence cannot discard the original Send result. A Send error after Stop/SpeechStop/PTZStop is still retained for the same session owner. A separate session-owner revision rejects late results after switching away and back to the same session.

Navigation to another profile/activity or finishing the current activity is blocked while a Send is pending, input is nonempty, or either review slot contains text. The two slots are independent and cannot overwrite one another. Clear input and Dismiss remain available even if another client has finished the activity, avoiding a navigation dead end. State-driven owner changes clear old review ownership rather than inserting old text into the new profile.

Send now includes `expected_generation` while preserving existing `auto_speak` and `client_submission_id` behavior. The backend validates that generation before its cancellation/bump. This slice does not add a general retry or idempotency framework.

## Validation boundary

Twelve new pure `recording_draft` tests cover untouched/edited/existing drafts, generation observation before start acknowledgement, language/owner invalidation, old capture callbacks, exact four-field payloads and aliases, two independent review slots, character-limit preservation, visibility cancellation, all-error send review, Stop followed by a late Send error, and session ABA rejection. One added caption regression rejects a mismatched language/session generation.

Implementation formatting was checked with `rustfmt`; native/WASM compilation and tests are delegated to root. `fixtures/recording-draft-20261104.json` contains synthetic acceptance tasks, all explicitly NOT_RUN. No browser, real microphone, audio playback or model was used by this UI implementation slice.

Root validation subsequently passed in `workspace-twenty-second.log`: 597 workspace tests, including 104 UI tests, with four explicit ignored tests and no failures. Native release and WASM builds passed. The preceding full-build attempt exposed an ambiguous Yew `update` trait call in the bound timeout handler; it was corrected with an explicit `Component` call and the failed log retained. The actual release HTTP smoke also passed stale recording/Send rejection without changing session or recording generation. These checks do not replace the still-NOT_RUN browser and device tasks.
