"""Exercise persistent help preferences through an isolated release process.

The caller supplies a built executable and UI directory. This script creates a
new fixture; it never opens a source database, writes SQL, starts a browser,
requests a model/device, or reads provider credentials. An optional older binary
can reopen that fixture for public HTTP readback only. Every process is stopped
with its native --root <fixture> --stop command and awaited; no process is killed.
Failed fixtures and stdout/stderr logs are retained for inspection.
"""
import argparse
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time


MAX_RESPONSE_BYTES = 4 * 1024 * 1024
PROFILES = (
    {
        "language": "zh", "name": "SYNTHETIC 帮助偏好",
        "original": "以后请先给我一个小提示，再让我自己试一试；需要时再逐步解释。\n请用简短的中文。",
        "corrected": "以后请先解释一个具体例子，再给我一个可以自己完成的小步骤。\n请用简短的中文。",
        "retained": "这是忘记后重新明确保存的偏好：以后一次只解释一个小步骤，并等我回答。",
    },
    {
        "language": "en", "name": "SYNTHETIC help preference",
        "original": "In future activities, give me one short hint at a time.\nLet me try before explaining the next step.",
        "corrected": "In future activities, explain one concrete example first.\nThen give me a small step to try myself.",
        "retained": "Newly saved after forgetting: in future activities, explain one small step and wait for my reply.",
    },
)


class SmokeFailure(Exception):
    """Report codes only: never include HTTP bodies or local auth descriptors."""


def require(condition, code):
    if not condition:
        raise SmokeFailure(code)


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def isolated_environment(fixture):
    # Only Windows boot variables are inherited. Provider homes/configuration,
    # PATH programs, cloud tokens and the user's application profiles are absent.
    env = {key: os.environ[key] for key in ("SystemRoot", "WINDIR", "SystemDrive") if key in os.environ}
    directories = {
        "USERPROFILE": "isolated-home", "HOME": "isolated-home",
        "KIMI_CODE_HOME": "isolated-home/.kimi-code", "APPDATA": "isolated-home/AppData/Roaming",
        "LOCALAPPDATA": "isolated-home/AppData/Local", "TEMP": "isolated-temp",
        "TMP": "isolated-temp", "PATH": "empty-bin",
    }
    for name, relative in directories.items():
        folder = fixture / relative
        folder.mkdir(parents=True, exist_ok=True)
        env[name] = str(folder)
    return env


class Workstation:
    def __init__(self, executable, fixture, env, logs, stage):
        self.executable, self.fixture, self.env = executable, fixture, env
        self.logs, self.stage = logs, stage
        self.descriptor = fixture / "runtime/server.json"
        self.token = None
        self.process = None
        self.streams = []
        self.stop_attempted = False

    def start(self):
        require(not self.descriptor.exists(), "fixture_descriptor_remains_before_start")
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.streams = [(self.logs / f"{self.stage['name']}.{name}.log").open("xb")
                        for name in ("stdout", "stderr")]
        self.process = subprocess.Popen(
            [str(self.executable), "--root", str(self.fixture), "--port", str(self.port), "--no-browser"],
            cwd=self.fixture, env=self.env, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            stdin=subprocess.DEVNULL, stdout=self.streams[0], stderr=self.streams[1],
        )
        self.stage.update(pid=self.process.pid, port=self.port, hidden=True)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "main_exited_before_ready")
            if self.token is None and self.descriptor.exists():
                try:
                    info = json.loads(self.descriptor.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    time.sleep(0.05)
                    continue
                require(info.get("pid") == self.process.pid and info.get("port") == self.port,
                        "descriptor_not_owned_process")
                token = info.get("token")
                require(isinstance(token, str) and 32 <= len(token) <= 256,
                        "owned_local_auth_descriptor_invalid")
                self.token = token
                self.stage["descriptor_bound"] = True
            if self.token is not None:
                try:
                    state = self.api("GET", "/api/state")
                    check_idle(state)
                    return state
                except (ConnectionRefusedError, ConnectionResetError, TimeoutError, http.client.HTTPException):
                    time.sleep(0.05)
                    continue
            time.sleep(0.05)
        raise SmokeFailure("main_startup_deadline_exceeded")

    def request(self, method, path, body=None, authenticated=True):
        require(self.process is not None and self.process.poll() is None, "owned_main_not_running")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        headers = {"Content-Type": "application/json"}
        if authenticated:
            require(self.token is not None, "local_auth_not_ready")
            headers["Cookie"] = "ai_tutor_local=" + self.token
        try:
            payload = json.dumps(body, ensure_ascii=False).encode("utf-8") if body is not None else None
            connection.request(method, path, payload, headers)
            response = connection.getresponse()
            self.stage["last_http"] = {"method": method, "path": path, "status": response.status}
            raw = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(raw) <= MAX_RESPONSE_BYTES, "http_response_over_limit")
            require("json" in response.getheader("Content-Type", ""), "http_response_not_json")
            return response.status, json.loads(raw)
        finally:
            connection.close()

    def api(self, method, path, body=None):
        status, value = self.request(method, path, body)
        require(status == 200, f"unexpected_http_{method}_{status}")
        return value

    def stop(self):
        """Native graceful stop only; even a timed-out child remains inspectable."""
        if self.stop_attempted:
            return
        self.stop_attempted = True
        if self.process is None:
            for stream in self.streams:
                stream.close()
            return
        stop_process = None
        try:
            if self.process.poll() is None:
                owned = False
                if self.descriptor.exists():
                    try:
                        info = json.loads(self.descriptor.read_text(encoding="utf-8"))
                        owned = info.get("pid") == self.process.pid and info.get("port") == self.port
                    except (OSError, ValueError):
                        pass
                if owned:
                    with (self.logs / f"{self.stage['name']}.native-stop.stdout.log").open("xb") as out, \
                            (self.logs / f"{self.stage['name']}.native-stop.stderr.log").open("xb") as err:
                        stop_process = subprocess.Popen(
                            [str(self.executable), "--root", str(self.fixture), "--stop"],
                            cwd=self.fixture, env=self.env,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
                            stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                        )
                        self.stage["native_stop_pid"] = stop_process.pid
                        try:
                            self.stage["native_stop_exit_code"] = stop_process.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            self.stage["native_stop_wait_timed_out"] = True
                else:
                    self.stage["native_stop_skipped_unbound_descriptor"] = True
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.stage["main_wait_timed_out"] = True
        finally:
            self.stage["exit_code"] = self.process.poll()
            self.stage["main_still_running"] = self.process.poll() is None
            self.stage["native_stop_still_running"] = stop_process is not None and stop_process.poll() is None
            self.stage["forced_stop"] = False
            self.stage["clean_exit"] = (self.stage.get("native_stop_exit_code") == 0
                                        and self.process.poll() == 0
                                        and not self.stage["native_stop_still_running"])
            self.token = None
            for stream in self.streams:
                stream.close()


def check_idle(state):
    require(state["camera"]["state"] == "closed", "camera_not_closed")
    require(state["audio"]["recording"] is False and state["audio"].get("state") == "idle", "audio_not_idle")
    require(state["ptz"]["mode"] == "off", "ptz_not_off")
    require(state["kimi"].get("available") is False, "provider_not_disabled")
    require(state["kimi"].get("running", 0) == 0 and state["kimi"]["metrics"].get("requests", 0) == 0,
            "model_request_observed")
    require(not state.get("actuator_execution"), "actuator_execution_enabled")
    for name in ("local_speech", "local_asr", "final_asr"):
        service = state.get(name)
        if isinstance(service, dict):
            require(not service.get("active") and not service.get("busy") and not service.get("ready"),
                    name + "_became_active")
    if isinstance(state.get("local_speech"), dict):
        require(state["local_speech"].get("phase") == "disabled", "local_speech_not_disabled")


def memories(app, lid):
    result = app.api("GET", f"/api/learners/{lid}/memories")
    require(isinstance(result, list), "memory_list_invalid")
    return result


def intent(app, lid, sid, content, request_id):
    state = app.api("GET", "/api/state")
    require(state["session"]["session_id"] == sid and state["session"]["learner_id"] == lid,
            "intent_not_current_owner_session")
    rows = [row for row in state["learners"] if row["learner_id"] == lid]
    require(len(rows) == 1 and type(rows[0].get("epoch")) is int, "public_learner_epoch_missing")
    return {"content": content, "request_id": request_id, "expected_epoch": rows[0]["epoch"],
            "expected_generation": state["session"]["generation"]}


def preference_path(lid, sid):
    return f"/api/learners/{lid}/sessions/{sid}/help-preference"


def source_evidence(app, lid, mid, text, event_id=None, corrected=False):
    rows = memories(app, lid)
    require(len(rows) == 1 and rows[0]["memory_id"] == mid, "active_preference_missing_or_duplicated")
    memory = rows[0]
    require(memory["learner_id"] == lid and memory["kind"] == "help_preference"
            and memory["scope"] == "cross_session_explicit" and memory["source_type"] == "simulated"
            and memory["content"] == text, "preference_content_owner_scope_or_grade")
    evidence = app.api("GET", f"/api/learners/{lid}/memories/{mid}/evidence")
    require(evidence["namespace"] == "simulator" and evidence["memory"] == memory, "evidence_memory_mismatch")
    sources, links = evidence["sources"], evidence["source_links"]
    require(len(sources) == 1 and len(links) == 1, "preference_source_duplicated_or_missing")
    source = sources[0]
    require(source["learner_id"] == lid and source["source_type"] == "simulated", "source_owner_or_grade")
    require(source["event_kind"] == ("teacher_setting" if corrected else "learner_statement"), "source_kind")
    require(source["content"].get("correction" if corrected else "text") == text, "source_original_text_changed")
    require(source["content"].get("explicit_preference") is True
            and source["content"].get("preference_scope") == "persistent", "source_not_explicit_persistent")
    if not corrected:
        require(source["content"].get("confirmation") == "explicit_save_action", "source_missing_save_action")
    if event_id is not None:
        require(source["event_id"] == event_id, "response_event_mismatch")
    require(memory["source_event_ids"] == [source["event_id"]]
            and links[0]["event_id"] == source["event_id"]
            and links[0]["event_version"] == source["event_version"], "live_source_version_link_mismatch")
    return evidence


def save(app, lid, sid, body):
    result = app.api("POST", preference_path(lid, sid), body)
    require(result.get("learner_id") == lid and result.get("session_id") == sid
            and result.get("replayed") is False, "save_response_owner_or_replay")
    require(isinstance(result.get("memory_id"), str) and isinstance(result.get("event_id"), str), "save_missing_ids")
    require(result.get("learner_epoch") == body["expected_epoch"] + 1, "save_did_not_advance_epoch_once")
    current = intent(app, lid, sid, body["content"], body["request_id"])
    require(current["expected_epoch"] == result["learner_epoch"]
            and current["expected_generation"] == body["expected_generation"], "save_generation_or_epoch_changed")
    source_evidence(app, lid, result["memory_id"], body["content"], result["event_id"])
    return result


def stable_public(app, lids):
    state = app.api("GET", "/api/state")
    check_idle(state)
    return {"learners": state["learners"], "session": state["session"],
            "memories": {lid: memories(app, lid) for lid in lids}}


def rejected(app, method, path, body, lids, report, label):
    before = stable_public(app, lids)
    status, _ = app.request(method, path, body)
    require(status == 400, label + "_not_rejected")
    require(stable_public(app, lids) == before, label + "_mutated_state")
    report["checks"].append(label)


def new_session(app, lid, resume=False):
    return app.api("POST", "/api/sessions", {"learner_id": lid, "resume": resume})


def finish(app, sid):
    app.api("POST", f"/api/sessions/{sid}/finish", {})


def public_readback(app, lids):
    result = {}
    for lid in lids:
        rows = memories(app, lid)
        require(len(rows) == 1, "rollback_requires_nonempty_current_preference")
        result[lid] = {"memories": rows, "evidence": {
            row["memory_id"]: app.api("GET", f"/api/learners/{lid}/memories/{row['memory_id']}/evidence")
            for row in rows}}
    return result


def exercise(app, report):
    require(app.request("GET", "/api/state", authenticated=False)[0] == 401, "unauthenticated_state_not_rejected")
    profiles = []
    for data in PROFILES:
        learner = app.api("POST", "/api/learners", {"name": data["name"], "language": data["language"],
                                                    "namespace": "simulator"})
        require(learner["namespace"] == "simulator", "learner_not_simulator")
        profiles.append({**data, "lid": learner["learner_id"]})
    lids = [profile["lid"] for profile in profiles]
    for profile in profiles:
        lid, lang = profile["lid"], profile["language"]
        session = new_session(app, lid)
        sid = session["session_id"]
        require(memories(app, lid) == [], "fresh_learner_has_memories")
        body = intent(app, lid, sid, profile["original"], f"{lang}-original")
        result = save(app, lid, sid, body)
        before = public_readback_one(app, lid)
        replay = app.api("POST", preference_path(lid, sid), body)
        require(replay == {**result, "replayed": True}, "replay_changed_receipt")
        require(public_readback_one(app, lid) == before, "replay_duplicated_memory_or_evidence")
        profile.update(sid=sid, body=body, receipt=result)
        report["checks"].extend([lang + "_verbatim_simulated_source", lang + "_idempotent_replay"])
        other_lid = next(value for value in lids if value != lid)
        rejected(app, "POST", preference_path(other_lid, sid), body, lids, report, lang + "_wrong_owner_rejected")
        rejected(app, "GET", f"/api/learners/{other_lid}/memories/{result['memory_id']}/evidence",
                 None, lids, report, lang + "_foreign_evidence_rejected")
        rejected(app, "POST", preference_path(lid, sid), {**body, "content": profile["corrected"]},
                 lids, report, lang + "_changed_payload_same_key_rejected")

    for profile in profiles:
        lid, lang, sid = profile["lid"], profile["language"], profile["sid"]
        other_lid = next(value for value in lids if value != lid)
        other_before = public_readback_one(app, other_lid)
        current = new_session(app, lid, resume=True)
        require(current["session_id"] == sid, "original_session_not_resumed")
        replay = app.api("POST", preference_path(lid, sid), profile["body"])
        require(replay == {**profile["receipt"], "replayed": True}, "same_session_generation_replay_failed")
        mid = profile["receipt"]["memory_id"]
        corrected = app.api("PATCH", f"/api/learners/{lid}/memories/{mid}", {"content": profile["corrected"]})
        corrected_mid = corrected["memory_id"]
        require(corrected_mid != mid, "correction_reused_old_memory")
        source_evidence(app, lid, corrected_mid, profile["corrected"], corrected=True)
        for label, payload in (("original", profile["body"]),
                               ("fresh_versions", intent(app, lid, sid, profile["original"], profile["body"]["request_id"]))):
            rejected(app, "POST", preference_path(lid, sid), payload, lids, report,
                     f"{lang}_corrected_request_{label}_cannot_revive")
        rejected(app, "GET", f"/api/learners/{lid}/memories/{mid}/evidence", None, lids, report,
                 lang + "_corrected_old_evidence_invalid")
        finish(app, sid)
        next_activity = new_session(app, lid)
        next_sid = next_activity["session_id"]
        require(next_sid != sid and next_activity["activity_id"] != current["activity_id"], "activity_not_new")
        recalled = app.api("GET", f"/api/sessions/{next_sid}/context")["recalled_memories"]
        require(len(recalled) == 1 and recalled[0]["memory_id"] == corrected_mid
                and recalled[0]["learner_id"] == lid and recalled[0]["content"] == profile["corrected"],
                "next_activity_missing_own_corrected_preference_or_recalled_foreign")
        report["checks"].append(lang + "_next_activity_recalls_only_own_corrected_preference")
        # This new receipt belongs to the active session, so deletion replay is
        # rejected for tombstoning rather than merely for an inactive old session.
        forget_body = intent(app, lid, next_sid, profile["corrected"], f"{lang}-delete-probe")
        forget_receipt = save(app, lid, next_sid, forget_body)
        app.api("DELETE", f"/api/learners/{lid}/memories/{forget_receipt['memory_id']}")
        require(memories(app, lid) == [], "forget_left_active_preference")
        for label, payload in (("original", forget_body),
                               ("fresh_versions", intent(app, lid, next_sid, profile["corrected"], forget_body["request_id"]))):
            rejected(app, "POST", preference_path(lid, next_sid), payload, lids, report,
                     f"{lang}_forgotten_request_{label}_cannot_revive")
        rejected(app, "GET", f"/api/learners/{lid}/memories/{forget_receipt['memory_id']}/evidence",
                 None, lids, report, lang + "_forgotten_evidence_invalid")
        finish(app, next_sid)
        final = new_session(app, lid)
        require(app.api("GET", f"/api/sessions/{final['session_id']}/context")["recalled_memories"] == [],
                "next_activity_revived_forgotten_preference")
        report["checks"].append(lang + "_forgotten_preference_absent_next_activity")
        retained_body = intent(app, lid, final["session_id"], profile["retained"], f"{lang}-new-after-forget")
        retained = save(app, lid, final["session_id"], retained_body)
        require(public_readback_one(app, other_lid) == other_before, "edit_or_forget_mutated_other_owner")
        profile["retained_memory_id"] = retained["memory_id"]
        finish(app, final["session_id"])
    check_idle(app.api("GET", "/api/state"))
    report["profiles"] = [{"language": p["language"], "learner_id": p["lid"],
                           "retained_memory_id": p["retained_memory_id"]} for p in profiles]
    report["public_visible_idempotency"] = True
    report["source_versions_verified"] = True
    return lids, public_readback(app, lids)


def public_readback_one(app, lid):
    return {"memories": memories(app, lid), "evidence": {
        row["memory_id"]: app.api("GET", f"/api/learners/{lid}/memories/{row['memory_id']}/evidence")
        for row in memories(app, lid)}}


def run(args):
    executable, ui, output = args.executable.resolve(), args.ui.resolve(), args.output.resolve()
    rollback = args.rollback_executable.resolve() if args.rollback_executable else None
    require(executable.is_file() and (ui / "index.html").is_file(), "executable_or_built_ui_missing")
    require(rollback is None or rollback.is_file(), "rollback_executable_missing")
    require(not output.exists() and not output.is_relative_to(ui), "output_must_be_new_and_outside_ui")
    output.mkdir(parents=True, exist_ok=False)
    fixture, logs = output / "fixture", output / "logs"
    logs.mkdir()
    (fixture / "runtime").mkdir(parents=True)
    shutil.copytree(ui, fixture / "frontend/dist")
    write_json(fixture / "runtime/local-speech-policy.json", {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"})
    env = isolated_environment(fixture)
    report = {"test_type": "release_http_persistent_help_preference", "passed": False,
              "executable_sha256": sha256(executable), "script_sha256": sha256(Path(__file__)),
              "fixture_root": str(fixture), "synthetic_fixture_only": True, "namespace": "simulator",
              "sql_reads_or_writes_by_probe": False, "model_requests_requested": False,
              "devices_requested": False, "provider_credentials_read": False,
              "browser_acceptance": "NOT_RUN", "language_teaching_effectiveness": "NOT_TESTED_no_real_model",
              "fixture_and_logs_preserved": True, "checks": [], "stages": [],
              "rollback": {"requested": rollback is not None, "passed": False if rollback else None}}
    app = None
    try:
        stage = {"name": "new-release", "checks_passed": False}
        report["stages"].append(stage)
        app = Workstation(executable, fixture, env, logs, stage)
        app.start()
        lids, expected = exercise(app, report)
        write_json(output / "expected-public-memories.json", expected)
        stage["checks_passed"] = True
        app.stop()
        require(stage.get("clean_exit"), "new_release_did_not_exit_cleanly")
        app = None
        if rollback:
            report["rollback"]["executable_sha256"] = sha256(rollback)
            old_stage = {"name": "rollback-readonly", "checks_passed": False}
            report["stages"].append(old_stage)
            app = Workstation(rollback, fixture, env, logs, old_stage)
            app.start()
            observed = public_readback(app, lids)
            require(observed == expected, "old_release_public_memory_or_source_readback_changed")
            check_idle(app.api("GET", "/api/state"))
            old_stage["checks_passed"] = True
            app.stop()
            require(old_stage.get("clean_exit"), "old_release_did_not_exit_cleanly")
            app = None
            report["rollback"].update(passed=True, read_only_http=True, nonempty_profiles_compared=len(lids))
        report["passed"] = True
    except Exception as error:
        report["error_code"] = str(error) if isinstance(error, SmokeFailure) else type(error).__name__
    finally:
        if app is not None:
            try:
                app.stop()
            except Exception as error:
                report["shutdown_error_type"] = type(error).__name__
            if not app.stage.get("clean_exit"):
                report["passed"] = False
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        report["checks_passed_count"] = len(report["checks"])
        write_json(output / "report.json", report)
        print(json.dumps(report, ensure_ascii=True))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", required=True, type=Path)
    parser.add_argument("--ui", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--rollback-executable", type=Path)
    args = parser.parse_args()
    try:
        return run(args)
    except SmokeFailure as error:
        print(json.dumps({"passed": False, "error_code": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
