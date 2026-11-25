"""Independent label/budget recount and frozen-vector paired audit, no model or Store calls."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct

FIXTURE_SHA = "858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81"
OLD_REPORT_SHA = "ff9d876a315074cc5c1bd7bcd8b277719a37f6df9968cae217703640d87a38ad"
OLD_VECTORS_SHA = "925fffa9bb29737fcaa6014edb42a55a010c38d7276859e2981aa578a37ea6f2"
METHODS = ("legacy_lexical", "pure_bge", "actual_rrf")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checked(path, sha):
    require(digest(path) == sha, "Input hash changed: " + str(path))
    return json.loads(path.read_bytes())


def choose(ranking, texts):
    ids, skipped, total = [], [], 0
    for row in ranking:
        if len(ids) == 5:
            break
        sid = row["source_id"]
        size = len(texts[sid])
        require(row["whole_chars"] == size, "Wrong whole-text character count")
        if total + size > 2400:
            skipped.append(sid)
        else:
            ids.append(sid)
            total += size
    return {"source_ids": ids, "whole_chars": total, "skipped_for_budget": skipped}


def metrics(ids, alternatives):
    if not alternatives:
        return {"answerable": False, "candidates_returned": len(ids), "abstention_tested": False}
    selected = set(ids)
    sets = [set(s) for s in alternatives]
    first = next((i for i, sid in enumerate(ids) if any(sid in s for s in sets)), None)
    return {"answerable": True, "complete_evidence_at_5": any(s <= selected for s in sets),
            "reciprocal_rank_at_5": 0.0 if first is None else 1.0 / (first + 1),
            "best_set_coverage_at_5": max(len(s & selected) / len(s) for s in sets)}


def summarize(cases, method):
    positive = [c for c in cases if c["alternatives"]]
    available = [c for c in positive if c["complete_set_available"]]
    negative = [c for c in cases if not c["alternatives"]]
    return {"original_positive_denominator": len(positive), "available_positive_denominator": len(available),
            "complete_evidence_at_5": sum(c[method]["metrics"]["complete_evidence_at_5"] for c in positive),
            "complete_available_evidence_at_5": sum(c[method]["metrics"]["complete_evidence_at_5"] for c in available),
            "mrr_at_5_original_positive": sum(c[method]["metrics"]["reciprocal_rank_at_5"] for c in positive) / len(positive) if positive else None,
            "negative_denominator": len(negative), "negative_with_candidates": sum(c[method]["metrics"]["candidates_returned"] > 0 for c in negative),
            "abstention_tested": False}


def same_metrics(actual, expected):
    require(actual.keys() == expected.keys(), "Metric fields differ")
    for key, value in expected.items():
        require(math.isclose(actual[key], value, rel_tol=1e-12, abs_tol=1e-12) if isinstance(value, float) else actual[key] == value,
                "Metric mismatch: " + key)


def vector_map(rows):
    result = {}
    for row in rows:
        vector = row["encoding"]["pooled_vector"]
        require(len(vector) == 1024 and all(map(math.isfinite, vector)), "Invalid vector")
        require(hashlib.sha256(struct.pack("<1024f", *vector)).hexdigest() == row["encoding"]["pooled_sha256_f32le"], "Vector hash mismatch")
        require(row["key"] not in result, "Duplicate vector key")
        result[row["key"]] = row
    return result


def audit(root, report_path, report_sha, encoding_dir):
    report = checked(report_path, report_sha)
    old = checked(root / "runtime/goal-baselines/20261104/typed-store-first/report.json", OLD_REPORT_SHA)
    fixture = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2"
    manifest = checked(fixture / "manifest.json", FIXTURE_SHA)
    require(report["fixture_manifest_sha256"] == FIXTURE_SHA, "Wrong fixture")
    data = {}
    for name in ("sources", "queries", "labels", "scope_controls"):
        path = fixture / (name + ".jsonl")
        require(digest(path) == manifest["files"][name + ".jsonl"]["sha256"], "Original fixture changed")
        data[name] = [json.loads(line) for line in path.read_text("utf-8").splitlines()]
    require((len(data["sources"]), len(data["queries"]), len(data["labels"]), len(data["scope_controls"])) == (60, 96, 96, 12), "Wrong frozen corpus count")
    labels = {r["query_id"]: r for r in data["labels"]}
    queries = {r["id"]: r for r in data["queries"]}
    decisions = {d["source_id"]: d for d in report["reconstructed_decisions"] if d["projected"]}
    texts = {sid: d["memory_evidence"]["memory"]["content"] for sid, d in decisions.items()}
    mids = {sid: d["memory_id"] for sid, d in decisions.items()}
    old_texts = {d["source_id"]: d["memory_evidence"]["memory"]["content"] for d in old["reconstructed_bindings"]}
    require(len(texts) == 48 and len(old_texts) == 42 and all(texts[s] == t for s, t in old_texts.items()), "Old42 text or new48 pool changed")
    new_sources = set(texts) - set(old_texts)
    require(len(new_sources) == 6 and all(decisions[s]["projection_kind"] == "context_constraint" for s in new_sources), "Unexpected additional source")
    encoding = checked(encoding_dir / "report.json", report["encoding_report_sha256"])
    require(encoding["encodings_sha256"] == report["vectors_sha256"], "Encoding lineage changed")
    new_vectors = vector_map(checked(encoding_dir / "bge_m3-encodings.json", report["vectors_sha256"]))
    old_vectors = vector_map(checked(root / "runtime/goal-baselines/20261104/typed-encode-first/bge_m3-encodings.json", OLD_VECTORS_SHA))
    require(len(new_vectors) == 240 and len(old_vectors) == 234, "Wrong vector count")
    shared = list(old_vectors)
    require(all(new_vectors[k]["text_sha256"] == old_vectors[k]["text_sha256"] for k in shared), "Shared input text changed")
    vector_comparison = {"shared_inputs": 234,
        "exact_token_inputs": sum(new_vectors[k]["inputs"] == old_vectors[k]["inputs"] for k in shared),
        "exact_vectors": sum(new_vectors[k]["encoding"]["pooled_vector"] == old_vectors[k]["encoding"]["pooled_vector"] for k in shared),
        "max_coordinate_difference": max(abs(a-b) for k in shared for a,b in zip(new_vectors[k]["encoding"]["pooled_vector"], old_vectors[k]["encoding"]["pooled_vector"]))}
    evaluations = []
    require([e["condition"] for e in report["evaluations"]] == ["current_query", "query_plus_goal"], "Wrong conditions")
    for evaluation in report["evaluations"]:
        condition = evaluation["condition"]
        old_cases = {c["query_id"]: c for e in old["evaluations"] if e["condition"] == condition for c in e["cases"]}
        cases, largest_score_error = [], 0.0
        require(len(evaluation["cases"]) == 96 and len({c["query_id"] for c in evaluation["cases"]}) == 96, "Missing/duplicate query")
        for row in evaluation["cases"]:
            qid = row["query_id"]
            label, query = labels[qid], queries[qid]
            alternatives = label["sufficient_evidence_sets"]
            require(row["original_sufficient_evidence_sets"] == alternatives and row["mode"] == query["mode"], "Original label changed")
            available = any(set(s) <= set(texts) for s in alternatives)
            require(row["complete_set_available"] is available, "Availability trimmed a source set")
            expected_query = query["text"] if condition == "current_query" else query["text"] + " " + query["goal"] + " "
            require(row["query_text_sha256"] == hashlib.sha256(expected_query.encode()).hexdigest(), "Query rewritten")
            case = {"query_id": qid, "mode": query["mode"], "pack_id": query["pack_id"], "alternatives": alternatives, "complete_set_available": available}
            for method in METHODS:
                ranked = row[method]["ranking"]
                require(len({r["source_id"] for r in ranked}) == len(ranked), "Duplicate candidate")
                require(all(r["source_id"] in texts and r["memory_id"] == mids[r["source_id"]] for r in ranked), "Candidate source or memory mismatch")
                if method == "pure_bge":
                    require(len(ranked) == 48 and set(r["source_id"] for r in ranked) == set(texts), "Incomplete pure pool")
                    qv = new_vectors[condition + ":" + qid]["encoding"]["pooled_vector"]
                    for r in ranked:
                        dv = new_vectors["source:" + r["source_id"]]["encoding"]["pooled_vector"]
                        score = sum(a*b for a,b in zip(qv,dv)) / math.sqrt(sum(a*a for a in qv) * sum(b*b for b in dv))
                        largest_score_error = max(largest_score_error, abs(score-r["score"]))
                        require(abs(score-r["score"]) < 1e-6, "Reported cosine differs from frozen vectors")
                    require(ranked == sorted(ranked, key=lambda r: (-r["score"], r["memory_id"])), "Pure ranking order changed")
                else:
                    require(len(ranked) <= 5 and sum(r["whole_chars"] for r in ranked) <= 2400, "Store return budget exceeded")
                selected = choose(ranked, texts)
                require(selected == row[method]["selection"], "Selection or budget mismatch")
                measured = metrics(selected["source_ids"], alternatives)
                same_metrics(row[method]["metrics"], measured)
                case[method] = {"selection": selected, "metrics": measured}
            subset = choose([r for r in row["pure_bge"]["ranking"] if r["source_id"] in old_texts], old_texts)
            case["new_vectors_42_subset"] = {"selection": subset, "metrics": metrics(subset["source_ids"], alternatives)}
            old_row = old_cases[qid]["pure_eligible_cosine"]
            old_selected = choose(old_row["ranking"], old_texts)
            require(old_selected == old_row["selection"], "Old42 budget mismatch")
            case["old42_actual_pure"] = {"selection": old_selected, "metrics": metrics(old_selected["source_ids"], alternatives)}
            case["complete_sets_using_new_condition"] = [s for s in alternatives if set(s) <= set(case["pure_bge"]["selection"]["source_ids"]) and set(s) & new_sources]
            cases.append(case)
        summaries = {}
        for method in METHODS:
            strata = {"overall": cases, "cross_language": [c for c in cases if c["mode"] in ("zh_to_en", "en_to_zh")],
                      "mixed_multi": [c for c in cases if c["mode"] == "mixed_multi"]}
            summaries[method] = {name: summarize(rows, method) for name, rows in strata.items()}
            summaries[method]["topics"] = {topic: summarize([c for c in cases if c["pack_id"] == topic], method) for topic in sorted({c["pack_id"] for c in cases})}
            for stratum, values in summaries[method].items():
                if stratum == "topics":
                    for topic, value in values.items():
                        same_metrics(evaluation["summary"][method][stratum][topic], value)
                else:
                    same_metrics(evaluation["summary"][method][stratum], values)
        paired = {}
        for before, after in (("old42_actual_pure", "new_vectors_42_subset"), ("new_vectors_42_subset", "pure_bge"), ("pure_bge", "actual_rrf")):
            good = lambda c, name: c[name]["metrics"].get("complete_evidence_at_5", False)
            paired[before + "_to_" + after] = {"gains": [c["query_id"] for c in cases if not good(c,before) and good(c,after)],
                "regressions": [c["query_id"] for c in cases if good(c,before) and not good(c,after)]}
        evaluations.append({"condition": condition, "summary": summaries, "paired": paired,
                            "max_recomputed_cosine_error": largest_score_error, "cases": cases})
    return {"status": "verified", "audit_kind": "post_hoc_independent_python_label_budget_cosine_recount",
            "script_sha256": digest(Path(__file__)), "new_store_report_sha256": report_sha, "old_store_report_sha256": OLD_REPORT_SHA,
            "new_encoding_report_sha256": report["encoding_report_sha256"], "new_vectors_sha256": report["vectors_sha256"],
            "old_vectors_sha256": OLD_VECTORS_SHA, "fixture_manifest_sha256": FIXTURE_SHA, "vector_comparison": vector_comparison,
            "new_condition_source_ids": sorted(new_sources), "evaluations": evaluations,
            "models_executed": False, "store_executed": False, "abstention_tested": False,
            "limits": "Subset42 is offline filtering of new48, not another Store run. RRF comparisons do not isolate text, lexical or actual-ID tie effects. No new quality gate or label revision."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--store-report", type=Path, required=True)
    parser.add_argument("--store-report-sha256", required=True)
    parser.add_argument("--encoding", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    require(not args.output.exists(), "Use a new audit output")
    result = audit(args.root.resolve(), args.store_report, args.store_report_sha256, args.encoding)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8", newline="\n") as handle:
        json.dump(result, handle, ensure_ascii=False, indent=2, allow_nan=False)
        handle.write("\n")
    print(json.dumps({"status": result["status"], "vector_comparison": result["vector_comparison"],
                      "evaluations": [{"condition": e["condition"], "summary": e["summary"], "paired": e["paired"]} for e in result["evaluations"]]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
