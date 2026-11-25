"""Transactional, source-aware memory. Separate SQLite files isolate simulation."""
from __future__ import annotations

import json
from contextlib import contextmanager
import re
import sqlite3
import threading
import uuid
from datetime import datetime, timezone
from pathlib import Path


def uid(prefix):
    return f"{prefix}_{uuid.uuid4().hex}"


def now():
    return datetime.now(timezone.utc).isoformat()


def encode(value):
    return json.dumps(value, ensure_ascii=False)


def empty_state():
    return {"goal": "", "stage": "goal_setting", "reported_result": "", "pending_step": "",
            "modules": None, "modules_source": "unknown", "latest_input": "", "last_delivered_hint": "",
            "attempts": [], "session_preferences": {}, "field_evidence": {}, "requires_inventory_confirmation": True}


class Store:
    def __init__(self, path: Path, namespace="real"):
        self.namespace = namespace
        path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(path, check_same_thread=False)
        self.db.row_factory = sqlite3.Row
        self.lock = threading.RLock()
        self._transaction_depth = 0
        self._privacy_dirty = False
        self.db.executescript('''
        PRAGMA journal_mode=WAL;
        PRAGMA secure_delete=ON;
        CREATE TABLE IF NOT EXISTS learners(learner_id TEXT PRIMARY KEY,name TEXT,language TEXT,help_style TEXT,epoch INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS sessions(session_id TEXT PRIMARY KEY,learner_id TEXT,activity_id TEXT,generation INTEGER DEFAULT 0,status TEXT,state TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS events(event_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,source_type TEXT,content TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS turns(turn_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,request_id TEXT,generation INTEGER,role TEXT,content TEXT,status TEXT,evidence TEXT,created_at TEXT);
        CREATE TABLE IF NOT EXISTS memories(memory_id TEXT PRIMARY KEY,learner_id TEXT,activity_scope TEXT,kind TEXT,content TEXT,source_event_ids TEXT,source_type TEXT,scope TEXT,created_at TEXT,updated_at TEXT,status TEXT,supersedes TEXT,epoch INTEGER);
        CREATE TABLE IF NOT EXISTS tombstones(id TEXT PRIMARY KEY,learner_id TEXT,epoch INTEGER,created_at TEXT);
        CREATE TABLE IF NOT EXISTS provider_runs(request_id TEXT PRIMARY KEY,learner_id TEXT,session_id TEXT,status TEXT,created_at TEXT,latency_ms INTEGER,payload_chars INTEGER,reported_tokens INTEGER);
        CREATE INDEX IF NOT EXISTS memory_owner ON memories(learner_id,status);
        CREATE INDEX IF NOT EXISTS events_owner ON events(learner_id,session_id);
        ''')
        # An interrupted process must never deliver a pending generation on restart.
        self.db.execute("UPDATE turns SET status='cancelled' WHERE status IN ('generated','generating')")
        self.db.execute("UPDATE turns SET status='speech_interrupted' WHERE status='speech_started'")
        self.db.execute("UPDATE sessions SET status='idle',generation=generation+1 WHERE status='generating'")
        self.db.execute("UPDATE provider_runs SET status='interrupted' WHERE status='running'")
        self.db.commit()

    @contextmanager
    def transaction(self):
        outer = self._transaction_depth == 0
        self._transaction_depth += 1
        try:
            yield
            if outer:
                self.db.commit()
        except BaseException:
            if outer:
                self.db.rollback()
            raise
        finally:
            self._transaction_depth -= 1
            if outer and self._privacy_dirty:
                self.db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
                self._privacy_dirty = False

    def start_run(self, rid, learner_id, sid, chars):
        with self.lock, self.transaction():
            self.db.execute("INSERT INTO provider_runs VALUES(?,?,?,'running',?,NULL,?,NULL)", (rid, learner_id, sid, now(), chars))

    def end_run(self, rid, status, latency_ms):
        with self.lock, self.transaction():
            self.db.execute("UPDATE provider_runs SET status=?,latency_ms=? WHERE request_id=?", (status, latency_ms, rid))

    def run_metrics(self):
        with self.lock:
            rows = self.db.execute("SELECT status,COUNT(*) FROM provider_runs GROUP BY status").fetchall()
            return {r[0]: r[1] for r in rows}

    def _one(self, query, args=()):
        row = self.db.execute(query, args).fetchone()
        return dict(row) if row else None

    def learners(self):
        with self.lock:
            return [dict(r, namespace=self.namespace) for r in self.db.execute("SELECT learner_id,name,language,help_style FROM learners ORDER BY rowid")]

    def learner(self, learner_id):
        with self.lock:
            row = self._one("SELECT * FROM learners WHERE learner_id=?", (learner_id,))
            if not row:
                raise KeyError("学习者档案不存在")
            return dict(row, namespace=self.namespace)

    def create_learner(self, name, language="zh", help_style="one_step"):
        with self.lock, self.transaction():
            learner_id = uid("sim" if self.namespace == "simulator" else "learner")
            self.db.execute("INSERT INTO learners(learner_id,name,language,help_style) VALUES(?,?,?,?)", (learner_id, name, language, help_style))
        return self.learner(learner_id)

    def create_session(self, learner_id):
        self.learner(learner_id)
        with self.lock, self.transaction():
            sid = uid("session")
            self.db.execute("INSERT INTO sessions VALUES(?,?,?,0,'idle',?,?)", (sid, learner_id, uid("activity"), encode(empty_state()), now()))
            return self.session(sid)

    def latest_session(self, learner_id):
        with self.lock:
            row = self._one("SELECT session_id FROM sessions WHERE learner_id=? AND status!='finished' ORDER BY rowid DESC LIMIT 1", (learner_id,))
            return self.session(row["session_id"]) if row else None

    def session(self, sid):
        with self.lock:
            result = self._one("SELECT * FROM sessions WHERE session_id=?", (sid,))
            if not result:
                raise KeyError("会话不存在")
            result["state"] = json.loads(result["state"])
            result["namespace"] = self.namespace
            turns = self.db.execute("SELECT * FROM turns WHERE session_id=? ORDER BY rowid DESC LIMIT 24", (sid,)).fetchall()
            result["turns"] = [dict(t, evidence=json.loads(t["evidence"])) for t in reversed(turns)]
            result["recalled_memories"] = self.recall(result["learner_id"], result["state"]["goal"] or "小车 停车 墙 距离 灯", 5)
            return result

    def checkpoint(self, sid, state):
        with self.lock, self.transaction():
            self.db.execute("UPDATE sessions SET state=? WHERE session_id=?", (encode(state), sid))

    def bump(self, sid, status="idle"):
        with self.lock, self.transaction():
            self.db.execute("UPDATE sessions SET generation=generation+1,status=? WHERE session_id=?", (status, sid))
            self.db.execute("UPDATE turns SET status='cancelled' WHERE session_id=? AND status IN ('generated','generating')", (sid,))
            self.db.execute("UPDATE turns SET status='speech_interrupted' WHERE session_id=? AND status='speech_started'", (sid,))
            return self.session(sid)["generation"]

    def event(self, learner_id, sid, source_type, content):
        with self.lock, self.transaction():
            eid = uid("event")
            if self.namespace == "simulator":
                source_type = "simulated"
            elif source_type == "simulated":
                raise ValueError("模拟事件只能写入模拟档案")
            self.db.execute("INSERT INTO events VALUES(?,?,?,?,?,?)", (eid, learner_id, sid, source_type, encode(content), now()))
            return eid

    def source_events(self, learner_id, ids):
        if not ids or len(ids) > 20:
            return []
        with self.lock:
            rows = [self._one("SELECT * FROM events WHERE learner_id=? AND event_id=?", (learner_id, e)) for e in set(ids)]
            return [dict(r, content=json.loads(r["content"])) for r in rows if r]

    def recent_evidence(self, sid):
        with self.lock:
            rows = self.db.execute("SELECT * FROM events WHERE session_id=? ORDER BY rowid DESC LIMIT 12", (sid,)).fetchall()
            return [dict(r, content=json.loads(r["content"])) for r in reversed(rows)]

    def add_turn(self, sid, request_id, generation, role, content, status, evidence):
        with self.lock, self.transaction():
            s = self.session(sid)
            tid = uid("turn")
            self.db.execute("INSERT INTO turns VALUES(?,?,?,?,?,?,?,?,?,?)", (tid, s["learner_id"], sid, request_id, generation, role, content, status, encode(evidence), now()))
            return tid

    def acknowledge(self, sid, tid, status):
        with self.lock, self.transaction():
            s = self.session(sid)
            t = self._one("SELECT * FROM turns WHERE session_id=? AND turn_id=?", (sid, tid))
            if not t or t["generation"] != s["generation"] or t["role"] != "assistant" or t["status"] == "cancelled":
                raise ValueError("回复已过期或不存在")
            allowed = {"shown", "speech_started", "speech_completed", "speech_interrupted", "cancelled"}
            if status not in allowed:
                raise ValueError("无效投递状态")
            # A polling UI must not move a completed speech backwards to shown.
            if status == "shown" and t["status"] in {"shown", "speech_started", "speech_completed", "speech_interrupted"}:
                return
            self.db.execute("UPDATE turns SET status=? WHERE turn_id=?", (status, tid))
            if status != "cancelled":
                st = s["state"]
                st["last_delivered_hint"] = t["content"]
                self.checkpoint(sid, st)

    def add_memory(self, learner_id, candidate, epoch):
        with self.lock, self.transaction():
            if self.learner(learner_id)["epoch"] != epoch:
                return None
            sources = self.source_events(learner_id, candidate["source_event_ids"])
            if len(sources) != len(set(candidate["source_event_ids"])):
                return None
            if any(s["source_type"] not in {"learner_report", "operator_confirmed", "simulated"} for s in sources):
                return None
            if self.namespace == "real" and any(s["source_type"] == "simulated" for s in sources):
                return None
            if candidate["scope"] == "current_session":
                return None
            raw = encode([s["content"] for s in sources])
            if candidate["kind"] == "help_preference":
                if candidate["scope"] != "cross_session_explicit" or not (re.search(r"以后|每次|一直|今后|always|from now|every time", raw, re.I) or any(s["source_type"] == "operator_confirmed" and s["content"].get("set_preference") is True for s in sources)):
                    return None
            source_type = "simulated" if self.namespace == "simulator" else ("learner_report" if any(s["source_type"] == "learner_report" for s in sources) else "operator_confirmed")
            existing = self._one("SELECT memory_id FROM memories WHERE learner_id=? AND content=? AND status='active'", (learner_id, candidate["content"]))
            if existing:
                return existing["memory_id"]
            supersedes = []
            if candidate["kind"] == "correction":
                ids = list(dict.fromkeys(candidate["source_event_ids"]))
                placeholders = ",".join("?" for _ in ids)
                ordered = self.db.execute(f"SELECT rowid AS source_order,* FROM events WHERE learner_id=? AND event_id IN ({placeholders})", (learner_id, *ids)).fetchall()
                if len(ordered) < 2:
                    return None
                current = max(ordered, key=lambda row: row["source_order"])
                current_content = json.loads(current["content"])
                raw_correction = str(current_content.get("text", "")) + " " + str(current_content.get("report", ""))
                explicit = (current["source_type"] == "operator_confirmed" and bool(current_content.get("correction"))) or bool(re.search(
                    r"刚才.{0,12}说错|我.{0,8}说错|更正[:：]|纠正[:：]|更正一下|纠正一下|correction\s*:|i was wrong|i misspoke|that was incorrect", raw_correction, re.I))
                latest = self._one("SELECT event_id FROM events WHERE learner_id=? AND session_id=? ORDER BY rowid DESC LIMIT 1", (learner_id, current["session_id"]))
                if not explicit or not latest or latest["event_id"] != current["event_id"]:
                    return None
                older_ids = {row["event_id"] for row in ordered if row["source_order"] < current["source_order"]}
                previous = self.db.execute("SELECT memory_id,source_event_ids FROM memories WHERE learner_id=? AND activity_scope='modular_rover' AND status='active'", (learner_id,)).fetchall()
                supersedes = [row["memory_id"] for row in previous if older_ids & set(json.loads(row["source_event_ids"]))]
                if not supersedes:
                    # An unlinked correction cannot silently coexist with a contradictory
                    # active note; ask which original record the learner means instead.
                    return None
                for old_id in supersedes:
                    self.db.execute("UPDATE memories SET status='superseded' WHERE learner_id=? AND memory_id=?", (learner_id, old_id))
            elif candidate["kind"] == "help_preference":
                old = self.db.execute("SELECT memory_id FROM memories WHERE learner_id=? AND status='active' AND kind=?", (learner_id, candidate["kind"])).fetchall()
                supersedes = [r[0] for r in old]
                self.db.execute("UPDATE memories SET status='superseded' WHERE learner_id=? AND status='active' AND kind=?", (learner_id, candidate["kind"]))
            mid, timestamp = uid("memory"), now()
            self.db.execute("INSERT INTO memories VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", (mid, learner_id, "modular_rover", candidate["kind"], candidate["content"], encode(candidate["source_event_ids"]), source_type, candidate["scope"], timestamp, timestamp, "active", encode(supersedes), epoch))
            return mid

    def memories(self, learner_id):
        self.learner(learner_id)
        with self.lock:
            rows = self.db.execute("SELECT * FROM memories WHERE learner_id=? AND status='active' ORDER BY rowid DESC LIMIT 100", (learner_id,)).fetchall()
            return [dict(r, source_event_ids=json.loads(r["source_event_ids"]), supersedes=json.loads(r["supersedes"])) for r in rows]

    def recall(self, learner_id, query, top_k=5):
        def grams(text):
            text = re.sub(r"\s+", "", text.lower())
            return set(text[i:i+2] for i in range(max(0, len(text)-1)))
        q = grams(query)
        scored = []
        for m in self.memories(learner_id):
            overlap = len(q & grams(m["content"]))
            if m["kind"] == "help_preference" or overlap:
                scored.append((overlap + (2 if m["kind"] == "pending_experiment" else 0), m["updated_at"], m))
        scored.sort(key=lambda x: (x[0], x[1]), reverse=True)
        result, budget = [], 2400
        for _, _, m in scored[:top_k]:
            if len(m["content"]) <= budget:
                result.append(m)
                budget -= len(m["content"])
        return result

    def correct_memory(self, learner_id, mid, content):
        with self.lock, self.transaction():
            original = self._one("SELECT * FROM memories WHERE learner_id=? AND memory_id=? AND status='active'", (learner_id, mid))
            if not original:
                raise KeyError("记忆不存在")
            # Erase the old provenance and all derivatives before authoring a replacement.
            self.forget(learner_id, mid)
            latest = self.latest_session(learner_id)
            sid = (latest or self.create_session(learner_id))["session_id"]
            eid = self.event(learner_id, sid, "operator_confirmed", {"correction": content, "set_preference": original["kind"] == "help_preference"})
            new = self.add_memory(learner_id, {"kind": original["kind"], "content": content, "scope": original["scope"], "source_event_ids": [eid]}, self.learner(learner_id)["epoch"])
            self.db.execute("UPDATE memories SET supersedes=? WHERE memory_id=?", (encode([mid]), new))
            return new

    def forget(self, learner_id, mid=None):
        with self.lock, self.transaction():
            learner = self.learner(learner_id)
            epoch = learner["epoch"] + 1
            self.db.execute("UPDATE learners SET epoch=? WHERE learner_id=?", (epoch, learner_id))
            self._privacy_dirty = True
            self.db.execute("INSERT INTO tombstones VALUES(?,?,?,?)", (uid("deleted"), learner_id, epoch, now()))
            sources = set()
            if mid:
                memory = self._one("SELECT * FROM memories WHERE learner_id=? AND memory_id=?", (learner_id, mid))
                if not memory:
                    raise KeyError("记忆不存在")
                sources.update(json.loads(memory["source_event_ids"]))
                all_notes = self.db.execute("SELECT * FROM memories WHERE learner_id=?", (learner_id,)).fetchall()
                changed = True
                while changed:
                    changed = False
                    for m in all_notes:
                        ids = set(json.loads(m["source_event_ids"]))
                        if ids & sources and not ids.issubset(sources):
                            sources |= ids
                            changed = True
                for m in all_notes:
                    if m["memory_id"] == mid or sources & set(json.loads(m["source_event_ids"])):
                        self.db.execute("DELETE FROM memories WHERE memory_id=?", (m["memory_id"],))
                for eid in sources:
                    self.db.execute("DELETE FROM events WHERE learner_id=? AND event_id=?", (learner_id, eid))
            else:
                self.db.execute("DELETE FROM memories WHERE learner_id=?", (learner_id,))
                self.db.execute("DELETE FROM events WHERE learner_id=?", (learner_id,))
            self.db.execute("DELETE FROM provider_runs WHERE learner_id=?", (learner_id,))
            # Every derived dialogue/checkpoint may contain paraphrases. Invalidate all of
            # this learner's transient context rather than permit a deleted fact to reappear.
            retained = []
            for m in self.db.execute("SELECT * FROM memories WHERE learner_id=?", (learner_id,)).fetchall():
                for eid in json.loads(m["source_event_ids"]):
                    original = self._one("SELECT * FROM events WHERE learner_id=? AND event_id=?", (learner_id, eid))
                    if original:
                        retained.append(original)
            self.db.execute("DELETE FROM turns WHERE learner_id=?", (learner_id,))
            self.db.execute("DELETE FROM events WHERE learner_id=?", (learner_id,))
            self.db.execute("UPDATE sessions SET state=?,generation=generation+1,status='idle' WHERE learner_id=?", (encode(empty_state()), learner_id))
            # Preserve exact original evidence for unrelated notes, never replace it
            # with a model summary disguised as a learner's own report.
            for event in retained:
                self.db.execute("INSERT OR IGNORE INTO events VALUES(?,?,?,?,?,?)", tuple(event[k] for k in ("event_id", "learner_id", "session_id", "source_type", "content", "created_at")))
            return epoch

    def settings(self, learner_id, language, help_style):
        with self.lock, self.transaction():
            self.db.execute("UPDATE learners SET language=?,help_style=? WHERE learner_id=?", (language, help_style, learner_id))

    def close(self):
        with self.lock:
            self.db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            self.db.close()
