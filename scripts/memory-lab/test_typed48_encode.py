"""No-model regressions against an explicitly hash-bound actual typed48 projection."""
import argparse
import copy
import json
from pathlib import Path
import typed48_encode as encoder


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--projection", type=Path, required=True)
    parser.add_argument("--projection-report-sha256", required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    method = encoder.utility.checked_json(encoder.HERE / "typed48_encode_method_v1.json", encoder.METHOD_SHA)
    report, inputs, adapted = encoder.checked_projection(root, args.projection.resolve(), args.projection_report_sha256, method)
    assert len(adapted["sources"]) == 48 and len(inputs) == 240
    fixture = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2"
    manifest = encoder.utility.checked_json(fixture / "manifest.json", method["fixture_manifest_sha256"])
    data = {name: encoder.utility.read_jsonl(fixture / (name + ".jsonl"), manifest["files"][name + ".jsonl"]["sha256"])
            for name in ("sources", "queries", "labels", "scope_controls")}

    def decision(value, kind):
        return next(d for d in value["projection"]["decisions"] if d["projection_kind"] == kind)

    def rejected(name, mutate):
        changed, rows = copy.deepcopy(report), copy.deepcopy(inputs)
        mutate(changed, rows)
        try:
            encoder.validate_payload(changed, rows, data, method, manifest["files"])
        except ValueError:
            return name
        raise AssertionError("Tampered payload accepted: " + name)

    cases = [
        ("constraint_support_not_unknown", lambda r, _: decision(r, "context_constraint")["memory_evidence"]["education_metadata"].update(support_level="hinted")),
        ("constraint_event_relabelled", lambda r, _: decision(r, "context_constraint")["live_imported_event"].update(event_kind="learner_answer")),
        ("source_version_changed", lambda r, _: decision(r, "context_constraint")["live_imported_event"].update(event_version=2)),
        ("source_grade_upgraded", lambda r, _: decision(r, "context_constraint")["memory_evidence"]["education_metadata"].update(source_grade="hardware_verified")),
        ("memory_text_rewritten", lambda r, _: decision(r, "concept_evidence")["memory_evidence"]["memory"].update(content="Invented mastery")),
        ("excluded_plan_promoted", lambda r, _: next(d for d in r["projection"]["decisions"] if d["exclusion_reason"] == "proposal_not_accepted").update(projected=True)),
        ("query_omitted", lambda _, rows: rows.pop()),
        ("query_rewritten", lambda _, rows: rows[-1].update(text="New question")),
        ("duplicate_input", lambda _, rows: rows.__setitem__(-1, copy.deepcopy(rows[-2]))),
    ]
    passed = [rejected(name, change) for name, change in cases]
    print(json.dumps({"valid_actual_projection": True, "tampered_rejected": passed,
                      "test_count": 1 + len(passed), "model_executed": False, "ranking_executed": False}))


if __name__ == "__main__":
    main()
