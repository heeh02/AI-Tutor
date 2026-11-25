"""Post-hoc diagnosis of frozen raw/typed rankings; no model or Store execution."""
import argparse
import hashlib
import json
import math
import pathlib
import struct

INPUTS = {
    "store": ("runtime/goal-baselines/20261104/typed-store-first/report.json", "ff9d876a315074cc5c1bd7bcd8b277719a37f6df9968cae217703640d87a38ad"),
    "raw_report": ("runtime/goal-baselines/20261104/bge-challenge-first/report.json", "b614aa9fcad3d12fb0ff6f4f3319eb0b73fbceac903a5aa967f7b76442a09cae"),
    "raw_vectors": ("runtime/goal-baselines/20261104/bge-challenge-first/bge_m3-encodings.json", "a8fd7529581098579f28941d57099ed59b1de5f4bec4ecda2a9b89bf0839ed24"),
    "typed_vectors": ("runtime/goal-baselines/20261104/typed-encode-first/bge_m3-encodings.json", "925fffa9bb29737fcaa6014edb42a55a010c38d7276859e2981aa578a37ea6f2"),
    "projection": ("runtime/goal-baselines/20261104/typed-projection-first/report.json", "5feb84ddc072b05a8bc9106082bfec096f8f5ec3208403f0ca9c018cca19e426"),
}


def load(root, name):
    path, expected = INPUTS[name]
    data = (root / path).read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"frozen input mismatch: {name}")
    return json.loads(data)


def vectors(rows):
    result = {}
    for row in rows:
        key = row["key"]
        value = row["encoding"]["pooled_vector"]
        if key in result or len(value) != 1024 or not all(map(math.isfinite, value)):
            raise ValueError("invalid frozen vector")
        digest = hashlib.sha256(struct.pack("<1024f", *value)).hexdigest()
        if digest != row["encoding"]["pooled_sha256_f32le"]:
            raise ValueError("vector bytes mismatch")
        result[key] = row
    return result


def select(ranking, texts):
    selected, skipped, total = [], [], 0
    for row in ranking:
        sid = row["source_id"]
        size = len(texts[sid])
        if total + size > 2400:
            skipped.append(sid)
            continue
        selected.append(sid)
        total += size
        if len(selected) == 5:
            break
    return {"source_ids": selected, "whole_chars": total, "skipped_for_budget": skipped}


def stage(ranking, texts, alternatives):
    selection = select(ranking, texts)
    picked = set(selection["source_ids"])
    ranks = {row["source_id"]: i + 1 for i, row in enumerate(ranking)}
    return {
        "selection": selection,
        "complete_evidence_at_5": bool(alternatives) and any(set(a) <= picked for a in alternatives),
        "evidence_ranks": [{sid: ranks.get(sid) for sid in a} for a in alternatives],
        "ranking": ranking,
    }


def summarize(cases, name):
    positive = [c for c in cases if c["alternatives"]]
    available = [c for c in positive if c["complete_set_available"]]
    negative = [c for c in cases if not c["alternatives"]]
    return {
        "original_positive_denominator": len(positive),
        "available_positive_denominator": len(available),
        "complete_original": sum(c[name]["complete_evidence_at_5"] for c in positive),
        "complete_available": sum(c[name]["complete_evidence_at_5"] for c in available),
        "available_failed_ids": [c["query_id"] for c in available if not c[name]["complete_evidence_at_5"]],
        "negative_candidates": sum(bool(c[name]["selection"]["source_ids"]) for c in negative),
        "negative_denominator": len(negative),
        "abstention_tested": False,
    }


def run(root):
    store, raw, projection = (load(root, n) for n in ("store", "raw_report", "projection"))
    rv, tv = (vectors(load(root, n)) for n in ("raw_vectors", "typed_vectors"))
    raw_texts = {d["original_source"]["id"]: d["original_source"]["text"] for d in projection["projection"]["decisions"]}
    typed_texts = {b["source_id"]: b["memory_evidence"]["memory"]["content"] for b in store["reconstructed_bindings"]}
    assert len(raw_texts) == 60 and len(typed_texts) == 42
    query_keys = [k for k in tv if not k.startswith("source:")]
    assert len(query_keys) == 192
    for key in query_keys:
        assert rv[key]["text_sha256"] == tv[key]["text_sha256"]
        assert rv[key]["inputs"] == tv[key]["inputs"]
        assert rv[key]["encoding"]["pooled_vector"] == tv[key]["encoding"]["pooled_vector"]
    prefixes = []
    for sid, text in typed_texts.items():
        original = raw_texts[sid]
        assert text.endswith(original)
        assert hashlib.sha256(original.encode()).hexdigest() == rv[f"source:{sid}"]["text_sha256"]
        assert hashlib.sha256(text.encode()).hexdigest() == tv[f"source:{sid}"]["text_sha256"]
        prefixes.append({"source_id": sid, "prefix": text[:-len(original)], "original_chars": len(original), "typed_chars": len(text)})
    evaluations = []
    for evaluation in store["evaluations"]:
        condition = evaluation["condition"]
        raw_cases = {c["query_id"]: c for c in raw["results"]["bge_m3"][condition]["cases"]}
        cases = []
        for case in evaluation["cases"]:
            original = raw_cases[case["query_id"]]
            alternatives = case["original_sufficient_evidence_sets"]
            assert alternatives == original["sufficient_evidence_sets"]
            raw60 = [{"source_id": r["id"], "score": r["score"]} for r in original["ranking"]]
            raw42 = [r for r in raw60 if r["source_id"] in typed_texts]
            typed42 = [{"source_id": r["source_id"], "score": r["score"]} for r in case["pure_eligible_cosine"]["ranking"]]
            assert len(raw60) == 60 and len(raw42) == len(typed42) == 42
            row = {"query_id": case["query_id"], "mode": case["mode"], "alternatives": alternatives, "complete_set_available": case["complete_set_available"]}
            for name, ranking, texts in (("raw60", raw60, raw_texts), ("raw42", raw42, raw_texts), ("typed42", typed42, typed_texts)):
                row[name] = stage(ranking, texts, alternatives)
                row[name]["adjacent_equal_scores"] = sum(a["score"] == b["score"] for a, b in zip(ranking, ranking[1:]))
            assert row["raw60"]["selection"]["source_ids"] == original["selected"]["ids"]
            assert row["typed42"]["selection"] == case["pure_eligible_cosine"]["selection"]
            assert row["typed42"]["complete_evidence_at_5"] == case["pure_eligible_cosine"]["metrics"].get("complete_evidence_at_5", False)
            cases.append(row)
        changes = {}
        for before, after in (("raw60", "raw42"), ("raw42", "typed42")):
            changes[f"{before}_to_{after}"] = {
                "regressions": [c["query_id"] for c in cases if c[before]["complete_evidence_at_5"] and not c[after]["complete_evidence_at_5"]],
                "gains": [c["query_id"] for c in cases if not c[before]["complete_evidence_at_5"] and c[after]["complete_evidence_at_5"]],
            }
        evaluations.append({"condition": condition, "summary": {n: summarize(cases, n) for n in ("raw60", "raw42", "typed42")}, "changes": changes, "cases": cases})
    return {
        "status": "completed", "analysis": "post_hoc_frozen_ranking_subset_and_text_prefix_diagnosis",
        "script_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
        "inputs": {k: {"path": p, "sha256": h} for k, (p, h) in INPUTS.items()},
        "method": {
            "ranking_source": "reuse frozen full cosine rankings; raw42 is stable filter of raw60; no new encoding or reranking",
            "query_control": "all 192 text hashes, token inputs and pooled vector values exactly equal across frozen runs",
            "pool_control": "raw60 to raw42 changes eligible source pool; raw42 to typed42 holds source IDs and query vectors fixed",
            "text_control": "all 42 typed texts equal an identified Chinese prefix plus unchanged full source text",
            "budget": "top5 / 2400 Unicode characters; whole-source skipping, no truncation",
            "ties": "raw uses frozen source-ID order, typed uses actual memory-ID order; equal-score adjacency counted, not silently unified",
            "limitations": "post-hoc explanation, not independent evaluation, new Store run, gate, or proof that all production text prefixes have the same effect",
        },
        "query_vectors_exact_equal_count": 192, "prefixes": prefixes, "evaluations": evaluations,
        "models_executed": False, "store_executed": False, "production_changed": False,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    report = run(args.root.resolve())
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8", newline="\n") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    print(json.dumps({"status": report["status"], "evaluations": [{"condition": e["condition"], "summary": e["summary"], "changes": e["changes"]} for e in report["evaluations"]]}, ensure_ascii=False))


if __name__ == "__main__":
    main()
