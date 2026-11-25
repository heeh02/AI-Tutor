"""Four frozen comparison cases, eight explicit production memory-review actions.

A is the supplied baseline executable; B is the supplied candidate executable.
The fixed pair order is AB, BA, AB, BA. No review POST is retried or replaced.
Seed creation uses HTTP only. SQLite reads are restricted to this run's new
fixtures; no SQL writes, source databases, credentials or voice policies are
copied. Native providers inherit the normal official Kimi environment, which
this script neither inspects nor records. Main processes stop via native --stop.

Mechanical source/type/retention checks are separate from human semantic review.
An empty candidate list is never counted as a passing model result.
"""
import argparse
from collections import Counter
from contextlib import closing
import datetime
import hashlib
import http.client
import json
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import subprocess
import time


# Exact provider allowlist from scripts/voice-bridge/prepare_conversation_demo.py.
# The memory prompt is embedded in each executable, not copied from source-root.
PROVIDER_FILES = (
    "prompts/tutor_core.md", "prompts/tutor-agent.md",
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json",
    "runtime/kimi/verification-rust.json",
)
DATABASE_FILES = tuple(f"runtime/data/{name}.sqlite3" for name in ("real", "simulator", "workstation"))
PAIR_ORDER = ("AB", "BA", "AB", "BA")
PROMPT_VERSIONS = {"A": "educational-memory-prompt-v4", "B": "educational-memory-prompt-v5"}
MAX_BYTES = 8 * 1024 * 1024
OBSERVATION_SECONDS = 120
METRIC_COUNTS = ("requests", "completed", "errors", "cancelled", "retries",
                 "consolidation_requests", "acp_memory_requests")


class PairFailure(Exception):
    """Controlled diagnostic codes; no environment or credential content."""


def require(condition, code):
    if not condition:
        raise PairFailure(code)


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False), encoding="utf-8")


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for data in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(data)
    return hasher.hexdigest()


def json_digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                     separators=(",", ":"), allow_nan=False).encode("utf-8")).hexdigest()


def read_cases(path):
    value = json.loads(path.read_text(encoding="utf-8"))
    require(value.get("namespace") == "simulator" and value.get("frozen_before_execution") is True,
            "cases_must_be_frozen_simulator_input")
    require(value.get("case_count") == 4 and len(value.get("cases", [])) == 4, "exactly_four_cases_required")
    require(value.get("pair_execution_order") == list(PAIR_ORDER), "frozen_pair_order_changed")
    require(isinstance(value.get("schema_version"), str), "cases_schema_version_missing")
    seen = set()
    for case in value["cases"]:
        cid = case.get("case_id")
        require(isinstance(cid, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,80}", cid), "invalid_case_id")
        require(cid not in seen, "duplicate_case_id")
        seen.add(cid)
        require(case.get("language") in ("zh", "en") and isinstance(case.get("comparison_request"), dict),
                "case_language_or_comparison_missing")
        require(isinstance(case.get("expected"), dict), "case_expected_rubric_missing")
    require(Counter(case["language"] for case in value["cases"]) == {"zh": 2, "en": 2},
            "paired_cases_require_two_zh_two_en")
    return value


class Main:
    def __init__(self, executable, root, logs, label):
        self.executable, self.root, self.logs = executable, root, logs
        self.descriptor = root / "runtime/server.json"
        self.token = None
        self.process = None
        self.streams = []
        self.stop_attempted = False
        self.record = {"label": label, "fixture": str(root), "clean_exit": False, "forced_stop": False}

    def start(self):
        require(not self.descriptor.exists(), "new_fixture_has_server_descriptor")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            self.port = reservation.getsockname()[1]
        self.streams = [(self.logs / f"{self.record['label']}.{kind}.log").open("xb")
                        for kind in ("stdout", "stderr")]
        self.process = subprocess.Popen(
            [str(self.executable), "--root", str(self.root), "--port", str(self.port), "--no-browser"],
            cwd=self.root, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            stdin=subprocess.DEVNULL, stdout=self.streams[0], stderr=self.streams[1],
        )
        self.record.update(pid=self.process.pid, port=self.port, started_at=utc())
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "main_exited_before_ready")
            if self.token is None and self.descriptor.exists():
                try:
                    info = json.loads(self.descriptor.read_text(encoding="utf-8"))
                except (ValueError, OSError):
                    time.sleep(0.05)
                    continue
                require((info.get("pid"), info.get("port")) == (self.process.pid, self.port),
                        "descriptor_not_owned_main")
                token = info.get("token")
                require(isinstance(token, str) and 32 <= len(token) <= 256, "owned_auth_descriptor_invalid")
                self.token = token
            if self.token:
                try:
                    state = self.api("GET", "/api/state")
                    idle_devices(state)
                    self.record["initial_provider_available"] = state["kimi"].get("available")
                    return state
                except (ConnectionRefusedError, ConnectionResetError, TimeoutError):
                    pass
            time.sleep(0.05)
        raise PairFailure("main_startup_timeout")

    def request(self, method, path, body=None, timeout_seconds=5):
        require(self.process is not None and self.process.poll() is None and self.token is not None,
                "owned_main_not_available")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout_seconds)
        try:
            payload = json.dumps(body, ensure_ascii=False).encode("utf-8") if body is not None else None
            connection.request(method, path, payload,
                               {"Content-Type": "application/json", "Cookie": "ai_tutor_local=" + self.token})
            response = connection.getresponse()
            raw = response.read(MAX_BYTES + 1)
            self.record["last_http"] = {"method": method, "path": path, "status": response.status}
            require(len(raw) <= MAX_BYTES and "json" in response.getheader("Content-Type", ""),
                    "invalid_http_response")
            return response.status, json.loads(raw)
        finally:
            connection.close()

    def api(self, method, path, body=None, timeout_seconds=5):
        status, value = self.request(method, path, body, timeout_seconds)
        require(status == 200, f"http_{method}_{status}")
        return value

    def stop(self):
        if self.stop_attempted:
            return
        self.stop_attempted = True
        stopper = None
        try:
            if self.process is not None and self.process.poll() is None:
                info = json.loads(self.descriptor.read_text(encoding="utf-8")) if self.descriptor.exists() else {}
                if (info.get("pid"), info.get("port")) == (self.process.pid, self.port):
                    with (self.logs / f"{self.record['label']}.stop.stdout.log").open("xb") as out, \
                            (self.logs / f"{self.record['label']}.stop.stderr.log").open("xb") as err:
                        stopper = subprocess.Popen([str(self.executable), "--root", str(self.root), "--stop"],
                                                   cwd=self.root, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                        self.record["stop_pid"] = stopper.pid
                        try:
                            self.record["stop_exit_code"] = stopper.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            self.record["stop_wait_timed_out"] = True
                else:
                    self.record["stop_skipped_unbound_descriptor"] = True
            if self.process is not None:
                try:
                    self.process.wait(timeout=25)
                except subprocess.TimeoutExpired:
                    self.record["main_wait_timed_out"] = True
        finally:
            self.record["exit_code"] = self.process.poll() if self.process is not None else None
            self.record["main_still_running"] = self.process is not None and self.process.poll() is None
            self.record["stopper_still_running"] = stopper is not None and stopper.poll() is None
            self.record["clean_exit"] = (self.record.get("stop_exit_code") == 0
                                         and self.record["exit_code"] == 0
                                         and not self.record["stopper_still_running"])
            self.record["stopped_at"] = utc()
            self.token = None
            for stream in self.streams:
                stream.close()


def idle_devices(state):
    require(state["camera"]["state"] == "closed", "unexpected_open_camera")
    require(state["audio"]["recording"] is False and state["audio"].get("state") == "idle", "unexpected_audio_activity")
    require(state["ptz"]["mode"] == "off" and not state.get("actuator_execution"), "unexpected_actuator_activity")
    for name in ("local_speech", "local_asr", "final_asr"):
        service = state.get(name)
        if isinstance(service, dict):
            require(not service.get("active") and not service.get("busy") and not service.get("ready"),
                    name + "_unexpectedly_active")


def quiescent(state):
    idle_devices(state)
    metrics = state["kimi"]["metrics"]
    return state["kimi"].get("running", 0) == 0 and not metrics.get("active_acp_memory")


def owned_database(root, immutable=False):
    path = root / "runtime/data/simulator.sqlite3"
    require(path.is_file(), "fixture_simulator_database_missing")
    uri = path.as_uri() + "?mode=ro" + ("&immutable=1" if immutable else "")
    connection = sqlite3.connect(uri, uri=True, timeout=3)
    connection.row_factory = sqlite3.Row
    return connection


def audit(root, lid, sid, job_id=None, immutable=False):
    """Read-only snapshot of this owned fixture; never queries source-root DBs."""
    with closing(owned_database(root, immutable)) as db:
        context = db.execute("""SELECT s.learner_id,s.session_id,s.activity_id,s.generation,
            l.epoch,v.revision AS state_revision FROM sessions s JOIN learners l ON l.learner_id=s.learner_id
            JOIN session_state_versions v ON v.session_id=s.session_id AND v.learner_id=s.learner_id
            WHERE s.learner_id=? AND s.session_id=? AND v.namespace='simulator'""", (lid, sid)).fetchone()
        require(context is not None, "fixture_context_missing")
        rows = db.execute("""SELECT e.event_id,e.learner_id,e.session_id,e.source_type,e.content,
            m.kind AS event_kind,m.version AS event_version,m.status AS event_status
            FROM events e JOIN education_event_meta m ON m.event_id=e.event_id
            AND m.learner_id=e.learner_id AND m.session_id=e.session_id
            WHERE e.learner_id=? AND e.session_id=? AND m.namespace='simulator'
            AND m.status='active' ORDER BY e.event_id""", (lid, sid)).fetchall()
        sources = []
        for row in rows:
            source = dict(row)
            source["content"] = json.loads(source["content"])
            sources.append(source)
        result = {"context": dict(context), "sources": sources,
                  "memory_count": db.execute("SELECT COUNT(*) FROM memories").fetchone()[0],
                  "job_count": db.execute("SELECT COUNT(*) FROM memory_jobs").fetchone()[0]}
        if job_id:
            row = db.execute("SELECT * FROM memory_jobs WHERE job_id=? AND learner_id=? AND session_id=? AND namespace='simulator'",
                             (job_id, lid, sid)).fetchone()
            require(row is not None, "target_job_missing")
            job = dict(row)
            # Lease tokens are internal authority, not model evidence.
            job.pop("lease_owner", None)
            job.pop("lease_token", None)
            job["source_snapshot"] = json.loads(job["source_snapshot"])
            if isinstance(job.get("limits_json"), str):
                job["limits"] = json.loads(job.pop("limits_json"))
            result["job"] = job
            result["attempts"] = [dict(row) for row in db.execute(
                "SELECT attempt_no,claimed_at_ms,started_at_ms,status FROM memory_job_attempts WHERE job_id=? ORDER BY attempt_no",
                (job_id,))]
            selected = {binding["event_id"]: binding["event_version"] for binding in job["source_snapshot"]}
            bound = [source for source in sources if source["event_id"] in selected]
            require(len(bound) == len(selected) == len(job["source_snapshot"]), "job_source_identity_missing_or_duplicated")
            require(all(source["event_version"] == selected[source["event_id"]] for source in bound), "job_source_version_changed")
            result["job_sources"] = bound
            result["normalized_prompt_input"] = {
                "learner_id": job["learner_id"], "session_id": job["session_id"],
                "_memory_attempt": job["attempts"],
                "context": {"namespace": job["namespace"], "output_status": "shadow_only", "sources": [
                    {key: source[key] for key in ("event_id", "event_version", "event_kind", "source_type", "content")}
                    for source in bound]},
            }
        return result


def copy_initial(source_root, ui, seed):
    paths = []
    for relative in PROVIDER_FILES:
        source, target = source_root / relative, seed / relative
        require(source.is_file() and not source.is_symlink(), "required_provider_file_missing_or_linked")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        paths.append(relative)
    for source in sorted(ui.rglob("*")):
        require(not source.is_symlink(), "ui_symlinks_not_copied")
        if source.is_file():
            relative = Path("frontend/dist") / source.relative_to(ui)
            target = seed / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            paths.append(relative.as_posix())
    return paths


def confirm_closed_databases(seed):
    hashes = {}
    for relative in DATABASE_FILES:
        path = seed / relative
        require(path.is_file(), "seed_database_missing")
        require(not Path(str(path) + "-wal").exists() and not Path(str(path) + "-shm").exists(),
                "seed_database_has_wal_or_shm_after_exit")
        with closing(sqlite3.connect(path.as_uri() + "?mode=ro&immutable=1", uri=True)) as db:
            require(db.execute("PRAGMA quick_check").fetchone()[0] == "ok", "seed_database_integrity_check_failed")
        hashes[relative] = digest(path)
    return hashes


def clone_seed(seed, destination, files):
    destination.mkdir()
    copied = {}
    for relative in files:
        source, target = seed / relative, destination / relative
        require(source.is_file() and not source.is_symlink(), "owned_seed_copy_source_invalid")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        copied[relative] = digest(target)
        require(copied[relative] == digest(source), "owned_seed_copy_hash_mismatch")
    return copied


def seed_cases(app, cases):
    initial = app.api("GET", "/api/state")
    require(initial["kimi"]["metrics"].get("requests", 0) == 0, "seed_started_model_request")
    identities = []
    for index, case in enumerate(cases):
        learner = app.api("POST", "/api/learners", {"name": f"PAIR SYNTHETIC {index + 1} {case['language']}",
                                                    "language": case["language"], "namespace": "simulator"})
        lid = learner["learner_id"]
        session = app.api("POST", "/api/sessions", {"learner_id": lid, "resume": False})
        sid = session["session_id"]
        comparison = app.api("POST", f"/api/learners/{lid}/sessions/{sid}/comparisons", case["comparison_request"])
        require(comparison["item"]["source_type"] == "simulated" and comparison["item"]["hardware_verified"] is False,
                "comparison_source_not_simulated")
        require(app.api("GET", f"/api/learners/{lid}/memories") == [], "seed_has_automatic_memory")
        identities.append({"case_id": case["case_id"], "learner_id": lid, "session_id": sid,
                           "comparison": comparison})
    final = app.api("GET", "/api/state")
    idle_devices(final)
    require(final["kimi"]["metrics"].get("requests", 0) == 0
            and final["kimi"]["metrics"].get("consolidation_requests", 0) == 0, "seed_was_not_zero_model")
    return identities, final["kimi"]


def mechanical_review(candidates, expected):
    rows = []
    for candidate in candidates:
        payload, sources = candidate.get("payload", {}), candidate.get("sources", [])
        kinds = sorted({source.get("event_kind") or "unknown" for source in sources})
        cited = set(payload.get("source_event_ids", []))
        resolved = {source.get("event_id") for source in sources}
        rows.append({"candidate_id": candidate.get("candidate_id"), "kind": payload.get("kind") or "unknown",
                     "source_kinds": kinds, "source_ids_complete": bool(cited) and cited == resolved,
                     "plan_sources_accepted": bool(sources) and all(source.get("content", {}).get("accepted") is True for source in sources),
                     "can_retain": candidate.get("can_retain") is True, "blocked_reason": candidate.get("blocked_reason"),
                     "source_grade": payload.get("source_grade"), "scope": payload.get("scope")})
    observations = [row for row in rows if row["kind"] == "trial_observation"]
    plans = [row for row in rows if row["kind"] == "accepted_pending_experiment"]
    gates = {"nonempty_candidates": bool(rows), "complete_source_ids": all(row["source_ids_complete"] for row in rows),
             "source_grade": all(row["source_grade"] == expected.get("source_grade", "simulated") for row in rows),
             "scope": all(row["scope"] == expected.get("scope", "current_activity") for row in rows),
             "observation_sources": all(row["source_kinds"] == expected.get("observation_source_kinds", ["observation"]) for row in observations),
             "accepted_plan_sources": all(row["source_kinds"] == expected.get("accepted_plan_source_kinds", ["plan"]) for row in plans),
             "accepted_plan_has_explicit_acceptance": all(row["plan_sources_accepted"] for row in plans)}
    for field, actual in (("observation_required", bool(observations)), ("accepted_plan_required", bool(plans)),
                          ("retainable_observation_required", any(row["can_retain"] for row in observations)),
                          ("retainable_accepted_plan_required", any(row["can_retain"] for row in plans))):
        gates[field] = not expected.get(field, False) or actual
    gates["accepted_plan_forbidden"] = not expected.get("accepted_plan_forbidden", False) or not plans
    return {"candidate_count": len(rows), "kind_counts": dict(Counter(row["kind"] for row in rows)),
            "retainable_count": sum(row["can_retain"] for row in rows), "candidate_source_checks": rows,
            "gates": gates, "mechanical_pass": all(gates.values()),
            "semantic_review_status": "PENDING_HUMAN_REVIEW",
            "semantic_rubric": {key: expected.get(key) for key in ("observation_must_preserve", "plan_must_preserve",
                                "uncertainty_must_remain", "must_not_infer", "no_completed_B_result")},
            "mechanical_pass_is_not_semantic_or_teaching_effectiveness_pass": True}


def metric_delta(before, after):
    return {key: after.get(key, 0) - before.get(key, 0) for key in METRIC_COUNTS
            if type(after.get(key, 0)) is int and type(before.get(key, 0)) is int}


def run_slot(app, other, case, identity, record, slot_file, seed_source, peer_input):
    lid, sid = identity["learner_id"], identity["session_id"]
    record.update(started_at=utc(), status="preparing", review_post_count=0)
    state = app.api("GET", "/api/state")
    require(quiescent(state) and quiescent(other.api("GET", "/api/state")), "paired_provider_overlap_before_slot")
    selected = app.api("POST", "/api/sessions", {"learner_id": lid, "resume": True})
    require(selected["session_id"] == sid, "resumed_wrong_seed_session")
    path = f"/api/learners/{lid}/sessions/{sid}/memory-review"
    review_before = app.api("GET", path)
    input_audit = audit(app.root, lid, sid)
    record.update(selection=selected, review_before=review_before, input_audit=input_audit)
    write_json(slot_file, record)
    require(input_audit["memory_count"] == 0, "memory_present_before_slot")
    require(input_audit["sources"] == seed_source, "seed_source_identity_or_content_changed")
    require(all(review_before[key] == input_audit["context"][key] for key in ("generation", "epoch", "state_revision")),
            "public_review_and_sql_context_disagree")
    comparable = {"context": input_audit["context"], "sources": input_audit["sources"]}
    if peer_input is not None:
        require(comparable == peer_input, "paired_pre_request_input_mismatch")
    body = {f"expected_{key}": review_before[key] for key in ("generation", "epoch", "state_revision")}
    state_before = app.api("GET", "/api/state")
    record.update(paired_input_sha256=json_digest(comparable), request=body,
                  provider_before=state_before["kimi"], workstation_metrics_before=state_before["metrics"])
    write_json(slot_file, record)
    start = time.monotonic()
    deadline = start + OBSERVATION_SECONDS
    # Exactly one explicit action. A lost response is never replaced or retried.
    record["review_post_count"] = 1
    record["status"] = "action_sent"
    write_json(slot_file, record)
    http_status, posted = app.request("POST", path, body)
    record["post_response"] = {"http_status": http_status, "body": posted}
    final_review = posted if http_status == 200 else app.api("GET", path)
    observations = []
    state_after = None
    while http_status == 200:
        if time.monotonic() >= deadline:
            break
        state = app.api("GET", "/api/state", timeout_seconds=max(0.1, min(5, deadline - time.monotonic())))
        state_after = state
        observations.append({"elapsed_seconds": round(time.monotonic() - start, 3),
                             "review_running": final_review.get("review_running"),
                             "review_outcome": final_review.get("review_outcome"),
                             "job": final_review.get("job"),
                             "active_acp_memory": state["kimi"]["metrics"].get("active_acp_memory")})
        if final_review.get("review_running") is False and quiescent(state):
            break
        if time.monotonic() >= deadline:
            break
        time.sleep(min(0.5, max(0, deadline - time.monotonic())))
        if time.monotonic() >= deadline:
            break
        final_review = app.api("GET", path, timeout_seconds=max(0.1, min(5, deadline - time.monotonic())))
    # Capture before selecting a different profile, which invalidates old jobs.
    if state_after is None:
        # A known HTTP rejection did not launch a review; this is not another
        # observation window or a substitute action.
        require(http_status != 200, "observation_budget_elapsed_before_first_state")
        state_after = app.api("GET", "/api/state")
    job_id = (final_review.get("job") or {}).get("job_id")
    after = audit(app.root, lid, sid, job_id)
    require(after["memory_count"] == 0, "background_review_applied_memory")
    summary = state_after["kimi"]["metrics"].get("last_acp_memory")
    if not isinstance(summary, dict) or summary.get("job_id") != job_id:
        summary = None
    candidates = final_review.get("candidates", [])
    record.update(finished_at=utc(), elapsed_seconds=round(time.monotonic() - start, 3),
                  polling=observations, review_final=final_review, output_audit=after, candidates=candidates,
                  provider_summary=summary, provider_after=state_after["kimi"],
                  provider_metric_delta=metric_delta(state_before["kimi"]["metrics"], state_after["kimi"]["metrics"]),
                  workstation_metrics_after=state_after["metrics"], mechanical=mechanical_review(candidates, case["expected"]))
    settled = final_review.get("review_running") is False and quiescent(state_after)
    record["status"] = "observed_terminal" if settled else "observation_timeout"
    record["execution_success"] = (http_status == 200 and settled and final_review.get("review_outcome") == "shadow_staged"
                                   and summary is not None and summary.get("success") is True
                                   and summary.get("cleanup_confirmed") is True
                                   and record["provider_metric_delta"].get("consolidation_requests") == 1
                                   and record["provider_metric_delta"].get("retries", 0) == 0)
    if job_id:
        require(after["job"]["attempts"] == 1, "slot_spent_more_than_one_attempt")
        require(after["job_sources"] == input_audit["sources"], "actual_job_sources_differ_from_pre_request_input")
        require(all(after["job"][key] == input_audit["context"][key] for key in ("generation", "epoch", "state_revision")),
                "actual_job_versions_differ_from_pre_request_input")
        record["prompt_version"] = after["job"]["prompt_version"]
        require(record["prompt_version"] == PROMPT_VERSIONS[record["arm"]], "executable_prompt_version_does_not_match_arm")
    write_json(slot_file, record)
    require(record["provider_metric_delta"].get("retries", 0) == 0
            and record["provider_metric_delta"].get("consolidation_requests", 0) <= 1,
            "unexpected_provider_retry_no_further_slots_permitted")
    require(settled, "slot_not_settled_no_further_slots_permitted")
    return comparable


def summarize(slots, cases):
    arms = {}
    for arm in ("A", "B"):
        rows = [row for row in slots if row["arm"] == arm]
        kinds = Counter()
        for row in rows:
            kinds.update(row.get("mechanical", {}).get("kind_counts", {}))
        arms[arm] = {"planned_slots": 4, "explicit_actions": sum(row.get("review_post_count", 0) for row in rows),
                     "execution_successes": sum(row.get("execution_success", False) for row in rows),
                     "nonempty_candidate_slots": sum(row.get("mechanical", {}).get("candidate_count", 0) > 0 for row in rows),
                     "mechanical_passes": sum(row.get("execution_success", False) and row.get("mechanical", {}).get("mechanical_pass", False) for row in rows),
                     "kind_counts": dict(kinds), "semantic_review": "PENDING_HUMAN_REVIEW"}
    pairs = []
    for case in cases:
        rows = {row["arm"]: row for row in slots if row["case_id"] == case["case_id"]}
        a = rows["A"].get("output_audit", {}).get("normalized_prompt_input")
        b = rows["B"].get("output_audit", {}).get("normalized_prompt_input")
        job_a = rows["A"].get("output_audit", {}).get("job")
        job_b = rows["B"].get("output_audit", {}).get("job")
        binding_equal = (isinstance(job_a, dict) and isinstance(job_b, dict)
                         and all(job_a.get(key) == job_b.get(key) for key in ("generation", "state_revision", "epoch")))
        pairs.append({"case_id": case["case_id"], "actual_prompt_inputs_equal_except_random_ids_and_prompt_version": a is not None and a == b,
                      "store_job_binding_generation_state_revision_epoch_equal": binding_equal,
                      "A_prompt_version": rows["A"].get("prompt_version"), "B_prompt_version": rows["B"].get("prompt_version"),
                      "A_mechanical": rows["A"].get("mechanical"), "B_mechanical": rows["B"].get("mechanical")})
    return {"arms": arms, "pairs": pairs, "semantic_review_status": "PENDING_HUMAN_REVIEW",
            "limited_four_case_experiment_not_statistical_or_teaching_effectiveness_proof": True}


def run(args):
    source_root, ui, output = args.source_root.resolve(), args.ui.resolve(), args.output.resolve()
    binaries = {"A": args.baseline_executable.resolve(), "B": args.candidate_executable.resolve()}
    cases_path = args.cases.resolve()
    frozen = read_cases(cases_path)
    cases = frozen["cases"]
    require(source_root.is_dir() and (ui / "index.html").is_file(), "source_root_or_ui_missing")
    require(all(path.is_file() for path in binaries.values()), "paired_executable_missing")
    require(not output.exists() and not output.is_relative_to(ui), "output_must_be_new_and_outside_ui")
    output.mkdir(parents=True, exist_ok=False)
    logs, artifacts, seed = output / "logs", output / "slots", output / "seed"
    logs.mkdir(); artifacts.mkdir(); seed.mkdir()
    report = {"experiment": "production_api_memory_plan_split_pair", "started_at": utc(), "completed": False,
              "frozen_cases_sha256": digest(cases_path), "script_sha256": digest(Path(__file__)),
              "executables": {arm: {"path": str(path), "sha256": digest(path)} for arm, path in binaries.items()},
              "source_databases_copied": False, "credentials_copied_or_logged_by_script": False,
              "environment": "normal official Kimi environment inherited, not inspected or recorded",
              "sqlite_access": "mode=ro; owned fixtures only; no SQL writes", "devices_requested": False,
              "ui_browser_run": False, "review_post_budget": 8, "automatic_retries_or_replacement_slots": False,
              "pair_order": list(PAIR_ORDER), "processes": [], "slots": []}
    report["expected_prompt_versions"] = PROMPT_VERSIONS
    report["prompt_input_provenance"] = "Persisted job.source_snapshot and resolved original source records, serialized using the worker contract; random request/job IDs and prompt_version omitted. Generation/state_revision/epoch are audited separately as Store job bindings and are not prompt fields."
    apps = []
    for index, (case, order) in enumerate(zip(cases, PAIR_ORDER)):
        for arm in order:
            report["slots"].append({"slot": len(report["slots"]) + 1, "case_index": index, "case_id": case["case_id"],
                                    "arm": arm, "status": "not_run", "review_post_count": 0})
    write_json(output / "cases.frozen.json", frozen)
    write_json(output / "method.json", {"pair_order": list(PAIR_ORDER), "review_post_budget": 8,
               "observe_seconds_per_slot": OBSERVATION_SECONDS, "no_automatic_retry": True,
               "A": report["executables"]["A"], "B": report["executables"]["B"],
               "expected_prompt_versions": PROMPT_VERSIONS,
               "cases_sha256": report["frozen_cases_sha256"], "semantic_review": "separate human review required"})
    try:
        seed_files = copy_initial(source_root, ui, seed)
        report["provider_files_sha256"] = {name: digest(seed / name) for name in PROVIDER_FILES}
        seeder = Main(binaries["A"], seed, logs, "seed-baseline")
        apps.append(seeder); report["processes"].append(seeder.record)
        seeder.start()
        identities, seed_provider = seed_cases(seeder, cases)
        seeder.stop()
        require(seeder.record["clean_exit"], "seed_main_did_not_exit_cleanly")
        report["seed_database_sha256"] = confirm_closed_databases(seed)
        report["seed_provider_zero_model"] = seed_provider
        seed_inputs = {identity["case_id"]: audit(seed, identity["learner_id"], identity["session_id"], immutable=True)
                       for identity in identities}
        require(all(row["memory_count"] == 0 and row["job_count"] == 0 for row in seed_inputs.values()), "seed_not_empty_of_memories_and_jobs")
        write_json(output / "seed-identities-and-inputs.json", {"identities": identities, "inputs": seed_inputs})
        arms = {}
        for arm, name in (("A", "baseline"), ("B", "candidate")):
            root = output / name
            report[f"{name}_initial_copy_sha256"] = clone_seed(seed, root, seed_files + list(DATABASE_FILES))
            app = Main(binaries[arm], root, logs, name)
            apps.append(app); report["processes"].append(app.record)
            state = app.start()
            require(state["kimi"]["metrics"].get("requests", 0) == 0, "arm_started_unrequested_model")
            arms[arm] = app
        first_inputs = {}
        for record in report["slots"]:
            index, arm = record["case_index"], record["arm"]
            case, identity = cases[index], identities[index]
            slot_file = artifacts / f"{record['slot']:02d}-{case['case_id']}-{arm}.json"
            try:
                comparable = run_slot(arms[arm], arms["B" if arm == "A" else "A"], case, identity,
                                      record, slot_file, seed_inputs[case["case_id"]]["sources"], first_inputs.get(case["case_id"]))
                first_inputs.setdefault(case["case_id"], comparable)
            except Exception as error:
                record["error_code"] = str(error) if isinstance(error, PairFailure) else type(error).__name__
                record["status"] = "failed_no_replacement"
                write_json(slot_file, record)
                raise
            write_json(output / "report.json", report)
        report["comparison"] = summarize(report["slots"], cases)
        report["completed"] = (sum(slot["review_post_count"] for slot in report["slots"]) == 8
                               and all(pair["actual_prompt_inputs_equal_except_random_ids_and_prompt_version"]
                                       and pair["store_job_binding_generation_state_revision_epoch_equal"]
                                       for pair in report["comparison"]["pairs"]))
    except Exception as error:
        report["error_code"] = str(error) if isinstance(error, PairFailure) else type(error).__name__
    finally:
        for app in reversed(apps):
            try:
                app.stop()
            except Exception as error:
                app.record["stop_error_type"] = type(error).__name__
        report["comparison"] = summarize(report["slots"], cases)
        report["explicit_actions_attempted"] = sum(slot["review_post_count"] for slot in report["slots"])
        report["all_slots_execution_success"] = all(slot.get("execution_success", False) for slot in report["slots"])
        report["all_main_processes_exited_cleanly"] = all(app.record.get("clean_exit") for app in apps)
        report["completed"] = report["completed"] and report["all_main_processes_exited_cleanly"]
        report["finished_at"] = utc()
        write_json(output / "report.json", report)
        print(json.dumps({"completed": report["completed"], "explicit_actions_attempted": report["explicit_actions_attempted"],
                          "report": str(output / "report.json"), "error_code": report.get("error_code"),
                          "all_slots_execution_success": report["all_slots_execution_success"],
                          "mechanical_summary": report["comparison"]["arms"],
                          "semantic_review": "PENDING_HUMAN_REVIEW"}))
    return 0 if report["completed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-root", "baseline-executable", "candidate-executable", "ui", "cases", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args()
    try:
        return run(args)
    except PairFailure as error:
        print(json.dumps({"completed": False, "error_code": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
