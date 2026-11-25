"""Explicit Windows input enumeration and invalid-binding HTTP smoke.

This diagnostic never sends a valid capture start, opens a capture stream,
plays audio or invokes a model. Importing it does not start a workstation.
Root reviews and runs it separately from compilation and other measurements.
"""
import argparse
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import uuid

from database_compatibility_smoke import (
    MAX_RESPONSE_BYTES, OwnedWorkstation, SmokeFailure, check_idle,
    isolated_environment, require, sha256, write_json,
)


ALLOWED = {
    ("GET", "/api/state"), ("GET", "/api/audio/status"),
    ("GET", "/api/audio/devices"), ("POST", "/api/learners"),
    ("POST", "/api/sessions"), ("POST", "/api/shutdown"),
}
START = "/api/audio/record/start"


class InputWorkstation(OwnedWorkstation):
    def _request(self, method, path, body=None, expected_status=200):
        require(self.process.poll() is None and self.token is not None,
                "owned_process_not_available")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None,
                {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + self.token})
            response = connection.getresponse()
            raw = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(raw) <= MAX_RESPONSE_BYTES, "http_response_over_limit")
            require(response.status == expected_status,
                    f"http_status_expected_{expected_status}_observed_{response.status}")
            return json.loads(raw)
        finally:
            connection.close()

    def call(self, method, path, body=None):
        require((method, path) in ALLOWED, "diagnostic_endpoint_not_allowed")
        return self._request(method, path, body)

    def reject_start(self, body, known_keys, revision):
        # The only call site allowed to reach record/start proves the pair is
        # invalid against the observed registry before making its HTTP request.
        require(set(body) == {"session_id", "expected_generation", "device_key",
                              "expected_input_topology_revision"},
                "invalid_start_body_contract")
        require(body["device_key"] not in known_keys
                or body["expected_input_topology_revision"] != revision,
                "refused_to_send_valid_capture_start")
        current = self.call("GET", "/api/audio/status")
        require(current.get("stable_binding_available") is True
                and current.get("input_topology_revision") == revision,
                "topology_changed_before_invalid_start")
        require(current.get("recording") is False, "unexpected_capture_before_invalid_start")
        self._request("POST", START, body, expected_status=400)


def quiet_state(app, stage):
    state = app.call("GET", "/api/state")
    check_idle(state, stage)
    asr = state.get("local_asr", {})
    require(asr.get("phase") == "disabled" and asr.get("active") is False
            and asr.get("ready") is False and asr.get("warmup_completed") is False,
            "local_asr_not_disabled")
    return state


def counters(state):
    audio = state["audio"]
    return {"teaching_generation": state["session"]["generation"],
            "recording_generation": audio["recording_generation"],
            "provider_requests": state["kimi"]["metrics"].get("requests", 0),
            "provider_running": state["kimi"].get("running", 0),
            "recording": audio["recording"], "audio_state": audio["state"]}


def catalog(rows):
    require(isinstance(rows, list), "device_list_not_array")
    keys = []
    revisions = set()
    for row in rows:
        require(isinstance(row, dict) and row.get("stable_binding_available") is True,
                "stable_binding_unavailable")
        key = row.get("device_key")
        require(isinstance(key, str) and 1 <= len(key) <= 96 and key.isascii(),
                "device_key_invalid")
        revision = row.get("input_topology_revision")
        require(type(revision) is int and revision > 0, "topology_revision_invalid")
        keys.append(key)
        revisions.add(revision)
    require(len(set(keys)) == len(keys), "duplicate_device_key")
    require(len(revisions) <= 1, "mixed_topology_revision_in_one_list")
    # No names, endpoint IDs, raw UUIDs, formats or hardware properties persist.
    digest = hashlib.sha256(json.dumps(sorted(keys), separators=(",", ":")).encode()).hexdigest()
    return set(keys), {"count": len(keys), "key_set_sha256": digest,
                       "input_topology_revision": next(iter(revisions), None)}


def run(root, executable, ui, output):
    require(os.name == "nt", "windows_only_diagnostic")
    require(root.is_dir() and executable.is_file() and (ui / "index.html").is_file(),
            "explicit_input_path_missing")
    require(not output.exists() and not output.is_relative_to(ui), "output_must_be_fresh_outside_ui")
    output.mkdir(parents=True, exist_ok=False)
    fixture = output / "fixture"
    report = {"test_type": "input_device_binding_enumeration_only", "passed": False,
              "outcome": "not_started", "executable_sha256": sha256(executable),
              "helper_sha256": sha256(Path(__file__).with_name("database_compatibility_smoke.py")),
              "production_database_opened": False, "production_credentials_read": False,
              "synthetic_fixture_only": True, "models_staged": False,
              "valid_capture_start_sent": False, "capture_stream_open_requested": False,
              "speaker_camera_ptz_requested": False, "model_request_requested": False,
              "real_open_reopen_unplug": "NOT_RUN",
              "topology_boundary": "capture default changes filtered; other output add/remove/state/property may conservatively invalidate",
              "enumeration": [], "invalid_binding_checks": [], "owner": {}}
    owned = None
    policies = {}
    checks_ok = False
    no_devices = False
    try:
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
        owned = InputWorkstation(executable, fixture, isolated_environment(fixture), report["owner"])
        initial = owned.ready()
        require(initial["learners"] == [], "fresh_fixture_has_learners")
        require(initial["audio"].get("stable_binding_available") is False,
                "startup_unexpectedly_initialized_device_catalog")
        quiet_state(owned, report["owner"])
        report["startup_stable_binding_available"] = False
        first_keys, first = catalog(owned.call("GET", "/api/audio/devices"))
        report["enumeration"].append(first)
        first_status = owned.call("GET", "/api/audio/status")
        first_revision = first_status.get("input_topology_revision")
        require(first_status.get("stable_binding_available") is True
                and type(first_revision) is int and first_revision > 0,
                "first_list_notification_registration_unavailable")
        second_keys, second = catalog(owned.call("GET", "/api/audio/devices"))
        report["enumeration"].append(second)
        require(first_keys == second_keys, "device_keys_changed_between_lists")
        status = owned.call("GET", "/api/audio/status")
        require(status.get("stable_binding_available") is True, "notification_registration_unavailable")
        revision = status.get("input_topology_revision")
        require(type(revision) is int and revision > 0, "status_topology_revision_invalid")
        require(first_revision == revision, "topology_changed_between_list_status_checks")
        report["first_status_input_topology_revision"] = first_revision
        if first_keys:
            require(first["input_topology_revision"] == second["input_topology_revision"] == revision,
                    "topology_changed_during_enumeration")
        else:
            # There is no row to carry a revision, so both list responses are
            # empty. This is not an input-selection or reopen pass.
            no_devices = True
        report["status_input_topology_revision"] = revision
        report["stable_binding_available_after_explicit_list"] = True
        state = quiet_state(owned, report["owner"])
        if not no_devices:
            learner = owned.call("POST", "/api/learners",
                                {"name": "SYNTHETIC INPUT BINDING", "namespace": "simulator"})
            session = owned.call("POST", "/api/sessions",
                                {"learner_id": learner["learner_id"], "resume": True})
            state = quiet_state(owned, report["owner"])
            baseline = counters(state)
            require(revision > 1, "first_nonempty_catalog_revision_invalid")
            unknown = str(uuid.uuid4())
            require(unknown not in first_keys, "synthetic_unknown_key_collided")
            cases = (("known_key_stale_revision", sorted(first_keys)[0], revision - 1),
                     ("unknown_key_current_revision", unknown, revision))
            for name, key, expected_revision in cases:
                owned.reject_start({"session_id": session["session_id"],
                    "expected_generation": baseline["teaching_generation"],
                    "device_key": key, "expected_input_topology_revision": expected_revision},
                    first_keys, revision)
                after = quiet_state(owned, report["owner"])
                require(counters(after) == baseline, "invalid_start_changed_teaching_audio_or_model_requests")
                require(after["audio"]["input_topology_revision"] == revision,
                        "topology_changed_during_invalid_binding_check")
                report["invalid_binding_checks"].append({"case": name, "http_status": 400,
                    "teaching_and_audio_generation_unchanged": True, "model_requests_unchanged": True})
            report["invariant_counters"] = baseline
            checks_ok = True
        else:
            report["invalid_binding_checks_not_run"] = "no_input_devices"
        require(sha256(executable) == report["executable_sha256"], "executable_changed_during_diagnostic")
    except Exception as error:
        report.update(error_type=type(error).__name__,
                      error_code=str(error) if isinstance(error, SmokeFailure) else "unexpected_exception_no_body_logged")
    finally:
        if owned is not None:
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
        process_gone = owned is None or owned.process.poll() is not None
        if process_gone:
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
        if not report.get("error_code") and not report.get("cleanup_error_code"):
            report["passed"] = checks_ok
            report["outcome"] = "enumeration_and_invalid_bindings_passed" if checks_ok else "no_input_devices" if no_devices else "incomplete"
        else:
            report["outcome"] = "failed"
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "outcome": report["outcome"],
                          "report": str(output / "report.json"), "error_code": report.get("error_code"),
                          "cleanup_error_code": report.get("cleanup_error_code")}))
    return 0 if report["passed"] else 2 if report["outcome"] == "no_input_devices" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--ui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--enumeration-only", action="store_true", required=True)
    args = parser.parse_args()
    return run(args.root.resolve(), args.executable.resolve(), args.ui.resolve(), args.output.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
