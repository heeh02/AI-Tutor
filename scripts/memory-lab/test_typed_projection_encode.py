"""No-model contract tests. Mock JSON is not claimed as a real Store projection."""
import copy
import json
from pathlib import Path
import unittest

import typed_projection_encode as encoder


class ProjectionInputTests(unittest.TestCase):
    def setUp(self):
        root = Path(__file__).resolve().parents[2]
        fixture = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2"
        self.sources = [json.loads(row) for row in (fixture / "sources.jsonl").read_text(encoding="utf-8").splitlines()]
        self.queries = [json.loads(row) for row in (fixture / "queries.jsonl").read_text(encoding="utf-8").splitlines()]
        self.files = json.loads((fixture / "manifest.json").read_text(encoding="utf-8"))["files"]
        self.method = json.loads(Path(__file__).with_name("typed_projection_encode_method_v1.json").read_text(encoding="utf-8"))
        decisions, inputs, counts, excluded = [], [], encoder.Counter(), encoder.Counter()
        for source in self.sources:
            kind = encoder.projection_kind(source)
            row = {"source_id":source["id"], "original_source":source,
                   "original_text_sha256":encoder.sha_bytes(source["text"].encode("utf-8")),
                   "projection_kind":kind, "projected":kind is not None}
            if kind is None:
                reason = {"condition_limit":"unsupported_condition_limit_kind", "unaccepted_plan":"proposal_not_accepted", "temporary_request":"temporary_request_not_persistent_preference"}[source["claim_type"]]
                row["exclusion_reason"] = reason
                excluded[reason] += 1
            else:
                text = encoder.PREFIXES[kind] + "：" + source["text"]
                eid, mid = "mock-event-" + source["id"], "mock-memory-" + source["id"]
                content = {"text":source["text"], "support_level":source["support_level"]}
                if source["accepted"] is not None:
                    content["accepted"] = source["accepted"]
                row.update(event_id=eid, event_version=1, memory_id=mid, encoding_text=text,
                           encoding_text_sha256=encoder.sha_bytes(text.encode("utf-8")),
                           memory_evidence={"memory":{"memory_id":mid,"source_event_ids":[eid],"content":text},
                               "education_metadata":{"kind":kind,"claim_type":source["claim_type"],"support_level":source["support_level"],"source_grade":"simulated","source_count":1},
                               "sources":[{"event_id":eid,"event_version":1,"event_kind":source["event_kind"],"event_status":"active","content":content}]})
                inputs.append({"key":"source:"+source["id"],"input_kind":"projected_memory","source_ids":[source["id"]],"text":text,"text_sha256":row["encoding_text_sha256"]})
                counts[kind] += 1
            decisions.append(row)
        for condition in encoder.CONDITIONS:
            for query in self.queries:
                text = query["text"] if condition == "current_query" else query["text"] + " " + query["goal"] + " "
                inputs.append({"key":condition+":"+query["id"],"input_kind":condition,"query_id":query["id"],"text":text,"text_sha256":encoder.sha_bytes(text.encode("utf-8"))})
        self.report = {"status":"projection_completed_no_encoding", "fixture_manifest_sha256":self.method["fixture_manifest_sha256"],
                       "method_sha256":self.method["projection_method_sha256"],"probe_sha256":self.method["projection_probe_sha256"],
                       "original_fixture_files":self.files,"models_executed":False,"ranking_executed":False,"production_changed":False,"devices_used":False,
                       "projection":{"projected_count":42,"excluded_count":18,"live_memory_evidence_rechecked":42,"encoder_profile":self.method["profile"],
                           "persistent_preference_coverage":0,"measured_hint_effectiveness_coverage":0,
                           "jobs":[{"status":"applied","model_call_started":False} for _ in range(12)],
                           "sessions":[{"progress_state_unchanged":True} for _ in range(6)],"decisions":decisions,
                           "kind_counts":dict(counts),"exclusion_counts":dict(excluded)}}
        self.inputs = inputs

    def validate(self, report=None, inputs=None):
        return encoder.validate_payload(report or self.report, self.inputs if inputs is None else inputs,
                                        self.sources, self.queries, self.method, self.files)

    def test_full_contract_and_query_text_are_preserved(self):
        data = self.validate()
        self.assertEqual(len(data["sources"]), 42)
        self.assertEqual(data["queries"], self.queries)
        self.assertEqual(len(self.inputs), 234)

    def test_rewritten_query_is_rejected_even_with_recomputed_local_hash(self):
        inputs = copy.deepcopy(self.inputs)
        inputs[42]["text"] += " rewritten"
        inputs[42]["text_sha256"] = encoder.sha_bytes(inputs[42]["text"].encode("utf-8"))
        with self.assertRaises(ValueError): self.validate(inputs=inputs)

    def test_goal_trailing_space_and_full_query_count_are_required(self):
        for change in ("trim", "omit", "duplicate"):
            inputs = copy.deepcopy(self.inputs)
            if change == "trim": inputs[-1]["text"] = inputs[-1]["text"].rstrip()
            elif change == "omit": inputs.pop()
            else: inputs[-1] = inputs[-2]
            with self.assertRaises(ValueError): self.validate(inputs=inputs)

    def test_raw_source_vector_text_cannot_replace_typed_memory_text(self):
        inputs = copy.deepcopy(self.inputs)
        inputs[0]["text"] = self.sources[0]["text"]
        inputs[0]["text_sha256"] = encoder.sha_bytes(inputs[0]["text"].encode("utf-8"))
        with self.assertRaises(ValueError): self.validate(inputs=inputs)

    def test_hint_grade_version_and_profile_cannot_be_strengthened(self):
        for changed in ("support", "grade", "version", "profile"):
            report = copy.deepcopy(self.report)
            row = next(row for row in report["projection"]["decisions"] if row["original_source"]["support_level"] == "hinted")
            if changed == "support": row["memory_evidence"]["education_metadata"]["support_level"] = "independent"
            elif changed == "grade": row["memory_evidence"]["education_metadata"]["source_grade"] = "operator_confirmed"
            elif changed == "version": row["memory_evidence"]["sources"][0]["event_version"] = 2
            else: report["projection"]["encoder_profile"]["dimension"] = 384
            with self.assertRaises(ValueError): self.validate(report=report)

    def test_excluded_plan_cannot_acquire_encoder_text(self):
        report = copy.deepcopy(self.report)
        row = next(row for row in report["projection"]["decisions"] if row["original_source"]["claim_type"] == "unaccepted_plan")
        row["encoding_text"] = row["original_source"]["text"]
        with self.assertRaises(ValueError): self.validate(report=report)

    def test_nonmonotonic_hwm_is_reported_without_fabricating_a_peak(self):
        samples = [{"available":True,"VmRSS_bytes":rss,"VmHWM_bytes":hwm} for rss,hwm in [(10,20),(15,25),(14,24),(12,23)]]
        runtime = {"memory":dict(zip(("before_load","after_load","after_encoding","after_unload"),samples))}
        summary = encoder.memory_summary(runtime,samples[-1])
        self.assertTrue(summary["hwm_decrease_observed"])
        self.assertEqual(summary["maximum_observed_rss_bytes"],15)


if __name__ == "__main__":
    unittest.main()
