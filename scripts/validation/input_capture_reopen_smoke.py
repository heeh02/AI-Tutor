"""Explicit, isolated Windows microphone open/reopen and stale-discard smoke.

This script DOES open the sole stable input twice, keeping each capture in RAM
for at least 300 ms, then discards it. It never calls record/stop, sends text,
saves PCM, runs ASR, or requests speaker/camera/PTZ output. Authoring/importing
it opens nothing. Root reviews and separately invokes --capture-reopen-only.

Start and Discard timings are HTTP request RTT, not physical device timing.
Two successful Start receipts and subsequent idle status demonstrate this
process's open/reopen path, not acoustic quality, endpoint accuracy or unplug.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import time

from database_compatibility_smoke import (
    SmokeFailure, isolated_environment, require, sha256, write_json,
)
from input_device_binding_smoke import (
    ALLOWED, InputWorkstation, catalog, quiet_state,
)


START = "/api/audio/record/start"
DISCARD = "/api/audio/record/discard"
CAPTURE_SECONDS = .300
PROTECTED = (
    "bin/ai-tutor.exe", "bin/tutorctl.exe", "runtime/server.json",
    "runtime/speech-service.json", "runtime/tts-service.json",
    "runtime/local-asr-policy.json", "runtime/local-speech-policy.json",
    "runtime/memory-policy.json", "runtime/tutor-delivery.json",
)
TICKET_FIELDS = ("session_id", "session_generation", "language", "recording_generation")


def protected_hashes(root):
    # Read bytes only for hashes; never parse or expose installed credentials.
    return {name: sha256(root / name) if (root / name).is_file() else None
            for name in PROTECTED}


def audio_projection(audio):
    return {key: audio.get(key) for key in (
        "state", "recording", "recording_generation", "capture_pending_result",
        "input_topology_revision", "stable_binding_available", "asr_ready",
        "asr_streaming_state", "raw_audio_saved",
    )}


def check_isolation(state):
    require(state["camera"]["state"] == "closed", "camera_not_closed")
    require(state["ptz"]["mode"] == "off", "ptz_not_off")
    provider = state["kimi"]
    require(provider.get("available") is False and provider.get("running", 0) == 0
            and provider["metrics"].get("requests", 0) == 0, "model_request_observed")
    for field in ("local_asr", "local_speech"):
        local = state[field]
        require(local.get("phase") == "disabled" and local.get("active") is False
                and local.get("ready") is False and local.get("warmup_completed") is False,
                field + "_not_disabled")
    audio = state["audio"]
    require(audio.get("asr_ready") is False, "unexpected_asr_backend_available")
    # Revoking a capture resets this projection to idle even when no ASR client
    # exists. Readiness and the disabled lifecycle remain independently checked.
    require(audio.get("asr_streaming_state") in (None, "disabled", "idle"),
            "unexpected_streaming_asr_activity")
    require(audio.get("raw_audio_saved") is False, "raw_audio_save_reported")
    require(audio.get("asr_partial") is None, "unexpected_transcript")
    require(audio.get("state") in ("idle", "recording"), "unexpected_audio_output_or_transcription")


def capture_identity(state):
    return {"session_id": state["session"]["session_id"],
            "session_generation": state["session"]["generation"],
            "recording_generation": state["audio"]["recording_generation"],
            "recording": state["audio"]["recording"],
            "audio_state": state["audio"]["state"]}


class CaptureWorkstation(InputWorkstation):
    def call(self, method, path, body=None):
        # Start and Discard are deliberately reachable only through the methods
        # below. No record/stop, provider, output or other device route is allowed.
        require((method, path) in ALLOWED, "diagnostic_endpoint_not_allowed")
        return self._request(method, path, body)

    def start_capture(self, session_id, expected_generation, keys, revision):
        require(len(keys) == 1, "capture_requires_exactly_one_stable_input")
        status = self.call("GET", "/api/audio/status")
        require(status.get("stable_binding_available") is True
                and status.get("input_topology_revision") == revision,
                "topology_changed_before_capture")
        require(status.get("recording") is False
                and status.get("capture_pending_result") is False,
                "previous_capture_not_released")
        body = {"session_id": session_id, "expected_generation": expected_generation,
                "device_key": next(iter(keys)), "expected_input_topology_revision": revision,
                "endpoint_mode": "voice_endpoint_v1"}
        started = time.perf_counter()
        response = self._request("POST", START, body)
        rtt = (time.perf_counter() - started) * 1000
        require(response.get("endpoint_mode") == "voice_endpoint_v1",
                "explicit_endpoint_mode_not_acknowledged")
        ticket = {field: response.get(field) for field in TICKET_FIELDS}
        require(ticket["session_id"] == session_id
                and ticket["session_generation"] == expected_generation + 1
                and type(ticket["recording_generation"]) is int
                and ticket["recording_generation"] > 0
                and ticket["language"] in ("zh", "en"), "capture_ticket_invalid")
        return ticket, rtt

    def discard_capture(self, ticket, expected_status=200):
        require(set(ticket) == set(TICKET_FIELDS), "discard_requires_complete_ticket")
        started = time.perf_counter()
        response = self._request("POST", DISCARD, ticket, expected_status=expected_status)
        rtt = (time.perf_counter() - started) * 1000
        if expected_status == 200:
            require(response.get("ok") is True
                    and all(response.get(field) == ticket[field] for field in TICKET_FIELDS),
                    "discard_receipt_mismatch")
        return rtt


def assert_capturing(app, ticket, revision):
    state = app.call("GET", "/api/state")
    check_isolation(state)
    require(capture_identity(state) == {
        "session_id": ticket["session_id"], "session_generation": ticket["session_generation"],
        "recording_generation": ticket["recording_generation"],
        "recording": True, "audio_state": "recording",
    }, "capture_or_session_identity_changed")
    require(state["audio"].get("input_topology_revision") == revision,
            "input_topology_changed_while_capturing")
    require(state["audio"].get("recording_endpoint") is None
            and state["audio"].get("capture_pending_result") is False,
            "capture_ended_before_explicit_discard")
    return state


def await_discard_status(app, ticket, revision):
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        state = app.call("GET", "/api/state")
        check_isolation(state)
        audio = state["audio"]
        require(state["session"]["session_id"] == ticket["session_id"]
                and state["session"]["generation"] == ticket["session_generation"],
                "discard_changed_teaching_session")
        require(audio.get("input_topology_revision") == revision,
                "input_topology_changed_during_discard")
        require(audio["recording_generation"] == ticket["recording_generation"] + 1,
                "discard_did_not_revoke_exact_audio_token")
        if audio.get("recording") is False and audio.get("state") == "idle":
            require(audio.get("capture_pending_result") is False
                    and audio.get("recording_endpoint") is None,
                    "discard_retained_pending_capture")
            return audio_projection(audio)
        time.sleep(.02)
    raise SmokeFailure("discard_idle_status_not_observed")


def run(root, executable, ui, output):
    require(os.name == "nt", "windows_only_diagnostic")
    require(root.is_dir() and executable.is_file() and (ui / "index.html").is_file(),
            "explicit_input_path_missing")
    require(not output.exists() and not output.is_relative_to(ui), "output_must_be_fresh_outside_ui")
    output.mkdir(parents=True, exist_ok=False)
    fixture = output / "fixture"
    report = {
        "test_type": "input_capture_open_reopen_and_stale_discard", "passed": False,
        "outcome": "not_started", "synthetic_fixture_only": True,
        "production_database_opened": False, "production_credentials_parsed_or_logged": False,
        "protected_files_read_only_sha256": True, "models_staged": False,
        "asr_or_model_request_requested": False, "raw_audio_saved": False,
        "capture_mode": "voice_endpoint_v1", "audio_sample_or_signal_inspected": False,
        "speaker_camera_ptz_requested": False, "record_stop_transcription_requested": False,
        "capture_start_requests": 0, "successful_open_receipts": 0,
        "successful_reopen_receipts": 0, "sample_wait_requested_ms_per_open": 300,
        "timing_method": "HTTP request RTT; final status is owner state, not measured physical closure time",
        "scope_not_run": ["speech_endpoint_accuracy", "acoustic_quality", "ASR",
                          "speaker_output", "unplug_replug", "default_device_change",
                          "multiple_input_selection", "full_voice_loop", "physical_close_latency"],
        "topology_boundary": "capture default changes filtered; other output add/remove/state/property may conservatively invalidate",
        "executable_sha256": sha256(executable), "script_sha256": sha256(Path(__file__)),
        "helper_sha256": {name: sha256(Path(__file__).with_name(name)) for name in (
            "database_compatibility_smoke.py", "input_device_binding_smoke.py")},
        "enumeration": [], "captures": [], "owner": {},
    }
    owned, active_ticket = None, None
    policies, protected_before = {}, None
    checks_ok, skipped = False, None
    try:
        protected_before = protected_hashes(root)
        report["protected_before_sha256"] = protected_before
        shutil.copytree(ui, fixture / "frontend/dist")
        runtime = fixture / "runtime"
        runtime.mkdir()
        policies = {
            "local-asr-policy.json": {"protocol_version": 1, "mode": "disabled"},
            "local-speech-policy.json": {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"},
        }
        for name, policy in policies.items():
            write_json(runtime / name, policy)
        require(not (fixture / "models").exists(), "fixture_must_have_no_models")
        require(not (runtime / "speech-service.json").exists()
                and not (runtime / "tts-service.json").exists(), "fixture_has_legacy_service_descriptor")
        owned = CaptureWorkstation(executable, fixture, isolated_environment(fixture), report["owner"])
        initial = owned.ready()
        require(initial["learners"] == [], "fresh_fixture_has_learners")
        check_isolation(quiet_state(owned, report["owner"]))
        first_keys, first = catalog(owned.call("GET", "/api/audio/devices"))
        second_keys, second = catalog(owned.call("GET", "/api/audio/devices"))
        report["enumeration"] = [first, second]
        require(first_keys == second_keys, "device_keys_changed_between_lists")
        if len(first_keys) != 1:
            skipped = "no_input_devices" if not first_keys else "multiple_input_devices_no_selection"
        else:
            revision = first["input_topology_revision"]
            require(second["input_topology_revision"] == revision, "topology_changed_between_lists")
            learner = owned.call("POST", "/api/learners",
                                 {"name": "SYNTHETIC CAPTURE REOPEN", "namespace": "simulator"})
            session = owned.call("POST", "/api/sessions",
                                 {"learner_id": learner["learner_id"], "resume": True})
            previous_ticket = None
            for index in range(2):
                baseline = quiet_state(owned, report["owner"])
                check_isolation(baseline)
                capture = {"index": index + 1, "status": "start_requested"}
                report["captures"].append(capture)
                report["capture_start_requests"] += 1
                ticket, start_rtt = owned.start_capture(session["session_id"],
                    baseline["session"]["generation"], first_keys, revision)
                active_ticket = ticket
                opened = time.perf_counter()
                report["successful_open_receipts"] += 1
                report["successful_reopen_receipts"] += int(index == 1)
                capture.update(status="capturing", start_request_rtt_ms=start_rtt,
                               recording_generation=ticket["recording_generation"],
                               session_generation=ticket["session_generation"])
                assert_capturing(owned, ticket, revision)
                if previous_ticket is not None:
                    before = capture_identity(assert_capturing(owned, ticket, revision))
                    stale_rtt = owned.discard_capture(previous_ticket, expected_status=400)
                    after = capture_identity(assert_capturing(owned, ticket, revision))
                    require(before == after, "old_discard_affected_reopened_capture")
                    capture["stale_discard"] = {"http_status": 400, "request_rtt_ms": stale_rtt,
                        "current_recording_token_and_session_unchanged": True}
                remaining = CAPTURE_SECONDS - (time.perf_counter() - opened)
                if remaining > 0:
                    time.sleep(remaining)
                assert_capturing(owned, ticket, revision)
                capture["start_receipt_to_discard_request_ms"] = (time.perf_counter() - opened) * 1000
                capture["discard_request_rtt_ms"] = owned.discard_capture(ticket)
                active_ticket = None
                capture["final_audio_status"] = await_discard_status(owned, ticket, revision)
                capture["status"] = "discarded_idle_observed"
                previous_ticket = ticket
            checks_ok = True
        check_isolation(quiet_state(owned, report["owner"]))
    except Exception as error:
        report.update(error_type=type(error).__name__, error_code=str(error)
                      if isinstance(error, SmokeFailure) else "unexpected_exception_no_body_logged")
    finally:
        if owned is not None:
            # A failed assertion after Start still discards only its exact ticket.
            # If Start timed out without a receipt, owned main shutdown invalidates
            # capture before cleanup; no guessed token or broad foreign request.
            if active_ticket is not None and owned.process.poll() is None:
                try:
                    report["failure_exact_discard_request_rtt_ms"] = owned.discard_capture(active_ticket)
                except Exception:
                    report["failure_exact_discard_unconfirmed"] = True
            try:
                owned.stop()
                stage = report["owner"]
                require(stage.get("shutdown_http_accepted") is True and stage.get("exit_code") == 0
                        and stage.get("forced_owned_process_stop") is False,
                        "owned_main_shutdown_not_clean")
                require(not owned.descriptor.exists(), "owned_descriptor_remained")
                report["descriptor_removed"] = True
                report["descriptor_removed_by"] = "diagnostic_after_exit" if stage.get("owned_credential_removed") else "main"
            except Exception as error:
                report["cleanup_error_code"] = str(error) if isinstance(error, SmokeFailure) else "owned_cleanup_exception"
        if owned is None or owned.process.poll() is not None:
            removed = []
            for name, expected in policies.items():
                path = fixture / "runtime" / name
                try:
                    if path.is_file() and json.loads(path.read_text(encoding="utf-8")) == expected:
                        path.unlink()
                        removed.append(name)
                except (OSError, ValueError):
                    report["cleanup_error_code"] = "owned_disabled_policy_cleanup_failed"
            report["own_disabled_policies_removed"] = removed
        else:
            report["cleanup_error_code"] = "owned_main_still_running"
        try:
            after = protected_hashes(root)
            report["protected_after_sha256"] = after
            require(protected_before is not None and after == protected_before,
                    "protected_installation_descriptor_or_policy_changed")
            require(sha256(executable) == report["executable_sha256"], "executable_changed")
            report["protected_files_unchanged"] = True
        except Exception as error:
            report["protection_error_code"] = str(error) if isinstance(error, SmokeFailure) else "protected_hash_check_failed"
        failed = any(report.get(key) for key in ("error_code", "cleanup_error_code", "protection_error_code"))
        report["passed"] = checks_ok and not failed
        report["outcome"] = "failed" if failed else "open_reopen_and_stale_discard_passed" if checks_ok else skipped or "incomplete"
        if skipped:
            report["capture_open_reopen"] = "NOT_RUN"
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "outcome": report["outcome"],
                          "report": str(output / "report.json"), "error_code": report.get("error_code"),
                          "cleanup_error_code": report.get("cleanup_error_code"),
                          "protection_error_code": report.get("protection_error_code")}))
    return 0 if report["passed"] else 2 if skipped and not failed else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("root", "executable", "ui", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--capture-reopen-only", action="store_true", required=True,
                        help="Explicitly open the sole stable microphone twice; RAM capture then discard")
    args = parser.parse_args()
    return run(args.root.resolve(), args.executable.resolve(), args.ui.resolve(), args.output.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
