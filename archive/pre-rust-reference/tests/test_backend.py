"""Local integration checks. Device/model stubs here are NOT hardware evidence."""
import asyncio
import json
from unittest.mock import patch
import pytest
from fastapi.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

from ai_tutor.main import Workstation, create_app
from ai_tutor.store import Store


class CameraStub:
    def status(self):
        return {"selected_track_key": None, "selected_status": "unselected"}
    def close(self):
        pass


class AudioStub:
    stream = None
    async def probe(self):
        return self.status()
    def status(self):
        return {"state": "idle", "recording": False}
    async def stop_speech(self):
        pass
    def discard_recording(self):
        pass
    async def close(self):
        pass


class ProviderStub:
    def __init__(self):
        self.requests = []
        self.wait = None
        self.invalid = False
        self.ignore_cancel = False
    async def probe(self):
        return self.status()
    def status(self):
        return {"available": True, "testing_stub": True}
    async def cancel(self, rid):
        pass
    async def cleanup_owned(self, learner_id):
        pass
    async def close(self):
        pass
    async def generate(self, request):
        self.requests.append(request)
        if self.wait:
            try:
                await self.wait.wait()
            except asyncio.CancelledError:
                if not self.ignore_cancel:
                    raise
        eid = request["context"]["current_user_event_id"]
        if self.invalid:
            return {"private_prompt": "CANARY_MUST_NOT_REACH_DIAGNOSTICS"}
        return {"schema_version": "1.0", "request_id": request["request_id"],
            "reply_text": "A generated teaching hint", "teaching_move": "give_hint",
            "task_update_candidates": [{"field": "goal", "value": "停车亮灯", "source_event_ids": [eid]}],
            "memory_candidates": [{"kind": "reported_progress", "content": "已报告停车亮灯", "scope": "current_activity", "source_event_ids": [eid]}],
            "used_evidence_ids": [eid], "needs_clarification": False}


def make_ws(tmp_path):
    provider = ProviderStub()
    with patch("ai_tutor.main.CameraService", return_value=CameraStub()), \
         patch("ai_tutor.main.AudioService", return_value=AudioStub()), \
         patch("ai_tutor.main.KimiProvider", return_value=provider):
        return Workstation(tmp_path)


async def generated(ws, sid, text="能停车亮灯"):
    ws.last_submit_at = float("-inf")  # Other tests exercise state, not the rate limiter.
    result = await ws.submit(sid, text)
    await ws.tasks[result["request_id"]][0]
    return result


def test_candidate_updates_require_display_ack_and_not_delivered_history(tmp_path):
    async def run():
        ws = make_ws(tmp_path)
        a = ws.stores["real"].create_learner("test-A")
        session = await ws.select_session(a["learner_id"], True)
        sid = session["session_id"]
        await generated(ws, sid)
        state = ws.stores["real"].session(sid)
        assert state["state"]["goal"] == ""
        assert state["state"]["last_delivered_hint"] == ""
        assert ws.stores["real"].memories(a["learner_id"]) == []
        old_tid = state["turns"][-1]["turn_id"]
        await generated(ws, sid, "这次再观察")
        assert all(t["text"] != "A generated teaching hint" for t in ws.provider.requests[-1]["context"]["recent_delivered_dialogue"])
        with pytest.raises(ValueError):
            await ws.deliver(sid, old_tid)
        tid = ws.stores["real"].session(sid)["turns"][-1]["turn_id"]
        await ws.deliver(sid, tid)
        await ws.deliver(sid, tid)  # Idempotent browser poll.
        state = ws.stores["real"].session(sid)
        assert state["state"]["goal"] == "停车亮灯"
        assert state["state"]["last_delivered_hint"] == "A generated teaching hint"
        assert len(ws.stores["real"].memories(a["learner_id"])) == 1
        await ws.close()
    asyncio.run(run())


def test_request_rate_limit_does_not_create_second_remote_request(tmp_path):
    async def run():
        ws = make_ws(tmp_path)
        a = ws.stores["real"].create_learner("test-A")
        session = await ws.select_session(a["learner_id"], True)
        await generated(ws, session["session_id"])
        with pytest.raises(ValueError, match="频繁"):
            await ws.submit(session["session_id"], "too soon")
        assert len(ws.provider.requests) == 1
        await ws.close()
    asyncio.run(run())


def test_switch_discards_even_provider_that_ignores_cancellation(tmp_path):
    async def run():
        ws = make_ws(tmp_path)
        store = ws.stores["real"]
        a, b = store.create_learner("test-A"), store.create_learner("test-B")
        s = await ws.select_session(a["learner_id"], True)
        ws.provider.wait = asyncio.Event()
        ws.provider.ignore_cancel = True
        r = await ws.submit(s["session_id"], "只给 A 的信息")
        task = ws.tasks[r["request_id"]][0]
        await asyncio.sleep(0)
        bs = await ws.select_session(b["learner_id"], True)
        await task
        assert store.session(bs["session_id"])["turns"] == []
        assert store.memories(b["learner_id"]) == []
        assert not ws.pending
        assert not [t for t in store.session(s["session_id"])["turns"] if t["role"] == "assistant"]
        await ws.close()
    asyncio.run(run())


def test_validation_error_does_not_expose_model_content(tmp_path):
    async def run():
        ws = make_ws(tmp_path)
        a = ws.stores["real"].create_learner("test-A")
        s = await ws.select_session(a["learner_id"], True)
        ws.provider.invalid = True
        await generated(ws, s["session_id"])
        assert "CANARY_MUST_NOT_REACH_DIAGNOSTICS" not in json.dumps(ws.state())
        assert ws.metrics["failed"] == 1
        await ws.close()
    asyncio.run(run())


def memory_candidate(eid, content="据报告停车成功", kind="reported_progress", scope="current_activity"):
    return {"kind": kind, "content": content, "scope": scope, "source_event_ids": [eid]}


def test_source_owner_preference_scope_and_simulation_isolation(tmp_path):
    real, sim = Store(tmp_path/"real.db"), Store(tmp_path/"sim.db", "simulator")
    a, b = real.create_learner("A"), real.create_learner("B")
    sid = real.create_session(a["learner_id"])["session_id"]
    eid = real.event(a["learner_id"], sid, "learner_report", {"text": "这次一步一步"})
    assert real.add_memory(b["learner_id"], memory_candidate(eid), 0) is None
    assert real.add_memory(a["learner_id"], memory_candidate(eid, "一次一步", "help_preference", "cross_session_explicit"), 0) is None
    assert real.add_memory(a["learner_id"], memory_candidate(eid, "一次一步", "help_preference", "current_session"), 0) is None
    e2 = real.event(a["learner_id"], sid, "learner_report", {"text": "以后都一次一步"})
    assert real.add_memory(a["learner_id"], memory_candidate(e2, "一次一步", "help_preference", "cross_session_explicit"), 0)
    with pytest.raises(ValueError):
        real.event(a["learner_id"], sid, "simulated", {"modules": ["distance"]})
    sl = sim.create_learner("S")
    ss = sim.create_session(sl["learner_id"])["session_id"]
    se = sim.event(sl["learner_id"], ss, "operator_confirmed", {"report": "模拟成功"})
    sim.add_memory(sl["learner_id"], memory_candidate(se, "模拟成功"), 0)
    assert sim.memories(sl["learner_id"])[0]["source_type"] == "simulated"
    assert real.memories(b["learner_id"]) == []
    real.close(); sim.close()


def test_forget_invalidates_old_jobs_clears_derivatives_and_preserves_unrelated_sources(tmp_path):
    store = Store(tmp_path/"data.db")
    a = store.create_learner("A")
    sid = store.create_session(a["learner_id"])["session_id"]
    e1 = store.event(a["learner_id"], sid, "learner_report", {"text": "停车成功"})
    e2 = store.event(a["learner_id"], sid, "operator_confirmed", {"text": "灯模块说明"})
    m1 = store.add_memory(a["learner_id"], memory_candidate(e1, "停车成功"), 0)
    store.add_memory(a["learner_id"], memory_candidate(e2, "灯模块说明"), 0)
    store.add_turn(sid, "old", 0, "user", "停车成功", "shown", [e1])
    state = store.session(sid)["state"]
    state["reported_result"] = "停车成功"
    store.checkpoint(sid, state)
    epoch = store.forget(a["learner_id"], m1)
    assert epoch == 1
    assert store.add_memory(a["learner_id"], memory_candidate(e1, "停车成功"), 0) is None
    assert store.source_events(a["learner_id"], [e1]) == []
    assert store.source_events(a["learner_id"], [e2])[0]["source_type"] == "operator_confirmed"
    assert [m["content"] for m in store.memories(a["learner_id"])] == ["灯模块说明"]
    assert store.session(sid)["turns"] == []
    assert store.session(sid)["state"]["reported_result"] == ""
    store.close()


def test_restart_recalls_chinese_without_restoring_current_hardware(tmp_path):
    async def run():
        ws = make_ws(tmp_path)
        store = ws.stores["real"]
        a = store.create_learner("A")
        s = await ws.select_session(a["learner_id"], True)
        eid = store.event(a["learner_id"], s["session_id"], "learner_report", {"text": "下次移动墙再试停车距离"})
        store.add_memory(a["learner_id"], memory_candidate(eid, "下次移动墙再试停车距离", "pending_experiment"), 0)
        st = store.session(s["session_id"])["state"]
        st["modules"] = ["control", "distance"]
        st["modules_source"] = "learner_report"
        store.checkpoint(s["session_id"], st)
        await ws.close()
        restarted = make_ws(tmp_path)
        restored = await restarted.select_session(a["learner_id"], True)
        assert restored["state"]["modules"] is None
        assert restored["state"]["requires_inventory_confirmation"] is True
        assert restored["recalled_memories"][0]["kind"] == "pending_experiment"
        assert restarted.stores["real"].recall(a["learner_id"], "完全无关的绘画问题") == []
        await restarted.close()
    asyncio.run(run())


def test_http_and_websocket_auth_origin_and_size_guards(tmp_path):
    ws = make_ws(tmp_path)
    app = create_app(ws, token="local-test-token")
    with TestClient(app, base_url="http://127.0.0.1:8765") as client:
        assert client.get("/api/state").status_code == 401
        assert client.get("/api/health").status_code == 200
        assert client.get("/api/health", headers={"Origin": "https://evil.example"}).status_code == 403
        assert client.get("/api/health", headers={"Host": "evil.example:8765"}).status_code == 403
        with pytest.raises(WebSocketDisconnect):
            with client.websocket_connect("ws://127.0.0.1:8765/api/events", headers={"Origin": "https://evil.example"}):
                pass
        result = client.get("/?token=local-test-token", follow_redirects=False)
        assert result.status_code == 303
        assert "httponly" in result.headers["set-cookie"].lower()
        assert client.get("/api/state").status_code == 200
        assert client.post("/api/learners", json={"name": "A"}, headers={"Origin": "http://localhost:9999"}).status_code == 403
        assert client.post("/api/learners", content=b"x"*25000).status_code == 413
        assert client.post("/api/learners", json={"name": "A", "shell": "forbidden"}).status_code == 422
        with client.websocket_connect("ws://127.0.0.1:8765/api/events", headers={"Origin": "http://127.0.0.1:8765"}):
            pass
