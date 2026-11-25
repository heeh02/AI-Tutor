from pathlib import Path
from ai_tutor.store import Store


def fixture(tmp_path):
    store = Store(tmp_path / "real.db")
    a, b = store.create_learner("A"), store.create_learner("B")
    session = store.create_session(a["learner_id"])
    return store, a["learner_id"], b["learner_id"], session["session_id"]


def candidate(event, text="据学习者报告，小车能在墙前停车并亮绿灯", **extra):
    return {"kind": "reported_progress", "content": text, "scope": "current_activity", "source_event_ids": [event], **extra}


def test_memory_provenance_isolation_and_chinese_recall(tmp_path):
    store, a, b, sid = fixture(tmp_path)
    eid = store.event(a, sid, "learner_report", {"text": "小车在墙前停下亮绿灯"})
    assert store.add_memory(a, candidate(eid), 0)
    assert store.recall(a, "墙前停车")
    assert not store.recall(b, "墙前停车")
    assert not store.recall(a, "彩虹折纸")
    assert not store.add_memory(b, candidate(eid), 0)
    assert not store.add_memory(a, candidate("made-up"), 0)
    store.close()


def test_simulation_cannot_enter_real_space(tmp_path):
    store, a, b, sid = fixture(tmp_path)
    import pytest
    with pytest.raises(ValueError):
        store.event(a, sid, "simulated", {"text": "made up"})
    sim = Store(tmp_path / "sim.db", "simulator")
    sa = sim.create_learner("sim")
    ss = sim.create_session(sa["learner_id"])
    eid = sim.event(sa["learner_id"], ss["session_id"], "learner_report", {"text": "小车停车"})
    assert sim.add_memory(sa["learner_id"], candidate(eid), 0)
    assert sim.memories(sa["learner_id"])[0]["source_type"] == "simulated"
    assert not store.source_events(a, [eid])
    store.close()
    sim.close()


def test_session_preference_never_silently_becomes_persistent(tmp_path):
    store, a, _, sid = fixture(tmp_path)
    eid = store.event(a, sid, "learner_report", {"text": "这次请一次一步"})
    assert not store.add_memory(a, candidate(eid, "一次一步", kind="help_preference", scope="cross_session_explicit"), 0)
    eid2 = store.event(a, sid, "learner_report", {"text": "以后都请一次一步"})
    assert store.add_memory(a, candidate(eid2, "以后一次一步", kind="help_preference", scope="cross_session_explicit"), 0)
    store.close()


def test_deletion_invalidates_stale_jobs_and_all_derived_context(tmp_path):
    store, a, _, sid = fixture(tmp_path)
    eid = store.event(a, sid, "learner_report", {"text": "机密待删进展"})
    mid = store.add_memory(a, candidate(eid, "机密待删进展"), 0)
    state = store.session(sid)["state"]
    state["reported_result"] = "机密待删进展"
    store.checkpoint(sid, state)
    store.add_turn(sid, "req", 0, "user", "机密待删进展", "shown", [eid])
    store.forget(a, mid)
    assert store.learner(a)["epoch"] == 1
    assert not store.memories(a)
    assert not store.source_events(a, [eid])
    assert not store.session(sid)["turns"]
    assert store.session(sid)["state"]["reported_result"] == ""
    assert not store.add_memory(a, candidate(eid), 0)
    store.close()


def test_restart_keeps_memory_not_current_hardware(tmp_path):
    path = tmp_path / "real.db"
    store, a, _, sid = fixture(tmp_path)
    eid = store.event(a, sid, "learner_report", {"text": "下次移动墙的位置再测"})
    store.add_memory(a, candidate(eid, "尚未进行：移动墙后测试停车", kind="pending_experiment"), 0)
    store.close()
    store = Store(path)
    new = store.create_session(a)
    assert new["state"]["modules"] is None
    assert store.recall(a, "移动墙测试")
    store.close()


def test_unshown_and_stale_responses_are_not_delivered(tmp_path):
    import pytest
    store, a, _, sid = fixture(tmp_path)
    tid = store.add_turn(sid, "r", 0, "assistant", "只改停车距离", "generated", [])
    assert not store.session(sid)["state"]["last_delivered_hint"]
    store.bump(sid)
    with pytest.raises(ValueError):
        store.acknowledge(sid, tid, "shown")
    assert not store.session(sid)["state"]["last_delivered_hint"]
    store.close()


def test_correction_forgets_old_content_and_retains_other_exact_evidence(tmp_path):
    store, a, _, sid = fixture(tmp_path)
    one = store.event(a, sid, "learner_report", {"text": "停车失败"})
    two = store.event(a, sid, "learner_report", {"text": "以后一次一步"})
    mid = store.add_memory(a, candidate(one, "停车失败"), 0)
    store.add_memory(a, candidate(two, "以后一次一步", kind="help_preference", scope="cross_session_explicit"), 0)
    new = store.correct_memory(a, mid, "更正：据教师确认停车成功，只是离墙太近")
    assert new
    assert all(m["content"] != "停车失败" for m in store.memories(a))
    assert store.source_events(a, [two])[0]["content"] == {"text": "以后一次一步"}
    store.close()
