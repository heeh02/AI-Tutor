"""New -> installed old -> new HTTP smoke on a fresh, synthetic fixture only.

Authoring this script does not run a workstation. Root reviews and invokes it.
No SQL writes, model binaries/configuration, browser, device or real credentials
are used. Existing binaries and source databases are never modified.
"""
import argparse
from contextlib import contextmanager
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import time


NEW_BINARY = "runtime/goal-releases/20261104-seventh/bin/ai-tutor.exe"
NEW_SHA256 = "99f7efefadecb1c51cd6d406a3deea833236d47791b7cdcac71d64bf74112764"
OLD_BINARY = "bin/ai-tutor.exe"
OLD_SHA256 = "16c654e1dae5fbce5431ce1df3a5b3616da655e7106ef7bb011bf5ceefbcaf1d"
NEW_NAME = "SYNTHETIC COMPAT NEW"
OLD_NAME = "SYNTHETIC COMPAT OLD"
MAX_RESPONSE_BYTES = 4 * 1024 * 1024
COMPARISON = {
    "submission_id": "synthetic-database-compat-v1",
    "question": "How could stopping thresholds be compared? Neither trial has run.",
    "changed_variable": "stopping threshold",
    "conditions": [{"name": "car, speed and wall position", "status": "not_confirmed"}],
    "trials": [
        {"label": "A", "setting": "20 cm", "status": "not_tested", "result": None, "trial_count": None},
        {"label": "B", "setting": "30 cm", "status": "not_tested", "result": None, "trial_count": None},
    ],
    "pending_plan": {"text": "Not executed: compare both thresholds after confirming conditions.", "accepted": False},
    "source_type": "learner_report",
}


class SmokeFailure(Exception):
    """Codes only; HTTP bodies, token-bearing descriptors and env are not logged."""


def require(condition, code):
    if not condition:
        raise SmokeFailure(code)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def json_hash(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(",", ":")).encode("utf-8")).hexdigest()


def write_json(path, value):
    # Output is a newly created owned directory, never a production location.
    with path.open("x", encoding="utf-8") as target:
        json.dump(value, target, ensure_ascii=False, indent=2)


def isolated_environment(fixture):
    """Allowlist OS boot variables, never copy the host's provider environment."""
    env = {key: os.environ[key] for key in ("SystemRoot", "WINDIR", "SystemDrive") if key in os.environ}
    paths = {
        "USERPROFILE": fixture / "isolated-home",
        "HOME": fixture / "isolated-home",
        "KIMI_CODE_HOME": fixture / "isolated-home/.kimi-code",
        "APPDATA": fixture / "isolated-home/AppData/Roaming",
        "LOCALAPPDATA": fixture / "isolated-home/AppData/Local",
        "TEMP": fixture / "isolated-temp",
        "TMP": fixture / "isolated-temp",
        "PATH": fixture / "empty-bin",
    }
    for key, value in paths.items():
        value.mkdir(parents=True, exist_ok=True)
        env[key] = str(value)
    return env


class OwnedWorkstation:
    def __init__(self, executable, fixture, env, stage):
        self.fixture, self.stage = fixture, stage
        self.descriptor = fixture / "runtime/server.json"
        self.token = None
        self.process = None
        require(not self.descriptor.exists(), "stale_fixture_descriptor_before_start")
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.process = subprocess.Popen(
            [str(executable), "--root", str(fixture), "--port", str(self.port), "--no-browser"],
            cwd=fixture, env=env, creationflags=subprocess.CREATE_NO_WINDOW,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        stage.update(pid=self.process.pid, port=self.port, hidden=True, descriptor_bound=False)

    def call(self, method, path, body=None):
        require(self.process.poll() is None and self.token is not None, "owned_process_not_available")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        try:
            connection.request(method, path, json.dumps(body) if body is not None else None,
                               {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + self.token})
            response = connection.getresponse()
            raw = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(raw) <= MAX_RESPONSE_BYTES, "http_response_over_limit")
            require(response.status == 200, f"http_{method}_{path}_{response.status}")
            return json.loads(raw)
        finally:
            connection.close()

    def ready(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "process_exited_before_ready")
            if self.token is None and self.descriptor.exists():
                try:
                    info = json.loads(self.descriptor.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    time.sleep(.05)
                    continue
                require(info.get("pid") == self.process.pid and info.get("port") == self.port,
                        "descriptor_not_owned_pid_and_port")
                token = info.get("token")
                require(isinstance(token, str) and 32 <= len(token) <= 256, "invalid_owned_descriptor_token")
                self.token = token
                self.stage["descriptor_bound"] = True
            if self.token is not None:
                try:
                    state = self.call("GET", "/api/state")
                except (ConnectionRefusedError, ConnectionResetError, TimeoutError):
                    time.sleep(.05)
                    continue
                check_idle(state, self.stage)
                return state
            time.sleep(.05)
        raise SmokeFailure("startup_deadline_exceeded")

    def stop(self):
        # Only the Popen child handle is ever terminated. No PID search, kill by
        # port, global --stop, taskkill, WSL or process-tree discovery is used.
        if self.process is None:
            return
        normal = False
        forced = False
        if self.process.poll() is None and self.token is not None:
            try:
                self.call("POST", "/api/shutdown", {})
                normal = True
            except (OSError, ValueError, http.client.HTTPException, SmokeFailure):
                pass
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            forced = True
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.stage.update(shutdown_http_accepted=normal, owned_process_reaped=True,
                          forced_owned_process_stop=forced, exit_code=self.process.returncode)
        # Do not retain even this synthetic credential after its owner exits.
        # A descriptor with any other identity is never removed or contacted.
        if self.descriptor.exists() and self.token is not None:
            try:
                info = json.loads(self.descriptor.read_text(encoding="utf-8"))
                if (info.get("pid"), info.get("port"), info.get("token")) == (self.process.pid, self.port, self.token):
                    self.descriptor.unlink()
                    self.stage["owned_credential_removed"] = True
            except (OSError, ValueError):
                self.stage["owned_credential_removal_failed"] = True
        self.token = None


@contextmanager
def workstation(executable, fixture, env, report, label):
    stage = {"stage": label, "checks_passed": False}
    report["stages"].append(stage)
    owned = OwnedWorkstation(executable, fixture, env, stage)
    try:
        state = owned.ready()
        yield owned, state, stage
        stage["checks_passed"] = True
    finally:
        owned.stop()
    require(stage.get("shutdown_http_accepted") and stage.get("exit_code") == 0
            and not stage.get("forced_owned_process_stop"), "stage_did_not_shutdown_cleanly")


def check_idle(state, stage):
    require(state["camera"]["state"] == "closed", "camera_not_closed")
    require(state["audio"]["recording"] is False, "recording_unexpected")
    require(state["audio"].get("state") == "idle", "audio_not_idle")
    require(state["ptz"]["mode"] == "off", "ptz_not_off")
    require(state["kimi"].get("available") is False, "provider_not_disabled")
    require(state["kimi"].get("running", 0) == 0 and state["kimi"]["metrics"].get("requests", 0) == 0,
            "model_request_observed")
    local = state.get("local_speech")
    if local is not None:
        require(not local.get("active") and not local.get("ready") and not local.get("warmup_completed"),
                "local_voice_became_active")
        require(local.get("phase") == "disabled", "local_voice_policy_not_disabled")
    stage["idle_checks"] = {"camera": "closed", "audio": "idle", "ptz": "off",
                            "provider_available": False, "model_requests": 0,
                            "local_voice": "disabled" if local is not None else "not_in_old_api"}


def learner_row(state, lid):
    rows = [row for row in state["learners"] if row["learner_id"] == lid]
    require(len(rows) == 1, "learner_missing_or_duplicated")
    return rows[0]


def assert_uncompleted(session):
    state = session["state"]
    require(session["status"] != "finished", "untested_session_became_finished")
    require(state.get("stage") != "completed", "untested_plan_became_completed_stage")
    require(not state.get("reported_result") and not state.get("attempts"), "untested_plan_became_observation")
    require(not state.get("pending_step"), "unaccepted_plan_became_accepted_step")
    require(not state.get("completed_steps") and not state.get("completed_experiments"), "plan_became_completion")


def database_snapshot(fixture, lid, sid):
    """Read only after the corresponding owned program has exited."""
    path = fixture / "runtime/data/simulator.sqlite3"
    require(path.is_file(), "fixture_database_missing")
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as db:
        db.row_factory = sqlite3.Row
        require(db.execute("PRAGMA integrity_check").fetchall()[0][0] == "ok", "sqlite_integrity_failed")
        def rows(sql, args=()):
            return [dict(row) for row in db.execute(sql, args)]
        learner = rows("SELECT * FROM learners WHERE learner_id=?", (lid,))
        session = rows("SELECT * FROM sessions WHERE learner_id=? AND session_id=?", (lid, sid))
        require(len(learner) == len(session) == 1, "snapshot_identity_missing")
        state = json.loads(session[0]["state"])
        assert_uncompleted({**session[0], "state": state})
        memories = rows("SELECT * FROM memories WHERE learner_id=? ORDER BY memory_id", (lid,))
        require(not memories, "untested_plan_entered_slow_memory")
        require(db.execute("SELECT count(*) FROM provider_runs").fetchone()[0] == 0, "persisted_model_run_observed")
        comparisons = rows("SELECT * FROM experiment_comparisons WHERE learner_id=? ORDER BY comparison_id", (lid,))
        events = rows("SELECT * FROM events WHERE learner_id=? ORDER BY event_id", (lid,))
        metadata = rows("SELECT * FROM education_event_meta WHERE learner_id=? ORDER BY event_id", (lid,))
        require(len(comparisons) == 1 and len(events) == len(metadata) == 2, "synthetic_source_count_changed")
        require(all(row["kind"] == "plan" and row["status"] == "active" for row in metadata), "plan_source_kind_or_status_changed")
        require(all(json.loads(row["content"]).get("accepted") is False for row in events), "plan_acceptance_changed")
        schema = rows("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        semantic = {"learner": learner[0], "session_id": sid,
                    "activity_id": session[0]["activity_id"], "state": state,
                    "comparisons": comparisons, "events": events, "event_metadata": metadata,
                    "memories": memories}
        return {"semantic": semantic, "semantic_sha256": json_hash(semantic),
                "session_status": session[0]["status"], "session_generation": session[0]["generation"],
                "schema_sha256": json_hash(schema), "schema": schema,
                "database_file_sha256": sha256(path), "sqlite_integrity": "ok"}


def run(root, output):
    require(os.name == "nt", "windows_binary_smoke_requires_windows")
    require(output.is_relative_to(root / "runtime/goal-baselines/20261104"), "output_must_be_under_owned_baseline_directory")
    require(not output.exists(), "output_already_exists")
    new, old = root / NEW_BINARY, root / OLD_BINARY
    require(sha256(new) == NEW_SHA256, "frozen_new_executable_hash_mismatch")
    require(sha256(old) == OLD_SHA256, "installed_old_executable_hash_mismatch")
    output.mkdir(parents=True, exist_ok=False)
    fixture = output / "fixture"
    (fixture / "runtime").mkdir(parents=True)
    write_json(fixture / "runtime/local-speech-policy.json",
               {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"})
    # There is no browser/UI acceptance in this test and no real UI assets are
    # needed for API startup. No prompts, model files or service descriptors are staged.
    (fixture / "frontend/dist").mkdir(parents=True)
    (fixture / "frontend/dist/index.html").write_text("<!doctype html><title>Synthetic compatibility fixture</title>", encoding="utf-8")
    env = isolated_environment(fixture)
    report = {"test_type": "new_old_new_database_compatibility", "passed": False,
              "binaries": {"new": {"path": NEW_BINARY, "sha256": NEW_SHA256, "frozen_test_count": 566},
                           "old": {"path": OLD_BINARY, "sha256": OLD_SHA256, "reported_installed_test_count": 261}},
              "synthetic_fixture_only": True, "production_database_opened": False,
              "production_credentials_read": False, "sql_writes_by_probe": False,
              "devices_requested": False, "model_requests_requested": False, "browser_opened": False,
              "environment": "allowlisted OS paths plus fresh empty provider home and PATH; no host credentials inherited",
              "local_speech_policy": "disabled", "stages": [],
              "scope": "startup/migrations, stable learner HTTP read/write and preservation of new comparison/plan data; not all downgrade operations"}
    try:
        with workstation(new, fixture, env, report, "new_create") as (app, initial, stage):
            require(initial["learners"] == [], "fresh_fixture_not_empty")
            learner = app.call("POST", "/api/learners", {"name": NEW_NAME, "namespace": "simulator"})
            lid = learner["learner_id"]
            session = app.call("POST", "/api/sessions", {"learner_id": lid, "resume": True})
            sid = session["session_id"]
            comparison_path = f"/api/learners/{lid}/sessions/{sid}/comparisons"
            saved = app.call("POST", comparison_path, COMPARISON)
            require(saved["item"]["source_type"] == "simulated" and saved["item"]["hardware_verified"] is False,
                    "comparison_not_simulated")
            require(saved["item"]["pending_plan"] == COMPARISON["pending_plan"], "initial_plan_changed")
            require(app.call("GET", comparison_path)["items"] == [saved["item"]], "comparison_readback_failed")
            state = app.call("GET", "/api/state")
            check_idle(state, stage)
            assert_uncompleted(state["session"])
            require(app.call("GET", f"/api/learners/{lid}/memories") == [], "unexpected_initial_slow_memory")
            stage.update(learner_id=lid, session_id=sid, comparison_id=saved["item"]["comparison_id"],
                         plans_unexecuted_and_unaccepted=True)
        baseline = database_snapshot(fixture, lid, sid)
        write_json(output / "after-new-create.json", baseline)

        with workstation(old, fixture, env, report, "old_read_and_write") as (app, initial, stage):
            require(learner_row(initial, lid)["name"] == NEW_NAME, "old_could_not_read_new_learner")
            older = app.call("POST", "/api/learners", {"name": OLD_NAME, "namespace": "simulator"})
            old_lid = older["learner_id"]
            require(old_lid != lid, "old_reused_learner_id")
            state = app.call("GET", "/api/state")
            check_idle(state, stage)
            require(learner_row(state, old_lid)["name"] == OLD_NAME and learner_row(state, lid)["name"] == NEW_NAME,
                    "old_shared_api_readback_failed")
            stage.update(original_learner_read=True, new_learner_id=old_lid,
                         stable_api_write_readback=True, new_feature_api_called=False)
        after_old = database_snapshot(fixture, lid, sid)
        write_json(output / "after-old-write.json", after_old)
        require(after_old["semantic"] == baseline["semantic"], "old_changed_new_comparison_or_plan_data")

        with workstation(new, fixture, env, report, "new_reopen_and_verify") as (app, initial, stage):
            require(learner_row(initial, lid)["name"] == NEW_NAME and learner_row(initial, old_lid)["name"] == OLD_NAME,
                    "new_could_not_read_both_generations")
            resumed = app.call("POST", "/api/sessions", {"learner_id": lid, "resume": True})
            require(resumed["session_id"] == sid, "original_session_not_resumed")
            require(app.call("GET", comparison_path)["items"] == [saved["item"]], "new_comparison_projection_changed")
            state = app.call("GET", "/api/state")
            check_idle(state, stage)
            assert_uncompleted(state["session"])
            require(state["session"]["state"] == baseline["semantic"]["state"], "new_task_state_changed")
            require(app.call("GET", f"/api/learners/{lid}/memories") == [], "new_promoted_plan_to_slow_memory")
            require(app.call("GET", f"/api/learners/{old_lid}/memories") == [], "old_learner_has_unexpected_memory")
            stage.update(both_learners_read=True, comparison_unchanged=True,
                         plan_uncompleted=True, slow_memories_empty=True)
        final = database_snapshot(fixture, lid, sid)
        write_json(output / "after-new-reopen.json", final)
        require(final["semantic"] == baseline["semantic"], "reopen_changed_new_comparison_or_plan_data")
        require(sha256(new) == NEW_SHA256 and sha256(old) == OLD_SHA256, "input_executable_changed")
        report.update(passed=True, comparison_and_plan_semantics_preserved=True,
                      semantic_sha256=baseline["semantic_sha256"],
                      schema_unchanged_across_stages=baseline["schema_sha256"] == after_old["schema_sha256"] == final["schema_sha256"],
                      old_learner_id=old_lid, new_learner_id=lid,
                      installed_executable_unchanged=True, frozen_new_executable_unchanged=True)
    except Exception as error:
        report.update(error_type=type(error).__name__,
                      error_code=str(error) if isinstance(error, SmokeFailure) else "unexpected_exception_no_body_logged")
    finally:
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "stages": len(report["stages"]),
                          "report": str(output / "report.json"), "error_code": report.get("error_code")}))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--local-synthetic", action="store_true", required=True)
    args = parser.parse_args()
    return run(args.root.resolve(), args.output.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
