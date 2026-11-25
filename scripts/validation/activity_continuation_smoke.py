"""Real HTTP continuation workflow on a fresh synthetic, model/device-free DB."""
import argparse
import datetime
import hashlib
import http.client
import json
import sqlite3
from pathlib import Path

from database_compatibility_smoke import (
    MAX_RESPONSE_BYTES, OLD_BINARY, OLD_SHA256, check_idle, isolated_environment,
    require, workstation, write_json,
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def continuation_snapshot(fixture):
    path = fixture / "runtime/data/simulator.sqlite3"
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as db:
        require(db.execute("PRAGMA integrity_check").fetchone()[0] == "ok", "fixture_integrity_failed")
        tables = ["sessions", "events", "education_event_meta", "memories",
                  "memory_activity_bindings", "session_continuations"]
        rows = {table: db.execute("SELECT * FROM " + table + " ORDER BY rowid").fetchall()
                for table in tables}
    return {"sha256": hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest(),
            "counts": {table: len(value) for table, value in rows.items()}}


def stable(state):
    return {"session": state["session"], "audio": state["audio"],
            "voice_loop": state["voice_loop"], "ptz": state["ptz"],
            "model_requests": state["kimi"]["metrics"].get("requests", 0)}


def rejected(app, path, payload, report, name, method="POST"):
    before = stable(app.call("GET", "/api/state"))
    connection = http.client.HTTPConnection("127.0.0.1", app.port, timeout=5)
    try:
        connection.request(method, path, json.dumps(payload) if payload is not None else None,
                           {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + app.token})
        response = connection.getresponse()
        raw = response.read(MAX_RESPONSE_BYTES + 1)
        require(response.status == 400 and len(raw) <= MAX_RESPONSE_BYTES, name + "_not_rejected")
    finally:
        connection.close()
    require(stable(app.call("GET", "/api/state")) == before, name + "_mutated_state")
    report["rejections"].append(name)


def remember(app, session, text):
    return app.call("POST", f"/api/activities/{session['activity_id']}/events",
                    {"source_type": "learner_report", "modules": ["control", "motor"], "report": text})


def new_session(app, lid):
    return app.call("POST", "/api/sessions", {"learner_id": lid, "resume": False})


def finish(app, session):
    app.call("POST", f"/api/sessions/{session['session_id']}/finish", {})


def intent(current, nonce):
    return {"client_submission_id": nonce,
            "expected_current_session_id": current["session_id"],
            "expected_generation": current["generation"]}


def run(root, output):
    require(output.is_relative_to(root / "runtime/goal-baselines/20261104"), "output_outside_baselines")
    require(not output.exists(), "output_exists")
    output.mkdir(parents=True)
    fixture = output / "fixture"
    (fixture / "runtime").mkdir(parents=True)
    write_json(fixture / "runtime/local-speech-policy.json", {"protocol_version": 1, "mode": "disabled", "warmup": "disabled"})
    env = isolated_environment(fixture)
    binary = root / "target/release/ai-tutor.exe"
    old_binary = root / OLD_BINARY
    require(digest(old_binary) == OLD_SHA256, "installed_old_hash_changed")
    protected = ["bin/ai-tutor.exe", "runtime/server.json", "runtime/speech-service.json",
                 "runtime/tts-service.json", "runtime/local-asr-policy.json",
                 "runtime/local-speech-policy.json", "runtime/tutor-delivery.json"]
    before = {name: digest(root / name) for name in protected}
    report = {"test_type": "actual_http_explicit_activity_continuation", "passed": False,
              "binary_sha256": digest(binary), "script_sha256": digest(Path(__file__)),
              "old_binary_sha256": OLD_SHA256,
              "helper_sha256": digest(Path(__file__).with_name("database_compatibility_smoke.py")),
              "synthetic_fixture_only": True, "sql_writes_by_probe": False,
              "model_requests_requested": False, "devices_requested": False,
              "browser_acceptance": "NOT_RUN", "stages": [], "rejections": []}
    try:
        with workstation(binary, fixture, env, report, "create_continue_and_replay") as (app, initial, stage):
            other = app.call("POST", "/api/learners", {"name": "SYNTHETIC OTHER", "namespace": "simulator"})
            lid_other = other["learner_id"]
            other_session = new_session(app, lid_other)
            remember(app, other_session, "OTHER OWNER rover observation")
            finish(app, other_session)
            learner = app.call("POST", "/api/learners", {"name": "SYNTHETIC CONTINUATION", "namespace": "simulator"})
            lid = learner["learner_id"]
            original = new_session(app, lid)
            remember(app, original, "ORIGIN ALPHA rover observation")
            finish(app, original)
            origin_memories = app.call("GET", f"/api/learners/{lid}/memories")
            require(len(origin_memories) == 1, "original_memory_missing")
            origin_mid = origin_memories[0]["memory_id"]
            unrelated = new_session(app, lid)
            remember(app, unrelated, "UNRELATED BETA rover observation")
            finish(app, unrelated)
            current = new_session(app, lid)
            catalog = f"/api/learners/{lid}/sessions"
            collected, cursor = [], None
            while True:
                page = app.call("GET", catalog + "?limit=1" + (f"&before={cursor}" if cursor else ""))
                require(page["learner_id"] == lid and len(page["items"]) <= 1, "catalog_owner_or_page")
                collected.extend(row["session_id"] for row in page["items"])
                cursor = page["next_before"]
                if cursor is None:
                    break
                require(len(collected) <= 10, "catalog_cursor_did_not_terminate")
            require(set(collected) == {original["session_id"], unrelated["session_id"], current["session_id"]}
                    and len(collected) == 3, "catalog_missing_or_foreign_rows")
            rejected(app, catalog + "?limit=51", None, report, "oversized_catalog", "GET")
            rejected(app, catalog + "?before=0", None, report, "invalid_catalog_cursor", "GET")
            path = f"/api/learners/{lid}/sessions/{original['session_id']}/continue"
            body = intent(current, "synthetic-explicit-continue-1")
            rejected(app, path, {**body, "expected_generation": body["expected_generation"] + 1}, report, "stale_generation")
            rejected(app, f"/api/learners/{lid}/sessions/{other_session['session_id']}/continue", body, report, "foreign_source")
            rejected(app, f"/api/learners/{lid}/sessions/{current['session_id']}/continue", body, report, "unfinished_source")
            response = app.call("POST", path, body)
            resumed = app.call("GET", "/api/state")["session"]
            new_sid, new_aid = resumed["session_id"], resumed["activity_id"]
            require(new_sid not in collected and new_aid not in {original["activity_id"], unrelated["activity_id"], current["activity_id"]}, "old_identity_reused")
            require(response["session_id"] == new_sid and not response["replayed"], "initial_receipt")
            require(resumed["continued_from"]["session_id"] == original["session_id"]
                    and resumed["continuation"]["client_submission_id"] == body["client_submission_id"], "state_cannot_identify_operation")
            state = resumed["state"]
            require(state["modules"] is None and state["requires_inventory_confirmation"]
                    and not state["reported_result"] and not state["attempts"]
                    and not state["session_preferences"] and state["stage"] == "goal_setting", "old_current_facts_copied")
            require([m["memory_id"] for m in resumed["recalled_memories"]] == [origin_mid], "unselected_activity_recalled")
            # This checks persisted state recognition; it does not simulate loss
            # at a browser transport boundary or claim a browser acceptance pass.
            report["state_echo_identifies_committed_operation"] = True
            before_replay = stable(app.call("GET", "/api/state"))
            replay = app.call("POST", path, body)
            require(replay["replayed"] and replay["session_id"] == new_sid
                    and stable(app.call("GET", "/api/state")) == before_replay, "replay_changed_state")
            remember(app, resumed, "RESUMED GAMMA rover new observation")
            progressed = stable(app.call("GET", "/api/state"))
            require(app.call("POST", path, body)["replayed"], "progressed_replay_missing")
            require(stable(app.call("GET", "/api/state")) == progressed, "replay_cancelled_new_progress")
            rejected(app, path, {**body, "expected_generation": body["expected_generation"] + 1}, report, "nonce_payload_changed")
            rejected(app, f"/api/learners/{lid}/sessions/{unrelated['session_id']}/continue", body, report, "nonce_target_changed")
            rejected(app, f"/api/activities/{original['activity_id']}/events",
                     {"source_type": "learner_report", "report": "late old activity"}, report, "old_activity_route")
            rejected(app, f"/api/sessions/{original['session_id']}/turns",
                     {"text": "late old session", "auto_speak": False}, report, "old_session_send")
            finish(app, resumed)
            all_memories = app.call("GET", f"/api/learners/{lid}/memories")
            gamma = [m for m in all_memories if "RESUMED GAMMA" in m["content"]]
            require(len(gamma) == 1, "resumed_memory_missing_or_ambiguous")
            gamma_mid = gamma[0]["memory_id"]
            finished_state = stable(app.call("GET", "/api/state"))
            require(app.call("POST", path, body)["replayed"], "finished_current_receipt_missing")
            require(stable(app.call("GET", "/api/state")) == finished_state, "replay_reopened_finished_session")
            next_current = new_session(app, lid)
            rejected(app, path, body, report, "replay_after_activity_switch")
            second = app.call("POST", f"/api/learners/{lid}/sessions/{new_sid}/continue",
                              intent(next_current, "synthetic-explicit-continue-2"))
            descendant = second["session"]
            ids = {m["memory_id"] for m in descendant["recalled_memories"]}
            require(ids == {origin_mid, gamma_mid}, "explicit_ancestor_chain_missing_or_contaminated")
            last_sid, last_aid = descendant["session_id"], descendant["activity_id"]
            check_idle(app.call("GET", "/api/state"), stage)
            report.update(learner_id=lid, origin_session_id=original["session_id"], resumed_session_id=new_sid,
                          last_session_id=last_sid, last_activity_id=last_aid, catalog_pages=3,
                          selected_memories_only=True, old_hardware_not_reused=True,
                          replay_did_not_cancel_new_progress=True, ancestor_chain_verified=True,
                          replay_did_not_reopen_finished_session=True)
        before_old = continuation_snapshot(fixture)
        with workstation(old_binary, fixture, env, report, "old_read_and_write_other_learner") as (app, initial, stage):
            require(any(row["learner_id"] == lid for row in initial["learners"]), "old_cannot_read_new_learner")
            old_created = app.call("POST", "/api/learners", {"name": "SYNTHETIC OLD WRITE", "namespace": "simulator"})
            require(old_created["learner_id"] not in {lid, lid_other}, "old_reused_learner")
            check_idle(app.call("GET", "/api/state"), stage)
        after_old = continuation_snapshot(fixture)
        require(after_old == before_old, "old_changed_continuation_or_source_rows")
        report["new_old_new_compatibility"] = {"preserved": before_old,
            "scope": "Old startup and stable learner read/write preserve continuation/source tables; not all downgrade operations."}
        with workstation(binary, fixture, env, report, "restart_live_link_and_forget") as (app, initial, stage):
            restored = app.call("POST", "/api/sessions", {"learner_id": lid, "resume": True})
            require(restored["session_id"] == last_sid and restored["activity_id"] == last_aid, "restart_identity_changed")
            require(restored["continued_from"]["session_id"] == new_sid
                    and restored["continuation"]["client_submission_id"] == "synthetic-explicit-continue-2", "restart_link_missing")
            require({m["memory_id"] for m in restored["recalled_memories"]} == {origin_mid, gamma_mid}, "restart_live_chain_changed")
            report["restart_preserves_live_link_and_exact_memories"] = True
            app.call("DELETE", f"/api/learners/{lid}/memories/{origin_mid}")
            after_forget = app.call("GET", "/api/state")["session"]
            require(after_forget["continuation"] is None and after_forget["continued_from"] is None,
                    "forgotten_selection_link_still_live")
            require(not after_forget["recalled_memories"], "broken_chain_still_recalled")
            remaining = {m["memory_id"] for m in app.call("GET", f"/api/learners/{lid}/memories")}
            require(origin_mid not in remaining and gamma_mid in remaining, "forget_changed_unrelated_memory")
            check_idle(app.call("GET", "/api/state"), stage)
        with workstation(binary, fixture, env, report, "restart_broken_link") as (app, initial, stage):
            restored = app.call("POST", "/api/sessions", {"learner_id": lid, "resume": True})
            require(restored["session_id"] == last_sid and restored["activity_id"] == last_aid, "restart_after_forget_identity_changed")
            require(restored["continuation"] is None and restored["continued_from"] is None,
                    "restart_revived_forgotten_selection")
            require(not restored["recalled_memories"], "restart_revived_broken_chain")
            remaining = {m["memory_id"] for m in app.call("GET", f"/api/learners/{lid}/memories")}
            require(origin_mid not in remaining and gamma_mid in remaining, "restart_changed_retained_memory")
            check_idle(app.call("GET", "/api/state"), stage)
            report["restart_preserves_identity_without_reviving_deleted_link"] = True
            report["forgotten_source_not_recalled"] = True
        report["passed"] = True
    except Exception as error:
        report.update(error_type=type(error).__name__, error_code=str(error))
    finally:
        report["protected_files_unchanged"] = before == {name: digest(root / name) for name in protected}
        report["passed"] = report["passed"] and report["protected_files_unchanged"]
        report["checked_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        write_json(output / "report.json", report)
        print(json.dumps({"passed": report["passed"], "stages": len(report["stages"]),
                          "rejection_count": len(report["rejections"]), "error_code": report.get("error_code")}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--local-synthetic", action="store_true", required=True)
    args = parser.parse_args()
    raise SystemExit(run(args.root.resolve(), args.output.resolve()))
