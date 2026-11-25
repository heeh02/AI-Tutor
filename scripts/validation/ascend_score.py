"""ASCEND text-only scoring proposal. Never imports audio, model, or service code.

Input references always contain all 1315 prepared rows. Accuracy scores that whole
population; --subset-json explicitly scores only its twelve realtime IDs.
The author-derived character table is accompanied by ascend_author_LICENSE.txt.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import sys

FULL_COUNT = 1315
CLEAR_COUNT = 1279
UNCERTAIN_COUNT = 36
REALTIME_COUNT = 12
STATUSES = {"ok", "ok_empty", "failed", "not_run"}
MAX_INPUT_BYTES = 32 * 1024 * 1024
CONFIG_PATH = Path(__file__).with_name("ascend_normalization_v1.json")
CONFIG_BYTES = CONFIG_PATH.read_bytes()
CONFIG = json.loads(CONFIG_BYTES)
IGNORE = frozenset(CONFIG["ignored_characters"])
if len(IGNORE) != 90 or any(len(char) != 1 for char in IGNORE):
    raise RuntimeError("invalid_90_character_normalization_table")
if CONFIG["ignored_codepoints"] != [f"U+{ord(char):04X}" for char in CONFIG["ignored_characters"]]:
    raise RuntimeError("normalization_codepoints_mismatch")
if any(char in IGNORE for char in "[]'"):
    raise RuntimeError("normalization_must_preserve_ascii_unknown_brackets_and_apostrophe")
ASCII_WORD = re.compile(r"[a-z0-9]+(?:'[a-z0-9]+)*")
UNKNOWN = re.compile(r"\[unk\]", re.IGNORECASE)


class ScoreError(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise ScoreError(code)


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_json_object_key")
        result[key] = value
    return result


def parse_json(text):
    try:
        return json.loads(text, object_pairs_hook=strict_object,
                          parse_constant=lambda _: (_ for _ in ()).throw(ScoreError("nonfinite_json")))
    except (json.JSONDecodeError, UnicodeError) as error:
        raise ScoreError("invalid_json") from error


def read_bytes(path):
    path = Path(path)
    require(path.stat().st_size <= MAX_INPUT_BYTES, "input_file_limit")
    with path.open("rb") as source:
        data = source.read(MAX_INPUT_BYTES + 1)
    require(len(data) <= MAX_INPUT_BYTES, "input_file_limit")
    return data


def normalize(text):
    require(isinstance(text, str), "text_must_be_string")
    removed = Counter(char for char in text if char in IGNORE)
    without = "".join(char for char in text if char not in IGNORE)
    lowered = without.lower()
    normalized = " ".join(lowered.split())
    return normalized, {
        "raw_codepoints": len(text),
        "removed_codepoints": sum(removed.values()),
        "removed_by_codepoint": {f"U+{ord(char):04X}": count for char, count in sorted(removed.items())},
        "lowercase_changed": lowered != without,
        "whitespace_before_collapse": sum(char.isspace() for char in lowered),
        "whitespace_after_collapse": normalized.count(" "),
        "normalized_codepoints": len(normalized),
    }


def cer_tokens(normalized):
    return [char for char in normalized if not char.isspace()]


def mer_tokens(normalized):
    tokens = []
    at = 0
    while at < len(normalized):
        if normalized[at].isspace():
            at += 1
            continue
        word = ASCII_WORD.match(normalized, at)
        if word:
            tokens.append(word.group())
            at = word.end()
        else:
            tokens.append(normalized[at])
            at += 1
    return tokens


def edit_counts(reference, hypothesis):
    """Unit-cost edits; ties prefer substitution, then deletion, then insertion."""
    previous = [(j, 0, 0, j) for j in range(len(hypothesis) + 1)]
    for i, ref in enumerate(reference, 1):
        current = [(i, 0, i, 0)]
        for j, hyp in enumerate(hypothesis, 1):
            if ref == hyp:
                current.append(previous[j - 1])
            else:
                diagonal, deletion, insertion = previous[j - 1], previous[j], current[j - 1]
                candidates = [
                    (diagonal[0] + 1, diagonal[1] + 1, diagonal[2], diagonal[3]),
                    (deletion[0] + 1, deletion[1], deletion[2] + 1, deletion[3]),
                    (insertion[0] + 1, insertion[1], insertion[2], insertion[3] + 1),
                ]
                current.append(min(candidates, key=lambda value: value[0]))
        previous = current
    errors, substitutions, deletions, insertions = previous[-1]
    return {"errors": errors, "substitutions": substitutions, "deletions": deletions,
            "insertions": insertions, "reference_units": len(reference)}


def category(value):
    require((type(value) is int) or (isinstance(value, str) and bool(value)), "invalid_stratum_identity")
    return str(value)


def validate_prepared(rows):
    require(isinstance(rows, list) and len(rows) == FULL_COUNT, "prepared_rows_must_be_1315")
    known_ids = set()
    required = {"ordinal", "id", "language", "anonymous_speaker_id", "session_id",
                "reference", "reference_uncertain"}
    for ordinal, row in enumerate(rows):
        require(isinstance(row, dict) and required.issubset(row), "prepared_required_fields_missing")
        require(type(row["ordinal"]) is int and row["ordinal"] == ordinal, "prepared_ordinal_not_file_order")
        require(isinstance(row["id"], str) and 0 < len(row["id"]) <= 128, "invalid_prepared_id")
        require(row["id"] not in known_ids, "duplicate_prepared_id")
        known_ids.add(row["id"])
        require(isinstance(row["language"], str) and row["language"] in {"zh", "en", "mixed"},
                "unknown_prepared_language")
        category(row["anonymous_speaker_id"])
        category(row["session_id"])
        require(isinstance(row["reference"], str) and len(row["reference"]) <= 8000, "invalid_reference")
        require(type(row["reference_uncertain"]) is bool, "uncertainty_must_be_boolean")
        require(row["reference_uncertain"] == bool(UNKNOWN.search(row["reference"])),
                "reference_uncertainty_marker_mismatch")
    require(sum(row["reference_uncertain"] for row in rows) == UNCERTAIN_COUNT,
            "prepared_partition_must_be_1279_and_36")
    require(len({category(row["anonymous_speaker_id"]) for row in rows}) == 2,
            "prepared_test_must_have_two_anonymous_speakers")
    return {row["id"]: row for row in rows}


def select_population(rows, subset):
    if subset is None:
        return "accuracy", rows
    require(isinstance(subset, dict) and isinstance(subset.get("ids"), list),
            "subset_requires_object_with_ids")
    ids = subset["ids"]
    require(len(ids) == REALTIME_COUNT and all(isinstance(value, str) for value in ids),
            "realtime_subset_requires_12_ids")
    require(len(set(ids)) == REALTIME_COUNT, "duplicate_subset_id")
    by_id = {row["id"]: row for row in rows}
    require(all(value in by_id for value in ids), "unknown_subset_id")
    return "realtime", [by_id[value] for value in ids]


def validate_results(results, prepared_by_id, selected_ids, mode):
    require(isinstance(results, list), "results_must_be_jsonl_records")
    matched = {}
    for result in results:
        require(isinstance(result, dict), "result_must_be_object")
        required = {"id", "ordinal", "mode", "attempted", "status", "hypothesis", "final_received"}
        require(required.issubset(result), "result_required_fields_missing")
        identity = result["id"]
        require(isinstance(identity, str) and identity in prepared_by_id, "unknown_result_id")
        require(identity in selected_ids, "result_outside_selected_population")
        require(identity not in matched, "duplicate_result_id")
        require(type(result["ordinal"]) is int and result["ordinal"] == prepared_by_id[identity]["ordinal"],
                "result_id_ordinal_mismatch")
        require(result["mode"] == mode, "result_mode_mismatch")
        require(type(result["attempted"]) is bool, "result_attempted_must_be_boolean")
        require(type(result["final_received"]) is bool, "result_final_received_must_be_boolean")
        require(isinstance(result["status"], str) and result["status"] in STATUSES, "invalid_result_status")
        hypothesis = result["hypothesis"]
        require(hypothesis is None or (isinstance(hypothesis, str) and len(hypothesis) <= 8000),
                "invalid_raw_hypothesis")
        require(result.get("error_code") is None or isinstance(result["error_code"], str),
                "invalid_error_code")
        partial = result.get("partial_at_failure")
        require(partial is None or (isinstance(partial, str) and len(partial) <= 8000),
                "invalid_partial_at_failure")
        if result["status"] in {"ok", "ok_empty"}:
            require(result["attempted"] and result["final_received"] and isinstance(hypothesis, str),
                    "successful_status_requires_attempt_and_final")
            require((result["status"] == "ok_empty") == (not hypothesis.strip()),
                    "ok_empty_status_mismatch")
        if result["status"] == "not_run":
            require(not result["attempted"] and not result["final_received"],
                    "not_run_cannot_claim_attempt_or_final")
        matched[identity] = result
    return matched


def score_row(prepared, result):
    present = result is not None
    raw_status = result["status"] if present else None
    valid_final = present and raw_status in {"ok", "ok_empty"}
    raw_hypothesis = result["hypothesis"] if present else None
    # A failed result's partial or raw text is retained, never promoted to final scoring.
    effective = raw_hypothesis if valid_final else ""
    ref_normalized, ref_stats = normalize(prepared["reference"])
    raw_normalized, hyp_stats = normalize(raw_hypothesis or "")
    hypothesis_normalized = raw_normalized if valid_final else ""
    row = {
        "ordinal": prepared["ordinal"], "id": prepared["id"],
        "language": prepared["language"], "anonymous_speaker_id": prepared["anonymous_speaker_id"],
        "session_id": prepared["session_id"], "reference_uncertain": prepared["reference_uncertain"],
        "unknown_marker_count": len(UNKNOWN.findall(prepared["reference"])),
        "reference_raw": prepared["reference"], "hypothesis_raw": raw_hypothesis,
        "partial_at_failure_raw": result.get("partial_at_failure") if present else None,
        "result_present": present, "status_raw": raw_status,
        "status_for_coverage": raw_status if present else "missing",
        "attempted_raw": result["attempted"] if present else None,
        "final_received_raw": result["final_received"] if present else None,
        "error_code_raw": result.get("error_code") if present else None,
        "valid_final_used_for_scoring": bool(valid_final),
        "scoring_hypothesis_raw": effective,
        "scoring_hypothesis_policy": "valid_final" if valid_final else "empty_due_to_failure_not_run_or_missing",
        "reference_normalized": ref_normalized,
        "hypothesis_raw_normalized_for_audit": raw_normalized if raw_hypothesis is not None else None,
        "scoring_hypothesis_normalized": hypothesis_normalized,
        "normalization": {"reference": ref_stats, "raw_hypothesis": hyp_stats,
                          "raw_hypothesis_was_null_or_missing": raw_hypothesis is None},
        "CER": edit_counts(cer_tokens(ref_normalized), cer_tokens(hypothesis_normalized)),
        "MER_project_v1": edit_counts(mer_tokens(ref_normalized), mer_tokens(hypothesis_normalized)),
        "WER_original_en": edit_counts(ref_normalized.split(), hypothesis_normalized.split())
                           if prepared["language"] == "en" else None,
        "normalized_exact_match": ref_normalized == hypothesis_normalized,
    }
    return row


def metric_total(rows, key):
    values = [row[key] for row in rows if row[key] is not None]
    totals = {name: sum(item[name] for item in values)
              for name in ("errors", "substitutions", "deletions", "insertions", "reference_units")}
    totals["utterances"] = len(values)
    totals["error_rate"] = totals["errors"] / totals["reference_units"] if totals["reference_units"] else None
    return totals


def coverage(rows):
    statuses = Counter(row["status_for_coverage"] for row in rows)
    expected = len(rows)
    successful = sum(row["valid_final_used_for_scoring"] for row in rows)
    attempted = sum(row["attempted_raw"] is True for row in rows)
    return {"expected_rows": expected, "reported_rows": sum(row["result_present"] for row in rows),
            "attempted_rows": attempted, "valid_final_rows": successful,
            "status_counts": {name: statuses[name] for name in ("ok", "ok_empty", "failed", "not_run", "missing")},
            "valid_final_fraction": successful / expected if expected else None,
            "all_expected_rows_reported": all(row["result_present"] for row in rows),
            "complete_selected_inference": bool(expected) and successful == expected}


def group_metrics(rows):
    return {"row_count": len(rows), "coverage": coverage(rows),
            "CER": metric_total(rows, "CER"), "MER_project_v1": metric_total(rows, "MER_project_v1"),
            "WER_original_en": metric_total(rows, "WER_original_en"),
            "normalized_exact_match": {"matches": sum(row["normalized_exact_match"] for row in rows),
                                       "denominator": len(rows),
                                       "rate": sum(row["normalized_exact_match"] for row in rows) / len(rows) if rows else None}}


def removed_character_totals(rows):
    result = {}
    for side in ("reference", "raw_hypothesis"):
        counts = Counter()
        for row in rows:
            counts.update(row["normalization"][side]["removed_by_codepoint"])
        result[side] = {"removed_codepoints": sum(counts.values()), "removed_by_codepoint": dict(sorted(counts.items()))}
    return result


def score(rows, results, subset=None):
    by_id = validate_prepared(rows)
    mode, selected = select_population(rows, subset)
    matched = validate_results(results, by_id, {row["id"] for row in selected}, mode)
    scored = [score_row(row, matched.get(row["id"])) for row in selected]
    clear = [row for row in scored if not row["reference_uncertain"]]
    uncertain = [row for row in scored if row["reference_uncertain"]]
    run_coverage = coverage(scored)
    strata = {}
    for field in ("language", "anonymous_speaker_id", "session_id"):
        groups = []
        for value in sorted({str(row[field]) for row in scored}):
            items = [row for row in scored if str(row[field]) == value]
            clean = [row for row in items if not row["reference_uncertain"]]
            groups.append({"value": value, "selected_rows": len(items),
                           "uncertain_reference_rows": len(items) - len(clean),
                           "reference_complete_metrics": group_metrics(clean),
                           "all_selected_literal_diagnostics": group_metrics(items)})
        strata[field] = groups
    summary = {
        "schema_version": 1, "mode": mode,
        "report_scope": "full_test_accuracy_evaluation" if mode == "accuracy" else "explicit_realtime_subset_only",
        "evaluation_status": "complete_selected_inference" if run_coverage["complete_selected_inference"]
                             else "incomplete_selected_inference",
        "prepared_population": {"rows": FULL_COUNT, "reference_complete_rows": CLEAR_COUNT,
                                "reference_uncertain_rows": UNCERTAIN_COUNT, "anonymous_speakers": 2},
        "selected_ids_in_declared_order": [row["id"] for row in selected],
        "selected_population": {"rows": len(scored), "reference_complete_rows": len(clear),
                                "reference_uncertain_rows": len(uncertain)},
        "coverage": run_coverage,
        "full_1315_inference_complete": mode == "accuracy" and run_coverage["complete_selected_inference"],
        "complete_model_accuracy_claim": False,
        "claim_boundary": "Coverage/completion describes execution. Primary reference-complete metrics are restricted to the declared no-UNK subset. The entire test has uncertain references and cannot establish wholly verifiable all-row accuracy; an incomplete run is not complete-model accuracy.",
        "reference_complete_metrics": group_metrics(clear),
        "reference_complete_coverage": {"rows": len(clear), "selected_denominator": len(scored),
                                       "selected_fraction": len(clear) / len(scored),
                                       "prepared_test_denominator": FULL_COUNT,
                                       "prepared_test_fraction": len(clear) / FULL_COUNT},
        "uncertain_reference": {
            "rows": len(uncertain), "policy": "All IDs, hypotheses and statuses retained; no wildcard, no result-based deletion. UNK is not a known spoken English word.",
            "outputs": [{"id": row["id"], "ordinal": row["ordinal"],
                         "status": row["status_for_coverage"], "hypothesis_raw": row["hypothesis_raw"],
                         "partial_at_failure_raw": row["partial_at_failure_raw"],
                         "attempted_raw": row["attempted_raw"], "final_received_raw": row["final_received_raw"],
                         "error_code_raw": row["error_code_raw"], "unknown_marker_count": row["unknown_marker_count"]}
                        for row in uncertain]},
        "all_selected_literal_diagnostics": {
            "warning": "Literal edit distance includes uncertain reference annotations; this is not fully verifiable ASR accuracy.",
            "metrics": group_metrics(scored)},
        "strata": strata,
        "normalization": {"table_file": CONFIG_PATH.name, "table_sha256": hashlib.sha256(CONFIG_BYTES).hexdigest(),
                          "ignored_character_count": len(IGNORE),
                          "operations": ["exact fixed 90-character removal", "str.lower", "whitespace collapse and trim"],
                          "symmetric_reference_hypothesis": True,
                          "raw_and_removed_character_counts_retained": True,
                          "removed_character_totals": removed_character_totals(scored)},
        "metric_definition": {
            "aggregation": "sum per-utterance S+D+I / sum reference units; never average sentence rates",
            "CER": "non-whitespace normalized Unicode code points",
            "MER_project_v1": "ASCII letters/digits with internal straight apostrophes as words; every other non-whitespace Unicode code point as one unit",
            "WER_original_en": "normalized whitespace-delimited words on original language=en rows only",
            "not_author_jieba_reproduction": True, "error_rates_clamped": False,
            "zero_reference_unit_denominator": "null metric with counts retained",
            "failed_not_run_missing_hypothesis": "empty for scoring; original raw hypothesis/status/partial retained"}}
    return summary, scored


def load_inputs(rows_path, results_path, subset_path=None):
    raw_rows, raw_results = read_bytes(rows_path), read_bytes(results_path)
    rows = parse_json(raw_rows)
    results = []
    for line in raw_results.splitlines():
        if line.strip():
            results.append(parse_json(line))
    raw_subset = read_bytes(subset_path) if subset_path is not None else None
    subset = parse_json(raw_subset) if raw_subset is not None else None
    identities = {
        "prepared_rows_sha256": hashlib.sha256(raw_rows).hexdigest(),
        "results_jsonl_sha256": hashlib.sha256(raw_results).hexdigest(),
        "subset_json_sha256": hashlib.sha256(raw_subset).hexdigest() if raw_subset is not None else None,
    }
    return rows, results, subset, identities


def run(rows_path, results_path, output, subset_path=None):
    rows, results, subset, identities = load_inputs(rows_path, results_path, subset_path)
    summary, scored = score(rows, results, subset)
    summary["input_identity"] = identities
    summary["scorer_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    serialized_rows = (json.dumps(scored, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode("utf-8")
    summary["per_row_scored_sha256"] = hashlib.sha256(serialized_rows).hexdigest()
    serialized_summary = (json.dumps(summary, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode("utf-8")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    with (output / "per-row-scored.json").open("xb") as target:
        target.write(serialized_rows)
    with (output / "summary.json").open("xb") as target:
        target.write(serialized_summary)
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rows", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--subset-json", type=Path)
    args = parser.parse_args(argv)
    try:
        summary = run(args.rows, args.results, args.output, args.subset_json)
    except (ScoreError, OSError) as error:
        code = str(error) if isinstance(error, ScoreError) else type(error).__name__
        parser.exit(2, f"scoring_rejected: {code}\n")
    print(json.dumps({"mode": summary["mode"], "evaluation_status": summary["evaluation_status"],
                      "expected_rows": summary["coverage"]["expected_rows"],
                      "valid_final_rows": summary["coverage"]["valid_final_rows"],
                      "output": str(args.output)}, ensure_ascii=False))


if __name__ == "__main__":
    main()

