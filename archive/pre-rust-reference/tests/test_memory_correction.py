from ai_tutor.store import Store


def candidate(content, sources, kind="reported_progress"):
    return {"kind": kind, "content": content, "scope": "current_activity", "source_event_ids": sources}


def test_linked_explicit_correction_supersedes_only_affected_record(tmp_path):
    store = Store(tmp_path/"correction.db")
    learner = store.create_learner("A")
    lid = learner["learner_id"]
    sid = store.create_session(lid)["session_id"]
    original = store.event(lid, sid, "learner_report", {"text": "我报告停车成功"})
    old_mid = store.add_memory(lid, candidate("据报告停车成功", [original]), 0)
    unrelated = store.event(lid, sid, "learner_report", {"text": "我还想测试移动墙"})
    keep_mid = store.add_memory(lid, candidate("下次测试移动墙", [unrelated], "pending_experiment"), 0)
    correction = store.event(lid, sid, "learner_report", {"text": "刚才说错了，第一次没有停下来，绿灯也没亮"})
    new_mid = store.add_memory(lid, candidate("学习者更正：第一次尚未停车成功", [original, correction], "correction"), 0)
    assert new_mid
    active = {memory["memory_id"]: memory for memory in store.memories(lid)}
    assert old_mid not in active
    assert keep_mid in active
    assert active[new_mid]["supersedes"] == [old_mid]
    assert active[new_mid]["source_type"] == "learner_report"
    assert store.db.execute("SELECT status FROM memories WHERE memory_id=?", (old_mid,)).fetchone()[0] == "superseded"
    assert old_mid not in {memory["memory_id"] for memory in store.recall(lid, "停车")}
    store.close()


def test_unlinked_or_nonexplicit_correction_is_rejected(tmp_path):
    store = Store(tmp_path/"correction.db")
    learner = store.create_learner("A")
    lid = learner["learner_id"]
    sid = store.create_session(lid)["session_id"]
    old = store.event(lid, sid, "learner_report", {"text": "停车成功"})
    old_mid = store.add_memory(lid, candidate("停车成功", [old]), 0)
    ordinary = store.event(lid, sid, "learner_report", {"text": "下一步是什么"})
    assert store.add_memory(lid, candidate("不能凭空更改", [old, ordinary], "correction"), 0) is None
    explicit = store.event(lid, sid, "learner_report", {"text": "刚才说错了，没有停车成功"})
    assert store.add_memory(lid, candidate("缺少被更正原记录", [explicit], "correction"), 0) is None
    assert [memory["memory_id"] for memory in store.memories(lid)] == [old_mid]
    # A historical correction cannot be replayed after newer evidence arrives.
    store.event(lid, sid, "learner_report", {"text": "现在重新测试，已经成功"})
    assert store.add_memory(lid, candidate("旧更正不得覆盖新报告", [old, explicit], "correction"), 0) is None
    store.close()
