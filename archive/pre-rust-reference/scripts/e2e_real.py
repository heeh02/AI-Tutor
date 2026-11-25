"""Real Kimi + live local API acceptance (typed adult/internal scenario).

No device mocks, no credentials printed, no camera interruption. Deliberately
manual module evidence; this does not claim that the toy car physically ran.
Use --resume after restarting the backend to check persisted memory separately.
"""
import argparse
import json
from pathlib import Path
import sys
import time
from datetime import datetime
from zoneinfo import ZoneInfo

import httpx

ROOT = Path(__file__).resolve().parents[1]
STATE = ROOT / "runtime/e2e_state.json"
REPORT = ROOT / "docs/E2E_REPORT.md"


class Acceptance:
    def __init__(self):
        descriptor = json.loads((ROOT / "runtime/server.json").read_text(encoding="utf-8"))
        self.client = httpx.Client(base_url=f"http://127.0.0.1:{int(descriptor['port'])}", timeout=15)
        response = self.client.get("/", params={"token": descriptor["token"]}, follow_redirects=False)
        if response.status_code != 303:
            raise RuntimeError("Local authentication bootstrap failed: " + str(response.status_code))
        self.turns = []
        self.checks = []
        self.created = {}

    def api(self, method, path, **kwargs):
        response = self.client.request(method, path, **kwargs)
        response.raise_for_status()
        return response.json()

    def state(self):
        return self.api("GET", "/api/state")

    def check(self, name, condition, detail=""):
        self.checks.append({"name": name, "status": "PASS" if condition else "FAIL", "detail": detail})
        print(("PASS " if condition else "FAIL ") + name, flush=True)

    def select(self, learner_id, resume=True):
        return self.api("POST", "/api/sessions", json={"learner_id": learner_id, "resume": resume})

    def turn(self, sid, text, label):
        started = time.monotonic()
        submitted = self.api("POST", f"/api/sessions/{sid}/turns", json={"text": text})
        rid = submitted["request_id"]
        until = time.monotonic() + 105
        while time.monotonic() < until:
            state = self.state()
            session = state["session"]
            if not session or session["session_id"] != sid:
                raise RuntimeError("Acceptance session switched unexpectedly")
            matches = [t for t in session["turns"] if t["request_id"] == rid and t["role"] == "assistant"]
            if matches:
                reply = matches[-1]
                self.api("POST", f"/api/sessions/{sid}/delivery", json={"turn_id": reply["turn_id"], "status": "shown"})
                record = {"label": label, "input": text, "reply": reply["content"],
                          "latency_ms": round((time.monotonic()-started)*1000), "request_id": rid}
                self.turns.append(record)
                print(label + ": " + reply["content"], flush=True)
                return self.state()["session"]
            if session["status"] == "error":
                raise RuntimeError("Real teaching request failed: " + str(state.get("errors", [])[-1:]))
            time.sleep(.3)
        raise RuntimeError("Real teaching request timed out")

    def event(self, session, modules=None, report=None):
        return self.api("POST", f"/api/activities/{session['activity_id']}/events",
                        json={"source_type": "operator_confirmed", "modules": modules, "report": report})

    def save_report(self, suffix=""):
        timestamp = datetime.now(ZoneInfo("Asia/Shanghai")).isoformat(timespec="seconds")
        lines = ["# Real teaching API acceptance" + suffix, "", f"Run: {timestamp} (Asia/Shanghai).", "",
            "This is an internal typed acceptance scenario through the real local HTTP API and authenticated Kimi Code 2.3.1. "
            "Module inventories and trial outcomes below are synthetic scripted reports entered by the test harness through the manual-report API; no adult observation, child session, or toy-car hardware execution is claimed. "
            "The live camera service remains running independently. This script acknowledges received teaching text as shown; it does not substitute for a human voice/UI demonstration.", "",
            "| Check | Status | Evidence |", "|---|---|---|"]
        lines.extend(f"| {c['name']} | {c['status']} | {c['detail'].replace('|','/')} |" for c in self.checks)
        lines.extend(["", "## Actual dialogue (anonymous internal scenario)", ""])
        for t in self.turns:
            lines.extend([f"### {t['label']} — {t['latency_ms']} ms", "", "Input: " + t["input"], "", "Real Kimi: " + t["reply"], ""])
        if suffix and REPORT.exists():
            with REPORT.open("a", encoding="utf-8") as stream:
                stream.write("\n\n" + "\n".join(lines))
        else:
            REPORT.write_text("\n".join(lines), encoding="utf-8")

    def first_run(self):
        initial = self.state()
        self.check("Real Kimi verified and available", initial["kimi"].get("available") is True,
                   "version=" + str(initial["kimi"].get("version")))
        a = self.api("POST", "/api/learners", json={"name": "内部验收 A（脚本合成报告）", "namespace": "real"})
        b = self.api("POST", "/api/learners", json={"name": "内部验收 B（隔离测试）", "namespace": "real"})
        aid, bid = a["learner_id"], b["learner_id"]
        self.created = {"learner_a": aid, "learner_b": bid}
        session = self.select(aid, resume=False)
        sid = session["session_id"]
        self.created["session_one"] = sid
        session = self.turn(sid, "我想让小车到墙前停下来，再亮绿灯。", "1. Goal")
        self.event(session, ["control", "motor", "light"])
        session = self.turn(sid, "老师刚才手动确认我有主控、电机和灯。我还缺什么能力？请给我一个提示。", "2. Missing capability")
        self.check("Inventory keeps manual provenance", session["state"]["modules_source"] == "operator_confirmed",
                   "hardware_verified remains false")
        session = self.turn(sid, "我把测距模块接好了。这只是我的口头报告，还没有硬件验证。", "3. Learner connection report")
        session = self.turn(sid, "第一次试跑，它能停下来，也亮绿灯了，但是我觉得离墙太近，想要大一点的间隙。请记住第一次据我报告已经成功。", "4. Success with improvement")
        result = session["state"]["reported_result"]
        self.check("Reported success is not relabelled failure", bool(result) and "失败" not in result,
                   result)
        session = self.turn(sid, "这次一次只告诉我一步，我想让它更早停。", "5. Session help preference")
        memories = self.api("GET", f"/api/learners/{aid}/memories")
        self.check("Session-only preference not promoted to permanent trait",
                   not any(m["kind"] == "help_preference" and m["scope"] == "cross_session_explicit" for m in memories))
        session = self.turn(sid, "我只调了停车距离，其他条件一样。第二次据我观察提前停下，亮绿灯，离墙间隙更大了。请记住这个进展；下次我想移动墙的位置，再测试是否仍按距离停车，这个实验还没有做。", "6. Comparison and pending experiment")
        memories = self.api("GET", f"/api/learners/{aid}/memories")
        self.check("Fast memory contains actually delivered hint", session["state"]["last_delivered_hint"] == self.turns[-1]["reply"])
        self.check("Slow memory has source-linked reported progress", any(m["kind"] == "reported_progress" and m["source_event_ids"] and m["source_type"] == "learner_report" for m in memories))
        self.check("Slow memory retains unfinished wall experiment", any(m["kind"] == "pending_experiment" and "墙" in m["content"] for m in memories))
        self.api("POST", f"/api/sessions/{sid}/finish")
        session = self.select(aid, resume=False)
        sid2 = session["session_id"]
        self.created["session_two"] = sid2
        self.check("New session treats current module build as unknown", session["state"]["modules"] is None and session["state"]["requires_inventory_confirmation"])
        self.check("New session recalls source-linked previous progress", bool(session["recalled_memories"]))
        session = self.turn(sid2, "继续上次的小车停车距离活动。上次我还留下了什么没做？请先确认今天搭建。", "7. Resume from memory")
        b_session = self.select(bid, resume=False)
        self.check("Learner B has no A dialogue or slow memories", not b_session["turns"] and not b_session["recalled_memories"] and not self.api("GET", f"/api/learners/{bid}/memories"))
        a_session = self.select(aid)
        cancelled = self.api("POST", f"/api/sessions/{a_session['session_id']}/turns", json={"text": "请解释我们接下来如何只改变一个变量。"})
        b_session = self.select(bid)
        time.sleep(1.5)
        b_state = self.state()["session"]
        self.check("Switch cancels A generation and keeps B empty", b_state["learner_id"] == bid and not b_state["turns"])
        self.check("Cancelled real provider process is reclaimed", self.state()["kimi"].get("running") == 0)
        self.created["cancelled_request"] = cancelled["request_id"]
        # Correct/delete an unrelated test note on a separate profile, preserving
        # demo A's progress for the explicit process-restart continuation check.
        c = self.api("POST", "/api/learners", json={"name": "内部验收 C（纠正删除）", "namespace": "real"})
        cid = c["learner_id"]
        cs = self.select(cid, resume=False)
        self.turn(cs["session_id"], "请记住我的口头进展：小车已经停车并亮绿灯，但没有做移动墙实验。", "8. Correction fixture through real Kimi")
        cm = self.api("GET", f"/api/learners/{cid}/memories")
        self.check("Correction fixture produced genuine candidate memory", bool(cm))
        if cm:
            old_id = cm[0]["memory_id"]
            correction = self.api("PATCH", f"/api/learners/{cid}/memories/{old_id}", json={"content": "教师更正：这只是口头报告，实体小车尚未验证，移动墙实验未做。"})
            corrected = self.api("GET", f"/api/learners/{cid}/memories")
            self.check("Teacher correction replaces active note", bool(corrected) and all(m["memory_id"] != old_id for m in corrected))
            self.api("DELETE", f"/api/learners/{cid}/data")
            self.check("Delete removes memory and derived context", not self.api("GET", f"/api/learners/{cid}/memories") and not self.state()["session"]["turns"])
        self.select(aid)
        self.created["memory_snapshot"] = [{k: m[k] for k in ("memory_id", "kind", "content", "source_type", "source_event_ids")} for m in memories]
        STATE.write_text(json.dumps(self.created, ensure_ascii=False, indent=2), encoding="utf-8")
        self.save_report()

    def resume_after_restart(self):
        created = json.loads(STATE.read_text(encoding="utf-8"))
        session = self.select(created["learner_a"])
        self.check("Persisted A memories survive actual backend restart", bool(session["recalled_memories"]))
        self.check("Actual restart still requires fresh inventory", session["state"]["modules"] is None and session["state"]["modules_source"] == "unknown")
        self.turn(session["session_id"], "重新打开了。我们继续小车停车距离活动，上次的进展和未完成实验是什么？先确认今天的模块。", "Restart continuation")
        session = self.select(created["learner_b"])
        self.check("B stays empty after backend restart", not session["turns"] and not session["recalled_memories"])
        self.select(created["learner_a"])
        self.save_report(" — process restart check")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()
    acceptance = Acceptance()
    try:
        acceptance.resume_after_restart() if args.resume else acceptance.first_run()
    except Exception:
        acceptance.save_report(" — interrupted")
        raise
    finally:
        acceptance.client.close()


if __name__ == "__main__":
    main()
