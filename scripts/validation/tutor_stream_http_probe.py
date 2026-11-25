"""Explicit real-model HTTP diagnostic with simulated display receipts, no devices.

This checks server persistence, not browser visibility or human reading. The only
remote input is authored synthetic teaching text in a new simulator database.
"""
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


parser = argparse.ArgumentParser()
parser.add_argument("--real-synthetic", action="store_true", required=True)
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--ui", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--segment-speech-contract", action="store_true",
                    help="Enable capability in fixture; close opted-in speech before any display ACK, never play")
args = parser.parse_args()
root, output = args.root.resolve(), args.output.resolve()
output.mkdir(parents=True, exist_ok=False)
fixture = output / "fixture"
shutil.copytree(args.ui, fixture / "frontend/dist")
for name in ["prompts/tutor-agent.md", "prompts/tutor_core.md",
             "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json",
             "runtime/kimi/verification-rust.json"]:
    target = fixture / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(root / name, target)
(fixture / "runtime/tutor-delivery.json").write_text(
    json.dumps({"protocol_version": 1, "mode": "segments",
                "segment_speech": "after_display" if args.segment_speech_contract else "disabled"}),
    encoding="utf-8")
with socket.socket() as reserved:
    reserved.bind(("127.0.0.1", 0))
    port = reserved.getsockname()[1]
process = subprocess.Popen(
    [str(root / "target/release/ai-tutor.exe"), "--root", str(fixture),
     "--port", str(port), "--no-browser"], creationflags=subprocess.CREATE_NO_WINDOW,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
report = {"test_type": "real_model_release_http_simulated_display", "passed": False,
          "production_data_used": False, "devices_requested": False,
          "receipts": "synthetic_protocol_receipts_not_actual_display",
          "browser_acceptance": "NOT_RUN", "speaker_output": False,
          "speech_gate_contract": args.segment_speech_contract, "cases": []}
token = None
db_path = fixture / "runtime/data/simulator.sqlite3"


def call(method, path, data=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    headers = {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + token}
    connection.request(method, path, json.dumps(data) if data is not None else None, headers)
    response = connection.getresponse()
    status, raw = response.status, response.read()
    connection.close()
    return status, json.loads(raw)


def api(method, path, data=None):
    status, result = call(method, path, data)
    assert status == 200, (path, status, result)
    return result


def row(sql, params=()):
    with sqlite3.connect(db_path) as database:
        database.row_factory = sqlite3.Row
        found = database.execute(sql, params).fetchone()
        return dict(found) if found else None


def no_devices():
    state = api("GET", "/api/state")
    assert state["camera"]["state"] == "closed"
    assert not state["audio"]["recording"]
    assert state["ptz"]["mode"] == "off"
    return state


def select(language):
    learner = api("POST", "/api/learners", {"name": "Synthetic stream " + language,
                  "namespace": "simulator", "language": language, "help_style": "guided"})
    return api("POST", "/api/sessions", {"learner_id": learner["learner_id"], "resume": True})


def run_case(language, receipt_mode):
    session = select(language)
    sid = session["session_id"]
    prompt = ("这是合成教学诊断，没有真实学习者或设备观察。请用两句简短的话解释测距模块的作用，再建议一个尚未进行的小实验。"
              if language == "zh" else
              "This is a synthetic teaching diagnostic without a real learner or device observation. "
              "Use two short sentences to explain a distance sensor and suggest one experiment that has not been performed.")
    started = time.perf_counter()
    submission = {"text": prompt}
    if args.segment_speech_contract:
        submission.update(client_submission_id="synthetic_gate_" + language,
                          auto_speak=language == "zh")
    request = api("POST", f"/api/sessions/{sid}/turns", submission)
    assert request["delivery_mode"] == "segments"
    rid, generation = request["request_id"], request["generation"]
    path = f"/api/sessions/{sid}/streams/{rid}"
    if args.segment_speech_contract:
        assert request["client_submission_id"] == submission["client_submission_id"]
        assert request["speech_gate"] == ("open" if language == "zh" else "disabled")
        # Opening a gate cannot start audio until display acknowledgement. Close
        # the only opted-in case synchronously before this diagnostic sends one.
        if language == "zh":
            closed = api("POST", path + "/speech/close", {"generation": generation, "reason": "hidden"})
            assert closed["speech_gate"] == "closed"
    result = {"language": language, "receipt_mode": receipt_mode, "passed": False,
              "first_segment_http_ms": None, "final_validated_http_ms": None,
              "observed_revisions": [], "synthetic_receipt_count": 0,
              "browser_display_ms": None}
    report["cases"].append(result)
    confirmed = []
    deadline = started + 100
    while time.perf_counter() < deadline:
        snapshot = api("GET", path)
        if args.segment_speech_contract:
            assert snapshot["client_submission_id"] == submission["client_submission_id"]
            assert snapshot["speech_gate"] == ("closed" if language == "zh" else "disabled")
        if snapshot["revision"] not in result["observed_revisions"]:
            result["observed_revisions"].append(snapshot["revision"])
        if snapshot["segments"] and result["first_segment_http_ms"] is None:
            result["first_segment_http_ms"] = (time.perf_counter() - started) * 1000
        if snapshot["final_validated"] and result["final_validated_http_ms"] is None:
            result["final_validated_http_ms"] = (time.perf_counter() - started) * 1000
        turn = row("SELECT content,status FROM turns WHERE turn_id=?", (snapshot["turn_id"],))
        assert turn["content"] == "".join(confirmed), "Unconfirmed text entered the teaching turn"
        if receipt_mode == "after_final" and not confirmed:
            assert turn["content"] == ""
        assert snapshot["phase"] not in ["failed", "cancelled", "interrupted"], snapshot
        can_ack = receipt_mode == "incremental" or snapshot["final_validated"]
        if can_ack and len(confirmed) < len(snapshot["segments"]):
            segment = snapshot["segments"][len(confirmed)]
            assert segment["sequence"] == len(confirmed)
            ack = {"generation": generation, "sequence": len(confirmed)}
            snapshot = api("POST", path + "/display", ack)
            confirmed.append(segment["text"])
            result["synthetic_receipt_count"] += 1
            assert row("SELECT content FROM turns WHERE turn_id=?", (snapshot["turn_id"],))["content"] == "".join(confirmed)
            # Duplicate receipts must not duplicate the visible prefix or final application.
            duplicate = api("POST", path + "/display", ack)
            assert duplicate["shown_through"] == len(confirmed) - 1
        if snapshot["phase"] == "completed":
            assert snapshot["final_validated"] and confirmed
            assert len(snapshot["segments"]) == len(confirmed)
            result.update(passed=True, segment_count=len(confirmed),
                          exact_confirmed_prefix=True, duplicate_receipt_idempotent=True,
                          completed_http_ms=(time.perf_counter() - started) * 1000)
            break
        time.sleep(.1)
    assert result["passed"], "Timed out waiting for a validated complete stream"
    if args.segment_speech_contract:
        assert row("SELECT COUNT(*) AS n FROM tutor_segments WHERE request_id=? AND speech_attempt_id IS NOT NULL",
                   (rid,))["n"] == 0, "Audio attempt was reserved despite closed/disabled gate"
        if language == "zh":
            closed = api("POST", path + "/speech/close", {"generation": generation, "reason": "user"})
            assert closed["speech_gate"] == "closed" and closed["speech_complete"] is False
        result.update(nonce_roundtrip=True, speech_closed_or_disabled=True,
                      no_audio_attempt_reserved=True, text_completed_after_speech_close=True)
    no_devices()


def cancel_case():
    session = select("en")
    sid = session["session_id"]
    request = api("POST", f"/api/sessions/{sid}/turns", {"text":
                  "Synthetic cancellation diagnostic. Explain a distance sensor in two short sentences."})
    time.sleep(.3)
    started = time.perf_counter()
    api("POST", f"/api/sessions/{sid}/cancel", {})
    cancel_response_ms = (time.perf_counter() - started) * 1000
    path = f"/api/sessions/{sid}/streams/{request['request_id']}"
    assert call("POST", path + "/display", {"generation": request["generation"], "sequence": 0})[0] == 400
    stream = row("SELECT * FROM tutor_streams WHERE request_id=?", (request["request_id"],))
    assert stream["phase"] == "cancelled" and stream["shown_through"] == -1
    assert row("SELECT content FROM turns WHERE turn_id=?", (stream["turn_id"],))["content"] == ""
    deadline = time.perf_counter() + 10
    while api("GET", "/api/state")["kimi"]["running"] and time.perf_counter() < deadline:
        time.sleep(.1)
    assert api("GET", "/api/state")["kimi"]["running"] == 0
    report["cancellation"] = {"passed": True, "requested_after_ms": 300,
        "http_response_ms": cancel_response_ms, "local_provider_drained": True,
        "stale_receipt_rejected": True, "unconfirmed_text_persisted": False,
        "remote_cancel_ack_received": False}


try:
    descriptor = fixture / "runtime/server.json"
    deadline = time.perf_counter() + 30
    while not descriptor.exists() and time.perf_counter() < deadline and process.poll() is None:
        time.sleep(.1)
    info = json.loads(descriptor.read_text(encoding="utf-8"))
    assert info["pid"] == process.pid and info["port"] == port
    token = info["token"]
    state = no_devices()
    assert state["tutor_delivery"]["mode"] == "segments"
    assert state["tutor_delivery"]["automatic_speech"] is False
    assert state["kimi"]["available"], "Existing verified provider is not ready in fixture"
    run_case("zh", "incremental")
    run_case("en", "after_final")
    cancel_case()
    report["passed"] = True
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
    report["process_exit_code"] = process.returncode
    report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    (output / "report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
    print(json.dumps(report, ensure_ascii=True))
