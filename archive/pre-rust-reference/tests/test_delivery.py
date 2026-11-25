from ai_tutor.store import Store


def test_interrupted_speech_keeps_shown_text_and_last_hint(tmp_path):
    store = Store(tmp_path / "test.sqlite3")
    learner = store.create_learner("A")
    session = store.create_session(learner["learner_id"])
    sid = session["session_id"]
    tid = store.add_turn(sid, "request", 0, "assistant", "只调整停车距离", "generated", [])
    store.acknowledge(sid, tid, "shown")
    store.acknowledge(sid, tid, "speech_started")
    store.bump(sid)
    restored = store.session(sid)
    assert restored["turns"][-1]["status"] == "speech_interrupted"
    assert restored["state"]["last_delivered_hint"] == "只调整停车距离"
    store.close()
