"""Pure report/selection regressions. No tokenizers, models, network or DB."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import bge_m3_shadow as probe


class ShadowContractTests(unittest.TestCase):
    def test_alternative_sets_are_not_union_or_any_partial_hit(self):
        sufficient = [["a", "b"], ["c"]]
        partial = probe.metrics(["a", "d"], sufficient)
        self.assertTrue(partial["hit_at_1"])
        self.assertFalse(partial["complete_evidence_at_5"])
        self.assertEqual(partial["best_valid_set_coverage_at_5"], 0.5)
        alternate = probe.metrics(["d", "c"], sufficient)
        self.assertTrue(alternate["complete_evidence_at_3"])
        self.assertEqual(alternate["first_complete_evidence_reciprocal_rank_at_5"], 0.5)

    def test_complete_and_first_relevant_reciprocal_rank_are_distinct(self):
        result = probe.metrics(["a", "d", "b"], [["a", "b"]])
        self.assertEqual(result["reciprocal_rank_at_5"], 1.0)
        self.assertEqual(result["first_complete_evidence_reciprocal_rank_at_5"], 1 / 3)
        late = probe.metrics(["x", "y", "z", "d", "e", "a"], [["a"]])
        self.assertEqual(late["reciprocal_rank_at_5"], 0)
        self.assertFalse(late["complete_evidence_at_5"])

    def test_budget_skips_whole_source_without_truncating(self):
        sources = {"large": {"text": "长" * 8}, "a": {"text": "中文"}, "b": {"text": "123"}}
        result = probe.select_whole([{"id": key} for key in ("large", "a", "b")], sources, limit=2, budget=5)
        self.assertEqual(result, {"ids": ["a", "b"], "chars": 5, "skipped_for_budget": ["large"]})

    def test_negative_candidate_is_not_factual_acceptance_or_abstention(self):
        result = probe.metrics(["unrelated"], [])
        self.assertFalse(result["answerable"])
        self.assertEqual(result["candidates_returned"], 1)
        self.assertEqual(result["abstention"], "not_implemented")
        self.assertNotIn("complete_evidence_at_5", result)

    def test_scope_controls_do_not_use_text_similarity(self):
        context = dict(learner_id="owner", namespace="simulator", activity_id="workshop", scope="current_activity", expected_source_version=1)
        source = {**context, "status": "active", "source_type": "simulated", "version": 1, "text": "same text"}
        self.assertTrue(probe.checker.eligible(source, context))
        for field, value in (("learner_id", "foreign"), ("namespace", "real"), ("activity_id", "other"), ("status", "invalid"), ("version", 2)):
            self.assertFalse(probe.checker.eligible({**source, field: value}, context))

    def test_old_v1_cannot_produce_rankings(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            content = json.dumps({"fixture_version": "education_memory_diagnostic_postfreeze_v1_20261104"}).encode()
            (directory / "manifest.json").write_bytes(content)
            with self.assertRaisesRegex(ValueError, "Only reviewed v2"):
                probe.checked_fixture(directory, hashlib.sha256(content).hexdigest())

    def test_goal_format_preserves_actual_context_whitespace(self):
        query = {"text": "question?", "goal": "goal"}
        self.assertEqual(probe.query_text(query, "current_query"), "question?")
        self.assertEqual(probe.query_text(query, "query_plus_goal"), "question? goal ")

    def test_quality_gates_use_v2_multi_denominator_and_do_not_claim_abstention(self):
        rows = []
        for mode, total, found in (("zh_to_en", 24, 20), ("mixed_multi", 11, 10), ("same_zh", 37, 35)):
            rows.extend({"mode": mode, "metrics": {"answerable": True, "complete_evidence_at_5": i < found},
                         "selected": {"ids": ["candidate"]}} for i in range(total))
        rows.extend({"mode": "negative_en", "metrics": {"answerable": False}, "selected": {"ids": ["candidate"]}} for _ in range(24))
        results = {"bge_m3": {"current_query": {"cases": rows}}}
        deltas = [{"condition": "current_query", "baseline": baseline,
                   "all_positive_queries": [{"pack_id": "topic", "complete_evidence_at_5_delta": 0}]} for baseline in ("e5_fp32", "lexical")]
        result = probe.quality_gates(results, [{"expected_eligible": False, "actual_eligible": False}] * 12, deltas)["bge_m3"]["current_query"]
        self.assertEqual(result["coverage"]["multi_complete_at_5"], {"numerator": 10, "denominator": 11, "minimum_count": 10, "passed": True})
        self.assertTrue(result["next_opt_in_candidate_gate_passed"])
        self.assertIsNone(result["negative_review"]["negative_quality_pass"])
        self.assertEqual(result["negative_review"]["with_candidates"], 24)
        self.assertFalse(result["deployment_allowed"])

    def test_topic_losses_cannot_be_hidden_by_overall_gain(self):
        results = {"bge_m3": {"current_query": {"cases": []}}}
        deltas = [{"condition": "current_query", "baseline": "e5_fp32", "all_positive_queries":
                   [{"pack_id": "lost_topic", "complete_evidence_at_5_delta": -1}] * 2
                   + [{"pack_id": "gained_topic", "complete_evidence_at_5_delta": 1}] * 3}]
        result = probe.quality_gates(results, [], deltas)["bge_m3"]["current_query"]["baseline_non_regression"]["e5_fp32"]
        self.assertEqual(result["complete_count_delta"], 1)
        self.assertFalse(result["passed"])


if __name__ == "__main__":
    unittest.main()
