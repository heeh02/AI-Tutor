"""Text-only contract regressions for the unapplied ASCEND scorer."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import ascend_score as score


def prepared():
    rows = []
    for ordinal in range(score.FULL_COUNT):
        language = "zh" if ordinal < 685 else "mixed" if ordinal < 1058 else "en"
        uncertain = ordinal >= score.CLEAR_COUNT
        reference = ("你好" if language == "zh" else "你好 test" if language == "mixed" else "a b")
        if uncertain:
            reference += " [UNK]"
        rows.append({"ordinal": ordinal, "id": f"{ordinal:05d}", "language": language,
                     "anonymous_speaker_id": 3 if ordinal < 454 else 17,
                     "session_id": ordinal % 4 + 1, "reference": reference,
                     "reference_uncertain": uncertain})
    return rows


def result(row, hypothesis=None, status="ok", mode="accuracy", attempted=True, final=True):
    return {"id": row["id"], "ordinal": row["ordinal"], "mode": mode,
            "attempted": attempted, "status": status,
            "hypothesis": row["reference"] if hypothesis is None else hypothesis,
            "final_received": final, "error_code": None}


class AscendScoreTests(unittest.TestCase):
    def test_symmetric_normalization_preserves_unknown_and_contractions_with_audit(self):
        normalized, stats = score.normalize("  DON'T，停！ [UNK]  ")
        self.assertEqual(normalized, "don't停 [unk]")
        self.assertEqual(score.mer_tokens(normalized), ["don't", "停", "[", "unk", "]"])
        self.assertEqual(stats["removed_codepoints"], 2)
        self.assertEqual(stats["removed_by_codepoint"], {"U+FF01": 1, "U+FF0C": 1})
        self.assertEqual(len(score.IGNORE), 90)
        self.assertEqual(score.normalize("Ａ")[0], "ａ")  # lower, not width normalization.

    def test_corpus_weighting_and_unclamped_insertions(self):
        rows = prepared()
        left = copy.deepcopy(rows[0])
        right = copy.deepcopy(rows[1])
        left["reference"], right["reference"] = "a", "abcdefghij"
        failed = result(left, hypothesis="a b c", status="failed", final=False)
        first = score.score_row(left, failed)
        second = score.score_row(right, result(right))
        total = score.metric_total([first, second], "CER")
        self.assertEqual((total["errors"], total["reference_units"], total["error_rate"]), (1, 11, 1 / 11))
        inserted = score.score_row(left, result(left, hypothesis="a b c"))
        self.assertEqual(score.metric_total([inserted], "MER_project_v1")["error_rate"], 2)
        zero = dict(inserted, MER_project_v1=score.edit_counts([], ["word"]))
        self.assertIsNone(score.metric_total([zero], "MER_project_v1")["error_rate"])

    def test_missing_and_failed_raw_text_cannot_erase_full_reference_denominator(self):
        rows = prepared()
        failed = result(rows[0], hypothesis="retained final-looking raw text", status="failed", final=True)
        failed["partial_at_failure"] = "PRIVATE_PARTIAL_NOT_A_FINAL"
        failed["error_code"] = "protocol_failure"
        summary, per_row = score.score(rows, [failed])
        self.assertEqual(summary["coverage"]["expected_rows"], 1315)
        self.assertEqual(summary["coverage"]["status_counts"]["missing"], 1314)
        self.assertEqual(summary["reference_complete_metrics"]["row_count"], 1279)
        self.assertEqual(summary["uncertain_reference"]["rows"], 36)
        self.assertFalse(summary["full_1315_inference_complete"])
        self.assertEqual(per_row[0]["hypothesis_raw"], failed["hypothesis"])
        self.assertEqual(per_row[0]["partial_at_failure_raw"], failed["partial_at_failure"])
        self.assertEqual(per_row[0]["scoring_hypothesis_normalized"], "")
        self.assertEqual(per_row[0]["CER"]["deletions"], per_row[0]["CER"]["reference_units"])
        self.assertIsNone(per_row[1]["status_raw"])
        self.assertEqual(per_row[1]["status_for_coverage"], "missing")
        self.assertEqual(summary["reference_complete_metrics"]["CER"]["error_rate"], 1)

    def test_duplicate_unknown_mismatched_or_wrong_mode_results_rejected(self):
        rows = prepared()
        original = result(rows[0])
        cases = [
            [original, original],
            [dict(original, id="unknown")],
            [dict(original, ordinal=1)],
            [dict(original, mode="realtime")],
            [dict(original, attempted=False)],
            [dict(original, final_received=False)],
            [dict(original, hypothesis="", status="ok")],
            [dict(original, attempted=1)],
        ]
        for records in cases:
            with self.subTest(records=records):
                with self.assertRaises(score.ScoreError):
                    score.score(rows, records)

    def test_prepared_count_partition_and_marker_contract_rejected(self):
        rows = prepared()
        cases = [rows[:-1]]
        duplicate = copy.deepcopy(rows)
        duplicate[1]["id"] = duplicate[0]["id"]
        cases.append(duplicate)
        mismatch = copy.deepcopy(rows)
        mismatch[-1]["reference_uncertain"] = False
        cases.append(mismatch)
        changed_partition = copy.deepcopy(rows)
        changed_partition[-1]["reference_uncertain"] = False
        changed_partition[-1]["reference"] = "a b"
        cases.append(changed_partition)
        for bad in cases:
            with self.assertRaises(score.ScoreError):
                score.score(bad, [])

    def test_realtime_is_explicit_twelve_population_and_rejects_extra_rows(self):
        rows = prepared()
        ids = [row["id"] for row in rows[:11]] + [rows[-1]["id"]]
        subset = {"ids": ids, "selection_method": "fixed_before_results"}
        selected = rows[:11] + [rows[-1]]
        records = [result(row, mode="realtime") for row in selected]
        summary, per_row = score.score(rows, records, subset)
        self.assertEqual(summary["mode"], "realtime")
        self.assertEqual(summary["coverage"]["expected_rows"], 12)
        self.assertTrue(summary["coverage"]["complete_selected_inference"])
        self.assertFalse(summary["full_1315_inference_complete"])
        self.assertEqual(summary["reference_complete_metrics"]["row_count"], 11)
        self.assertEqual(summary["reference_complete_coverage"]["prepared_test_fraction"], 11 / 1315)
        self.assertEqual(summary["uncertain_reference"]["rows"], 1)
        self.assertEqual([row["id"] for row in per_row], ids)
        with self.assertRaises(score.ScoreError):
            score.score(rows, records + [result(rows[12], mode="realtime")], subset)
        with self.assertRaises(score.ScoreError):
            score.score(rows, records, {"ids": ids[:-1] + [ids[0]]})
        with self.assertRaises(score.ScoreError):
            score.score(rows, records, ids)

    def test_complete_run_preserves_uncertain_outputs_without_accuracy_overclaim(self):
        rows = prepared()
        records = [result(row) for row in rows]
        summary, per_row = score.score(rows, records)
        self.assertTrue(summary["full_1315_inference_complete"])
        self.assertEqual(summary["reference_complete_metrics"]["CER"]["errors"], 0)
        self.assertEqual(summary["reference_complete_metrics"]["row_count"], 1279)
        self.assertEqual(summary["reference_complete_coverage"]["prepared_test_fraction"], 1279 / 1315)
        self.assertEqual(len(summary["uncertain_reference"]["outputs"]), 36)
        self.assertFalse(summary["complete_model_accuracy_claim"])
        self.assertIn("not fully verifiable", summary["all_selected_literal_diagnostics"]["warning"])
        self.assertEqual(len(per_row), 1315)
        self.assertEqual(len(summary["strata"]["anonymous_speaker_id"]), 2)

    def test_ok_empty_and_not_run_remain_distinct_with_empty_scoring_hypotheses(self):
        rows = prepared()
        records = [result(rows[0], hypothesis="", status="ok_empty"),
                   result(rows[1], hypothesis="late untrusted text", status="not_run", attempted=False, final=False)]
        summary, per_row = score.score(rows, records)
        self.assertEqual(summary["coverage"]["valid_final_rows"], 1)
        self.assertEqual(summary["coverage"]["status_counts"]["not_run"], 1)
        self.assertTrue(per_row[0]["valid_final_used_for_scoring"])
        self.assertFalse(per_row[1]["valid_final_used_for_scoring"])
        self.assertEqual(per_row[1]["hypothesis_raw"], "late untrusted text")
        self.assertEqual(per_row[1]["scoring_hypothesis_raw"], "")

    def test_output_create_new_and_input_bytes_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            rows_path, results_path = directory / "rows.json", directory / "results.jsonl"
            rows = prepared()
            rows_path.write_text(json.dumps(rows), encoding="utf-8")
            results_path.write_text(json.dumps(result(rows[0])) + "\n", encoding="utf-8")
            before = {path: hashlib.sha256(path.read_bytes()).hexdigest() for path in (rows_path, results_path)}
            output = directory / "scored"
            score.run(rows_path, results_path, output)
            self.assertEqual(len(json.loads((output / "per-row-scored.json").read_text(encoding="utf-8"))), 1315)
            with self.assertRaises(FileExistsError):
                score.run(rows_path, results_path, output)
            for path, expected in before.items():
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), expected)
            with self.assertRaises(score.ScoreError):
                score.parse_json('{"id":"one","id":"two"}')


if __name__ == "__main__":
    unittest.main()

