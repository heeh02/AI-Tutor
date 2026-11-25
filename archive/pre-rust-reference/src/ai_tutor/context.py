"""Bounded teaching context: select whole records, preserve current facts.

Source references always resolve to real same-learner event rows. Referenced
memory is explicitly a historical summary, never a verbatim sensor observation.
"""
from __future__ import annotations

import copy
import json
import subprocess

MAX_CONTEXT_CHARS = 18000
MAX_CONTEXT_COMMAND_UNITS = 21000  # Reserve 9,000 for policy/schema/paths/identifiers.
DELIVERED = {"shown", "speech_started", "speech_completed", "speech_interrupted"}


def _encoded(context):
    return json.dumps(context, ensure_ascii=False)


def _fits(context):
    value = _encoded(context)
    command_units = len(subprocess.list2cmdline([value]).encode("utf-16-le")) // 2
    return len(value) <= MAX_CONTEXT_CHARS-256 and command_units <= MAX_CONTEXT_COMMAND_UNITS-256


def _rows(store, learner_id, event_ids):
    result = {}
    ids = list(dict.fromkeys(event_ids))
    for start in range(0, len(ids), 20):
        for row in store.source_events(learner_id, ids[start:start+20]):
            if row["learner_id"] == learner_id:
                result[row["event_id"]] = row
    return result


def _event(row, *, reference=None, historical=False):
    value = {"event_id": row["event_id"], "source_type": row["source_type"],
             "created_at": row["created_at"], "historical": historical}
    if reference:
        value["content_reference"] = reference
        value["original_content_included"] = False
    else:
        value["content"] = row["content"]
        value["original_content_included"] = True
    return value


def build_context(store, session, learner, text, eid, vision) -> dict:
    lid, sid = learner["learner_id"], session["session_id"]
    if session["learner_id"] != lid:
        raise ValueError("教学上下文的学习者归属不匹配")
    current_rows = _rows(store, lid, [eid])
    if eid not in current_rows or current_rows[eid]["session_id"] != sid:
        raise ValueError("当前输入缺少同一学习者与会话的真实来源")
    if current_rows[eid]["content"].get("text") != text:
        raise ValueError("当前输入与其原始来源不一致")
    original_state = session["state"]
    mandatory_keys = ("goal", "stage", "reported_result", "pending_step", "modules", "modules_source",
                      "requires_inventory_confirmation", "session_preferences", "last_delivered_hint")
    task = {key: copy.deepcopy(original_state[key]) for key in mandatory_keys if key in original_state}
    field_evidence = {key: copy.deepcopy(value) for key, value in original_state.get("field_evidence", {}).items()
                      if key in {"goal", "stage", "reported_result", "pending_step", "modules"}}
    task["field_evidence"] = field_evidence
    for field in ("goal", "stage", "reported_result", "pending_step"):
        if field + "_source" in original_state:
            task[field + "_source"] = original_state[field + "_source"]
    task["attempts"] = []
    preferences = task.get("session_preferences", {})
    context = {
        "language": learner["language"], "help_style": preferences.get("help_style", learner["help_style"]),
        "namespace": store.namespace, "activity": "modular_rover", "task": task,
        "user_input": text, "current_user_event_id": eid,
        "slow_memory": [], "recent_delivered_dialogue": [],
        "evidence": [_event(current_rows[eid], reference={"path": "user_input", "verbatim": True})],
        "vision": {key: vision.get(key) for key in ("selected_status", "selected_track_key", "frame_age_ms")},
        "current_hardware_status": "unknown" if task.get("modules") is None else "reported_only",
        "rules": "当前输入与结构化事实完整保留。来源引用仅代表其标注的报告或历史摘要，不是传感器确认。历史配置不能证明当前连接；建议不是完成。成功后改进仍是成功；情绪与硬件执行不可用。若更正记忆，候选必须同时引用被更正的旧来源ID与当前明确更正来源ID；无法确定旧记录时先澄清。",
        "context_selection": {"max_chars": MAX_CONTEXT_CHARS, "omitted_history": 0,
                              "omitted_evidence": 0, "omitted_memories": 0, "omitted_attempts": 0,
                              "selection": "whole_records; current_input_and_current_facts_not_truncated"}}
    evidence = {eid: context["evidence"][0]}
    required_ids = [event_id for value in field_evidence.values() for event_id in value.get("source_event_ids", [])]
    state_rows = _rows(store, lid, required_ids)
    if set(required_ids) - set(state_rows):
        raise ValueError("活动状态引用了无效或其他学习者的来源")
    for event_id, row in state_rows.items():
        if event_id not in evidence:
            fields = [field for field, value in field_evidence.items() if event_id in value.get("source_event_ids", [])]
            evidence[event_id] = _event(row, reference={"task_fields": fields, "verbatim": False,
                "meaning": "validated model-interpreted report; original event retained locally"}, historical=row["session_id"] != sid)
    context["evidence"] = list(evidence.values())
    if not _fits(context):
        raise ValueError("本轮完整输入与必要活动事实超过本机上下文上限，请缩短输入或开始新活动；未截断事实")

    # Memories are whole validated summaries with preserved source IDs, not full DB rows.
    recalled = store.recall(lid, text + " " + str(task.get("goal", "")))
    for memory in recalled:
        if memory["learner_id"] != lid:
            raise ValueError("召回结果含其他学习者记录")
        compact = {key: copy.deepcopy(memory[key]) for key in
                   ("memory_id", "kind", "content", "source_type", "source_event_ids", "scope", "status", "activity_scope") if key in memory}
        compact["historical"] = True
        sources = _rows(store, lid, compact["source_event_ids"])
        if len(sources) != len(set(compact["source_event_ids"])):
            context["context_selection"]["omitted_memories"] += 1
            continue
        trial = copy.deepcopy(context)
        trial["slow_memory"].append(compact)
        trial_evidence = {row["event_id"]: row for row in trial["evidence"]}
        for event_id, row in sources.items():
            if event_id not in trial_evidence:
                trial_evidence[event_id] = _event(row, reference={"memory_id": compact["memory_id"],
                    "verbatim": False, "meaning": "historical summary of the referenced source; not current hardware"}, historical=True)
        trial["evidence"] = list(trial_evidence.values())
        if _fits(trial):
            context = trial
        else:
            context["context_selection"]["omitted_memories"] += 1

    # Select recent attempted results as whole records; do not repeat the latest result.
    attempts = original_state.get("attempts", [])
    for attempt in reversed(attempts[-3:]):
        sources = _rows(store, lid, attempt.get("source_event_ids", []))
        if len(sources) != len(set(attempt.get("source_event_ids", []))):
            continue
        trial = copy.deepcopy(context)
        value = copy.deepcopy(attempt)
        if value.get("report") == task.get("reported_result"):
            value.pop("report", None)
            value["report_reference"] = "task.reported_result"
        trial["task"]["attempts"].insert(0, value)
        trial_evidence = {row["event_id"]: row for row in trial["evidence"]}
        for event_id, row in sources.items():
            if event_id not in trial_evidence:
                trial_evidence[event_id] = _event(row, reference={"attempt_source_ids": value.get("source_event_ids", []),
                    "verbatim": False, "meaning": "reported attempted result in task.attempts"}, historical=row["session_id"] != sid)
        trial["evidence"] = list(trial_evidence.values())
        if _fits(trial):
            context = trial
        else:
            break
    context["context_selection"]["omitted_attempts"] = len(attempts) - len(context["task"]["attempts"])

    turns = [turn for turn in session.get("turns", [])
             if turn["status"] in DELIVERED and turn.get("learner_id", lid) == lid
             and not (turn.get("role") == "user" and eid in turn.get("evidence", []))]
    for turn in reversed(turns):
        trial = copy.deepcopy(context)
        value = {"turn_id": turn["turn_id"], "role": turn["role"], "delivery": turn["status"]}
        if turn["role"] == "assistant" and turn["content"] == task.get("last_delivered_hint"):
            value["text_reference"] = "task.last_delivered_hint"
        else:
            value["text"] = turn["content"]
        trial["recent_delivered_dialogue"].insert(0, value)
        trial_evidence = {row["event_id"]: row for row in trial["evidence"]}
        if turn["role"] == "user":
            for event_id, row in _rows(store, lid, turn.get("evidence", [])).items():
                if event_id not in trial_evidence:
                    trial_evidence[event_id] = _event(row, reference={"turn_id": turn["turn_id"],
                        "verbatim": row["content"].get("text") == turn["content"]}, historical=row["session_id"] != sid)
        trial["evidence"] = list(trial_evidence.values())
        if len(trial["recent_delivered_dialogue"]) > 8 or not _fits(trial):
            break
        context = trial
    context["context_selection"]["omitted_history"] = len(turns) - len(context["recent_delivered_dialogue"])

    recent = store.recent_evidence(sid)
    included = {row["event_id"] for row in context["evidence"]}
    for row in reversed(recent):
        if row["learner_id"] != lid or row["event_id"] in included:
            continue
        trial = copy.deepcopy(context)
        trial["evidence"].append(_event(row))
        if _fits(trial):
            context = trial
            included.add(row["event_id"])
    context["context_selection"]["omitted_evidence"] = sum(row["event_id"] not in included for row in recent if row["learner_id"] == lid)
    context["context_selection"]["chars"] = len(_encoded(context))
    # Diagnostic counters can add a few digits after record selection.
    if not _fits(context):
        raise ValueError("上下文选择元数据超过上限；未截断本轮事实，请缩短输入")
    return context
