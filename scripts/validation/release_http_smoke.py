"""Run a built workstation with an isolated synthetic database and no devices."""
import argparse
import datetime
import http.client
import json
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--ui", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
root, output = args.root.resolve(), args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
fixture = output / "fixture"
shutil.copytree(args.ui, fixture / "frontend/dist")
with socket.socket() as reserved:
    reserved.bind(("127.0.0.1", 0))
    port = reserved.getsockname()[1]
process = subprocess.Popen([str(root / "target/release/ai-tutor.exe"), "--root", str(fixture),
                            "--port", str(port), "--no-browser"],
                           creationflags=subprocess.CREATE_NO_WINDOW,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
report = {"test_type": "release_process_http_fixture", "passed": False,
          "production_data_used": False, "devices_requested": False,
          "visual_browser_acceptance": "NOT_RUN_tool_URL_verification_stop"}
token = None

def call(method, path, data=None, authenticated=True):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    headers = {"Content-Type": "application/json"}
    if authenticated:
        headers["Cookie"] = "ai_tutor_local=" + token
    connection.request(method, path, json.dumps(data) if data is not None else None, headers)
    response = connection.getresponse()
    raw = response.read()
    status, content_type = response.status, response.getheader("Content-Type", "")
    connection.close()
    return status, json.loads(raw) if "json" in content_type else raw

def api(method, path, data=None):
    status, result = call(method, path, data)
    assert status == 200, (path, status)
    return result

try:
    descriptor = fixture / "runtime/server.json"
    deadline = time.monotonic() + 30
    while not descriptor.exists() and time.monotonic() < deadline and process.poll() is None:
        time.sleep(.1)
    info = json.loads(descriptor.read_text(encoding="utf-8"))
    assert info["pid"] == process.pid and info["port"] == port
    token = info["token"]
    while True:
        try:
            assert call("GET", "/api/state", authenticated=False)[0] == 401
            break
        except (ConnectionRefusedError, ConnectionResetError):
            if time.monotonic() >= deadline or process.poll() is not None:
                raise
            time.sleep(.05)
    learner = api("POST", "/api/learners", {"name": "Synthetic smoke", "namespace": "simulator"})
    other = api("POST", "/api/learners", {"name": "Synthetic other", "namespace": "simulator"})
    lid = learner["learner_id"]
    session = api("POST", "/api/sessions", {"learner_id": lid, "resume": True})
    sid = session["session_id"]
    with sqlite3.connect(fixture / "runtime/data/simulator.sqlite3") as db:
        for index in range(130):
            db.execute("INSERT INTO turns VALUES(?,?,?,?,?,?,?,?,?,?)",
                       ("turn_" + uuid.uuid4().hex, lid, sid, "synthetic", session["generation"],
                        "user", f"history {index}", "shown", "[]", "2026-11-04T00:00:00Z"))
    state = api("GET", "/api/state")
    cursor = state["session"]["history_before"]
    path = f"/api/learners/{lid}/sessions/{sid}/history"
    history = api("GET", path + f"?before={cursor}&limit=2")
    assert [row["content"] for row in history["items"]] == ["history 104", "history 105"]
    assert call("GET", path + "?limit=51")[0] == 400
    assert call("GET", f"/api/learners/{other['learner_id']}/sessions/{sid}/history")[0] == 400
    comparison_path = f"/api/learners/{lid}/sessions/{sid}/comparisons"
    comparison = {
        "submission_id": "release-smoke-comparison-1", "question": "Does threshold change stopping?",
        "changed_variable": "distance threshold",
        "conditions": [{"name": "same car and speed", "status": "reported_same"}],
        "trials": [{"label": "A", "setting": "20 cm", "status": "reported",
                    "result": "hit the wall once", "trial_count": 1},
                   {"label": "B", "setting": "30 cm", "status": "not_tested",
                    "result": None, "trial_count": None}],
        "pending_plan": {"text": "Try thirty centimeters next time", "accepted": False},
        "source_type": "learner_report"}
    saved = api("POST", comparison_path, comparison)
    assert saved["item"]["source_type"] == "simulated" and not saved["item"]["hardware_verified"]
    assert api("POST", comparison_path, comparison) == saved
    assert api("GET", comparison_path)["items"] == [saved["item"]]
    assert call("GET", f"/api/learners/{other['learner_id']}/sessions/{sid}/comparisons")[0] == 400
    state = api("GET", "/api/state")
    assert len(state["session"]["state"]["attempts"]) == 1
    assert not state["session"]["state"].get("pending_step")
    assert api("GET", f"/api/learners/{lid}/memories") == []
    # Reject stale capture/submit requests before opening a device or making a
    # model request. Actual capture remains a separate device acceptance task.
    before_generation = state["session"]["generation"]
    before_recording = state["audio"]["recording_generation"]
    before_requests = state["kimi"]["metrics"].get("requests", 0)
    # This fixture has no delivery policy, models, device enumeration, or loop
    # permission. Exercise only rejected API paths; never enable a real lease.
    assert state["voice_loop"]["available"] is False
    assert not state["voice_loop"].get("client_loop_id")
    loop_before = state["voice_loop"]
    loop_state_before = state["session"]["state"]
    loop_turns_before = state["session"]["turns"]
    loop_status_before = state["session"]["status"]
    stale = before_generation + 1
    # Stable selection is lazy: state reads/start validation must not enumerate
    # or open devices. An unknown/incomplete binding is rejected before cancel.
    assert state["audio"]["stable_binding_available"] is False
    topology_before = state["audio"]["input_topology_revision"]
    loop_path = f"/api/sessions/{sid}/voice-loop"
    loop_tag = {"client_loop_id": "synthetic-never-enabled", "loop_epoch": 1}
    enable = {"client_loop_id": loop_tag["client_loop_id"], "expected_generation": before_generation,
              "language": learner.get("language", "zh"), "device_key": "synthetic-not-enumerated",
              "expected_input_topology_revision": max(1, topology_before)}
    loop_start = {"session_id": sid, "expected_generation": before_generation,
                  "voice_loop": loop_tag}
    loop_turn = {"text": "Synthetic voice-loop rejection", "expected_generation": before_generation,
                 "auto_speak": True, "client_submission_id": "synthetic-loop-submit",
                 "voice_loop": loop_tag, "recording_generation": before_recording}
    loop_cases = [
        ("enable_missing_fields", loop_path + "/enable", {}),
        ("enable_disabled", loop_path + "/enable", enable),
        ("enable_invalid_id", loop_path + "/enable", {**enable, "client_loop_id": "not/a/loop"}),
        ("enable_stale_generation", loop_path + "/enable", {**enable, "expected_generation": stale}),
        ("heartbeat_no_lease", loop_path + "/synthetic-never-enabled/heartbeat", {"loop_epoch": 1}),
        ("heartbeat_invalid_epoch", loop_path + "/synthetic-never-enabled/heartbeat", {"loop_epoch": 0}),
        ("stop_no_lease", loop_path + "/synthetic-never-enabled/stop", {"loop_epoch": 1, "reason": "user"}),
        ("stop_invalid_epoch", loop_path + "/synthetic-never-enabled/stop", {"loop_epoch": 0, "reason": "user"}),
        ("start_without_lease", "/api/audio/record/start", loop_start),
        ("endpoint_requires_bound_input", "/api/audio/record/start", {**loop_start, "endpoint_mode": "voice_endpoint_v1"}),
        ("start_stale_generation", "/api/audio/record/start", {**loop_start, "expected_generation": stale}),
        ("turn_without_lease", f"/api/sessions/{sid}/turns", loop_turn),
        ("turn_stale_generation", f"/api/sessions/{sid}/turns", {**loop_turn, "expected_generation": stale}),
        ("turn_missing_recording", f"/api/sessions/{sid}/turns", {k: v for k, v in loop_turn.items() if k != "recording_generation"}),
        ("turn_missing_loop", f"/api/sessions/{sid}/turns", {k: v for k, v in loop_turn.items() if k != "voice_loop"}),
    ]
    report["voice_loop_negative_cases"] = []
    for case_id, request_path, payload in loop_cases:
        status, _ = call("POST", request_path, payload)
        report["voice_loop_negative_cases"].append({"id": case_id, "http_status": status})
        assert status == 400, (case_id, status)
        unchanged = api("GET", "/api/state")
        assert unchanged["voice_loop"] == loop_before, case_id
        assert unchanged["session"]["generation"] == before_generation, case_id
        assert unchanged["session"]["state"] == loop_state_before, case_id
        assert unchanged["session"]["turns"] == loop_turns_before, case_id
        assert unchanged["session"]["status"] == loop_status_before, case_id
        assert unchanged["audio"]["recording_generation"] == before_recording, case_id
        assert unchanged["audio"]["recording"] is False, case_id
        assert unchanged["audio"]["capture_pending_result"] is False, case_id
        assert unchanged["audio"]["input_topology_revision"] == topology_before, case_id
        assert unchanged["audio"]["stable_binding_available"] is False, case_id
        assert unchanged["kimi"]["metrics"].get("requests", 0) == before_requests, case_id
    invalid_selections = [
        {"device_key": "synthetic-not-enumerated", "expected_input_topology_revision": topology_before},
        {"device_key": "synthetic-not-enumerated"},
        {"expected_input_topology_revision": topology_before},
        {"device_key": "synthetic-not-enumerated", "expected_input_topology_revision": 0},
        {"device_key": "synthetic-not-enumerated", "expected_input_topology_revision": topology_before,
         "device_id": 0},
    ]
    for selection in invalid_selections:
        assert call("POST", "/api/audio/record/start",
                    {"session_id": sid, "expected_generation": before_generation, **selection})[0] == 400
    assert call("POST", "/api/audio/record/start",
                {"session_id": sid, "expected_generation": stale})[0] == 400
    assert call("POST", f"/api/sessions/{sid}/turns",
                {"text": "Synthetic stale submission", "expected_generation": stale})[0] == 400
    for action in ("stop", "discard"):
        assert call("POST", "/api/audio/record/" + action,
                    {"session_id": sid, "recording_generation": before_recording,
                     "session_generation": before_generation})[0] == 400
    state = api("GET", "/api/state")
    assert state["session"]["generation"] == before_generation
    assert state["audio"]["recording_generation"] == before_recording
    assert state["audio"]["input_topology_revision"] == topology_before
    assert state["audio"]["stable_binding_available"] is False
    assert state["kimi"]["metrics"].get("requests", 0) == before_requests
    assert call("GET", "/")[0] == 200
    assert call("GET", "/assets/tutor_ui_bg.wasm")[1].startswith(b"\0asm")
    assert state["camera"]["state"] == "closed"
    assert state["audio"]["recording"] is False
    assert state["local_asr"]["phase"] == "disabled"
    assert state["local_asr"]["active"] is False and state["local_asr"]["busy"] is False
    assert state["audio"]["asr_ready"] is False
    assert state["ptz"]["mode"] == "off"
    report.update(passed=True, namespace="simulator", synthetic_turns=130,
                  history_query_and_owner=True, ui_assets_available=True,
                  comparison_saved_and_replayed=True, plan_not_promoted_to_completion=True,
                  stale_recording_and_submission_rejected_without_mutation=True,
                  voice_loop_disabled_by_default=True,
                  voice_loop_rejections_preserve_owner_and_audio=True,
                  unconfigured_local_asr_disabled=True,
                  invalid_stable_bindings_rejected_without_mutation=len(invalid_selections),
                  device_enumeration_initialized=False,
                  camera_open=False, audio_recording=False, ptz_mode="off")
except Exception as error:
    report["error"] = str(error)
    raise
finally:
    if token and process.poll() is None:
        try:
            api("POST", "/api/shutdown", {})
        except (OSError, AssertionError, http.client.HTTPException):
            pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.terminate()
        process.wait(timeout=5)
        report["forced_owned_process_stop"] = True
        report["passed"] = False
    report["process_exit_code"] = process.returncode
    if process.returncode != 0:
        report["passed"] = False
    report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    (output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report))

if not report["passed"]:
    raise SystemExit(1)
