import json
from pathlib import Path
import pytest
from ai_tutor.context import build_context
from ai_tutor.store import Store
from ai_tutor.kimi import build_prompt, validate_windows_command_budget

ROOT = Path(__file__).resolve().parents[1]


def fixture(tmp_path, text="我想让小车停车"):
    store = Store(tmp_path/"context.db")
    learner = store.create_learner("Anonymous")
    session = store.create_session(learner["learner_id"])
    eid = store.event(learner["learner_id"], session["session_id"], "learner_report", {"text": text})
    return store, learner, session, eid


def test_long_chinese_context_preserves_current_facts_and_whole_record_selection(tmp_path):
    text = "这次我要观察停车距离。" * 300
    store, learner, session, eid = fixture(tmp_path, text)
    sid = session["session_id"]
    for index in range(12):
        old_text = f"第{index}轮观察：" + "保持墙的位置不变，只调整停车距离。" * 150
        event = store.event(learner["learner_id"], sid, "learner_report", {"text": old_text})
        store.add_turn(sid, str(index), 0, "user", old_text, "shown", [event])
        store.add_turn(sid, str(index), 0, "assistant", "观察这次变化。", "shown", [event])
    store.add_turn(sid, "current", 0, "user", text, "shown", [eid])
    state = session["state"]
    state.update(goal="墙前停车亮绿灯", reported_result="据报告成功但间隙太小", pending_step="移动墙再测试",
                 modules=["control", "motor", "distance", "light"], modules_source="learner_report",
                 session_preferences={"help_style": "one_step", "explicit_request": "这次一次一步"})
    store.checkpoint(sid, state)
    context = build_context(store, store.session(sid), learner, text, eid, {})
    assert context["user_input"] == text
    assert context["task"]["goal"] == state["goal"]
    assert context["task"]["pending_step"] == state["pending_step"]
    assert context["task"]["reported_result"] == state["reported_result"]
    assert context["task"]["session_preferences"] == state["session_preferences"]
    assert len(json.dumps(context, ensure_ascii=False)) <= 18000
    assert context["context_selection"]["omitted_history"] > 0
    assert json.dumps(context, ensure_ascii=False).count(text) == 1
    ids = [event["event_id"] for event in context["evidence"]]
    assert len(ids) == len(set(ids))
    assert set(ids) == {event["event_id"] for event in store.source_events(learner["learner_id"], ids)}
    store.close()


@pytest.mark.parametrize("text", ["测距\"引号😀"*500, '"'*4000, "😀"*4000], ids=["mixed", "quotes", "emoji"])
def test_complete_serialization_fits_windows_with_long_quotes_and_emoji(tmp_path, text):
    store, learner, session, eid = fixture(tmp_path, text)
    context = build_context(store, session, learner, text, eid, {})
    schema = json.loads((ROOT/"implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json").read_text(encoding="utf-8"))
    prompt = build_prompt((ROOT/"prompts/tutor_core.md").read_text(encoding="utf-8"), schema,
        {"request_id": "request_" + "x"*32, "learner_id": learner["learner_id"], "session_id": session["session_id"], "generation": 1, "context": context})
    validate_windows_command_budget([r"C:\Users\RBMS\.kimi-code\bin\kimi.exe", "--agent-file", str(ROOT/"prompts/tutor-agent.md"),
                                    "--skills-dir", str(ROOT/"runtime/kimi/run-"/"home/empty-skills"), "--output-format", "stream-json", "-p", prompt])
    assert context["user_input"] == text
    store.close()


def test_only_delivered_history_and_same_learner_source_rows_are_used(tmp_path):
    store, learner, session, eid = fixture(tmp_path)
    sid = session["session_id"]
    store.add_turn(sid, "hidden", 0, "assistant", "UNDELIVERED_SECRET", "generated", [])
    store.add_turn(sid, "interrupted", 0, "assistant", "Already displayed but interrupted", "speech_interrupted", [])
    other = store.create_learner("Other")
    other_session = store.create_session(other["learner_id"])
    foreign = store.event(other["learner_id"], other_session["session_id"], "learner_report", {"text": "OTHER_LEARNER_SECRET"})
    context = build_context(store, store.session(sid), learner, "我想让小车停车", eid, {})
    encoded = json.dumps(context, ensure_ascii=False)
    assert "UNDELIVERED_SECRET" not in encoded
    assert "OTHER_LEARNER_SECRET" not in encoded
    assert "Already displayed but interrupted" in encoded
    with pytest.raises(ValueError):
        build_context(store, store.session(sid), learner, "foreign", foreign, {})
    with pytest.raises(ValueError, match="不一致"):
        build_context(store, store.session(sid), learner, "Different text cannot reuse that source", eid, {})
    store.close()


def test_memory_provenance_is_available_without_repeating_original_full_input(tmp_path):
    store, learner, session, eid = fixture(tmp_path, "上次停车距离有进展" + "详细观察"*800)
    store.add_memory(learner["learner_id"], {"kind": "reported_progress", "content": "已报告停车成功，下次移动墙",
        "scope": "current_activity", "source_event_ids": [eid]}, 0)
    new_session = store.create_session(learner["learner_id"])
    current = "继续停车距离活动"
    new_eid = store.event(learner["learner_id"], new_session["session_id"], "learner_report", {"text": current})
    context = build_context(store, new_session, learner, current, new_eid, {})
    assert context["slow_memory"][0]["historical"] is True
    assert context["slow_memory"][0]["source_event_ids"] == [eid]
    evidence = {event["event_id"]: event for event in context["evidence"]}
    assert evidence[eid]["historical"] is True
    assert evidence[eid]["content_reference"]["verbatim"] is False
    assert "详细观察" not in json.dumps(context, ensure_ascii=False)
    store.close()
