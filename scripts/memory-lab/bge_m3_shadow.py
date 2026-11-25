"""Offline native BGE-M3/E5 challenge comparison; requires reviewed v2 labels.

Prepare token inputs and the original Rust lexical adapter request first. Only
--run-model evaluates encoders, using sequential CPU models and no user database.
"""
import argparse
from collections import Counter
import gc
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import sys
import time

import download_bge_m3 as assets
import e5_python_reference as e5
import check_diagnostic_postfreeze as checker

for key in ("HF_HUB_OFFLINE", "TRANSFORMERS_OFFLINE", "HF_DATASETS_OFFLINE", "HF_HUB_DISABLE_TELEMETRY"):
    os.environ[key] = "1"
os.environ.update(USE_TORCH="1", USE_TF="0", CUDA_VISIBLE_DEVICES="", TOKENIZERS_PARALLELISM="false")
METHOD_SHA = "2f7abcf3f30ae3c31f62730f59e3dc792aedcadeac570585d1e8f0be2085bf17"
CONDITIONS = ("current_query", "query_plus_goal")
MODELS = ("e5_fp32", "bge_m3")
require, sha_file, load_json = e5.require, e5.sha_file, e5.load_json


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as output:
        json.dump(data, output, ensure_ascii=False, indent=2, allow_nan=False)
        output.write("\n")


def memory_snapshot():
    status = Path("/proc/self/status")
    if not status.is_file():
        return {"available": False, "source": "Linux /proc/self/status unavailable"}
    fields = {}
    for line in status.read_text(encoding="ascii").splitlines():
        name, _, value = line.partition(":")
        if name in ("VmRSS", "VmHWM"):
            parts = value.split()
            require(len(parts) == 2 and parts[1] == "kB", "Unexpected proc memory unit")
            fields[name + "_bytes"] = int(parts[0]) * 1024
    return {"available": len(fields) == 2, "source": "/proc/self/status", **fields,
            "meaning": "RSS is this whole process; VmHWM is cumulative process peak, not exclusive per-model peak"}


def query_text(query, condition):
    return query["text"] if condition == "current_query" else query["text"] + " " + query["goal"] + " "


def checked_fixture(directory, expected_sha):
    require(sha_file(directory / "manifest.json") == expected_sha, "Reviewed manifest SHA mismatch")
    manifest = load_json(directory / "manifest.json")
    require(manifest["fixture_version"] == "education_memory_diagnostic_postfreeze_v2_20261104",
            "Only reviewed v2 is allowed; do not run retrieval on superseded v1")
    require(manifest["synthetic_only"] is True and manifest["retrieval_run"] is False, "Unexpected fixture provenance")
    data = {}
    for name in ("sources", "queries", "labels", "scope_controls"):
        file = directory / (name + ".jsonl")
        require(sha_file(file) == manifest["files"][file.name]["sha256"], "Frozen fixture content changed: " + name)
        data[name] = checker.load_jsonl(file)
    # The v2 checker is provided with reviewed errata; no label repair is done here.
    require((len(data["sources"]), len(data["queries"]), len(data["labels"]), len(data["scope_controls"])) == (60, 96, 96, 12), "Incomplete challenge")
    checker.check_records(data["sources"], data["queries"], data["labels"], data["scope_controls"], version=manifest["fixture_version"])
    sources = {row["id"]: row for row in data["sources"]}
    for query in data["queries"]:
        require(set(query["candidate_pool_ids"]) == set(sources), "Topic/language prefilter forbidden")
        require(all(checker.eligible(source, query["context"]) for source in sources.values()), "Source scope mismatch")
    data["manifest"] = manifest
    return data


def checked_models(root, method):
    bge_dir = root / "models/memory/bge-m3"
    bge_hashes = {asset[0]: assets.verify(bge_dir / asset[0], asset)["sha256"] for asset in assets.ASSETS}
    e5_dir = root / "models/memory/e5-small"
    e5_expected = {"tokenizer.json": e5.TOKENIZER_SHA, **e5.LOCAL_CONFIG_HASHES,
                   "model.safetensors": method["models"]["e5_fp32"]["weight_sha256"]}
    for name, expected in e5_expected.items():
        require(sha_file(e5_dir / name) == expected, "Pinned E5 asset changed: " + name)
    return {"bge_m3": bge_dir, "e5_fp32": e5_dir}, {"bge_m3": bge_hashes, "e5_fp32": e5_expected}


def tokenize(data, directory, model_spec):
    from transformers import AutoTokenizer
    tokenizer = AutoTokenizer.from_pretrained(str(directory), local_files_only=True,
                                              trust_remote_code=False, use_fast=True)
    require(tokenizer.is_fast, "No unrecorded tokenizer fallback")
    inputs = [("source:" + row["id"], model_spec["source_prefix"] + row["text"]) for row in data["sources"]]
    inputs.extend((condition + ":" + row["id"], model_spec["query_prefix"] + query_text(row, condition))
                  for condition in CONDITIONS for row in data["queries"])
    rows = []
    for key, text in inputs:
        start = time.perf_counter()
        encoded = tokenizer(text, add_special_tokens=True, padding=False, truncation=False,
                            return_attention_mask=True, return_token_type_ids=False)
        elapsed_ms = (time.perf_counter() - start) * 1000
        ids = encoded["input_ids"]
        require(1 <= len(ids) <= 512, "Overlong input; no silent truncation: " + key)
        rows.append({"key": key, "text_sha256": hashlib.sha256(text.encode()).hexdigest(),
                     "inputs": {name: encoded[name] for name in ("input_ids", "attention_mask")},
                     "token_count": len(ids), "first_token_ids": ids[:4], "last_token_ids": ids[-4:],
                     "tokenization_ms": elapsed_ms})
    return rows


def select_whole(ranking, sources, limit=5, budget=2400):
    selected, used, skipped = [], 0, []
    for row in ranking:
        if len(selected) == limit:
            break
        size = len(sources[row["id"]]["text"])
        if used + size <= budget:
            selected.append(row["id"])
            used += size
        else:
            skipped.append(row["id"])
    return {"ids": selected, "chars": used, "skipped_for_budget": skipped}


def metrics(ids, sufficient_sets):
    if not sufficient_sets:
        return {"answerable": False, "candidates_returned": len(ids), "abstention": "not_implemented"}
    sets = [set(values) for values in sufficient_sets]
    relevant = set.union(*sets)
    first = next((i + 1 for i, key in enumerate(ids[:5]) if key in relevant), None)
    complete = next((k for k in range(1, min(5, len(ids)) + 1)
                     if any(values.issubset(ids[:k]) for values in sets)), None)
    result = {"answerable": True, "hit_at_1": bool(set(ids[:1]) & relevant),
              "hit_at_3": bool(set(ids[:3]) & relevant), "reciprocal_rank_at_5": 1 / first if first else 0.0,
              "first_complete_evidence_reciprocal_rank_at_5": 1 / complete if complete else 0.0}
    for k in (3, 5):
        result["complete_evidence_at_" + str(k)] = any(values.issubset(ids[:k]) for values in sets)
        result["best_valid_set_coverage_at_" + str(k)] = max(len(values.intersection(ids[:k])) / len(values) for values in sets)
    return result


def summarize(cases):
    positive = [case for case in cases if case["metrics"]["answerable"]]
    negative = [case for case in cases if not case["metrics"]["answerable"]]
    totals = {"cases": len(cases), "positive": len(positive), "negative": len(negative),
              "negative_cases_with_candidates": sum(bool(case["selected"]["ids"]) for case in negative)}
    for name in ("hit_at_1", "hit_at_3", "complete_evidence_at_3", "complete_evidence_at_5"):
        totals[name] = sum(case["metrics"][name] for case in positive)
    for name in ("reciprocal_rank_at_5", "first_complete_evidence_reciprocal_rank_at_5",
                 "best_valid_set_coverage_at_3", "best_valid_set_coverage_at_5"):
        totals["mean_" + name] = sum(case["metrics"][name] for case in positive) / len(positive) if positive else None
    return totals


def evaluate_rankings(rankings, data):
    labels = {row["query_id"]: row for row in data["labels"]}
    sources = {row["id"]: row for row in data["sources"]}
    cases = []
    for query in data["queries"]:
        ranking = rankings[query["id"]]
        ids = [row["id"] for row in ranking]
        require(len(ids) == len(set(ids)) and set(ids).issubset(sources), "Invalid rank source IDs")
        selected = select_whole(ranking, sources)
        sets = labels[query["id"]]["sufficient_evidence_sets"]
        cases.append({"query_id": query["id"], "mode": query["mode"], "language": query["language"],
                      "pack_id": query["pack_id"], "tags": query["tags"], "ranking": ranking,
                      "sufficient_evidence_sets": sets, "selected": selected,
                      "goal_only_lexical_selected": (sum(row.get("goal_only_match", False) for row in ranking if row["id"] in selected["ids"])
                                                     if any("goal_only_match" in row for row in ranking) else None),
                      "metrics": metrics(selected["ids"], sets),
                      "full_ranking_top5_metrics": metrics(ids[:5], sets)})
    strata = {"overall": summarize(cases)}
    for dimension in ("mode", "language", "pack_id", "tags"):
        values = sorted({value for case in cases for value in (case[dimension] if dimension == "tags" else [case[dimension]])})
        strata[dimension] = {value: summarize([case for case in cases if value in case[dimension]])
                            if dimension == "tags" else summarize([case for case in cases if case[dimension] == value])
                            for value in values}
    return {"cases": cases, "strata": strata}


def read_lexical(path, request_path, method, fixture_sha):
    lexical = load_json(path)
    require(lexical["schema_version"] == "lexical-challenge-result-v1"
            and lexical["fixture_manifest_sha256"] == fixture_sha
            and lexical["request_sha256"] == sha_file(request_path), "Lexical input provenance mismatch")
    require(lexical["module_sha256"] == method["lexical"]["module_sha256"]
            and lexical["method_sha256"] == method["lexical"]["method_sha256"], "Frozen Rust lexical implementation changed")
    require(lexical["adapter_sha256"] == sha_file(Path(__file__).with_name("lexical_challenge_adapter.rs")), "Lexical adapter provenance mismatch")
    require(len(lexical["cases"]) == 192, "Missing lexical cases")
    result = {condition: {} for condition in CONDITIONS}
    for row in lexical["cases"]:
        require(row["condition"] in result and row["query_id"] not in result[row["condition"]], "Duplicate/invalid lexical case")
        result[row["condition"]][row["query_id"]] = row["ranking"]
    return result


def encode(directory, rows, model_spec, model_name):
    import numpy as np
    import torch
    from transformers import AutoModel
    from torch.nn import functional as functional
    before_load = memory_snapshot()
    start = time.perf_counter()
    model = AutoModel.from_pretrained(str(directory), local_files_only=True, trust_remote_code=False,
                                     weights_only=True, use_safetensors=model_name == "e5_fp32",
                                     dtype=torch.float32, attn_implementation="eager")
    model.to(device="cpu", dtype=torch.float32).eval()
    require(all(p.device.type == "cpu" and p.dtype == torch.float32 for p in model.parameters()), "CPU FP32 required")
    load_ms = (time.perf_counter() - start) * 1000
    after_load = memory_snapshot()
    vectors = {}
    with torch.inference_mode():
        for row in rows:
            feed = {name: torch.tensor([values], dtype=torch.long) for name, values in row["inputs"].items()}
            start = time.perf_counter()
            hidden = model(**feed).last_hidden_state
            require(tuple(hidden.shape) == (1, row["token_count"], model_spec["dimension"]), "Unexpected hidden shape")
            require(bool(torch.isfinite(hidden).all()), "Nonfinite hidden state")
            if model_name == "bge_m3":
                pooled = hidden[:, 0]
            else:
                masked = hidden.masked_fill(~feed["attention_mask"][..., None].bool(), 0.0)
                pooled = masked.sum(dim=1) / feed["attention_mask"].sum(dim=1)[..., None]
            require(bool((pooled.norm(dim=1) > 0).all()), "Zero pooled vector")
            vector = functional.normalize(pooled, p=2, dim=1)[0].cpu().numpy().copy()
            inference_ms = (time.perf_counter() - start) * 1000
            vectors[row["key"]] = vector
            row["encoding"] = {"pooled_vector": vector.tolist(), "pooled_sha256_f32le": hashlib.sha256(vector.astype("<f4").tobytes()).hexdigest(),
                               "hidden_shape": list(hidden.shape), "inference_ms": inference_ms}
    runtime = {"class": type(model).__name__, "parameter_count": sum(p.numel() for p in model.parameters()),
               "load_ms": load_ms, "inference_total_ms": sum(row["encoding"]["inference_ms"] for row in rows),
               "device": "cpu", "dtype": "float32", "intra_threads": torch.get_num_threads(),
               "inter_threads": torch.get_num_interop_threads(), "training": model.training,
               "memory": {"before_load": before_load, "after_load": after_load, "after_encoding": memory_snapshot()}}
    del model, hidden, pooled, feed
    if model_name == "e5_fp32":
        del masked
    gc.collect()
    runtime["memory"]["after_unload"] = memory_snapshot()
    return vectors, runtime


def dense_rankings(vectors, data, condition):
    import numpy as np
    result = {}
    for query in data["queries"]:
        query_vector = vectors[condition + ":" + query["id"]].astype(np.float64)
        ranking = [{"id": source["id"], "score": float(np.dot(query_vector, vectors["source:" + source["id"]].astype(np.float64))),
                    "relevance_status": "candidate_requires_relevance_review"} for source in data["sources"]]
        ranking.sort(key=lambda row: (-row["score"], row["id"]))
        result[query["id"]] = ranking
    return result


def comparisons(results):
    changes = []
    for condition in CONDITIONS:
        actual = {row["query_id"]: row for row in results["bge_m3"][condition]["cases"]}
        for baseline in ("e5_fp32", "lexical"):
            rows = []
            for before in results[baseline][condition]["cases"]:
                after = actual[before["query_id"]]
                if not before["metrics"]["answerable"]:
                    continue
                row = {"query_id": before["query_id"], "pack_id": before["pack_id"], "mode": before["mode"]}
                for metric in ("complete_evidence_at_5", "hit_at_1", "reciprocal_rank_at_5", "best_valid_set_coverage_at_5"):
                    row[metric + "_delta"] = float(after["metrics"][metric]) - float(before["metrics"][metric])
                row["before_selected"] = before["selected"]["ids"]
                row["after_selected"] = after["selected"]["ids"]
                rows.append(row)
            changes.append({"condition": condition, "baseline": baseline, "all_positive_queries": rows,
                            "complete_regressions": [row["query_id"] for row in rows if row["complete_evidence_at_5_delta"] < 0]})
    return changes


def goal_comparisons(results):
    compared = {}
    for name, conditions in results.items():
        current = {row["query_id"]: row for row in conditions["current_query"]["cases"]}
        rows = []
        for combined in conditions["query_plus_goal"]["cases"]:
            raw = current[combined["query_id"]]
            row = {"query_id": raw["query_id"], "mode": raw["mode"], "pack_id": raw["pack_id"],
                   "current_selected": raw["selected"]["ids"], "combined_selected": combined["selected"]["ids"],
                   "goal_only_lexical_selected": combined["goal_only_lexical_selected"]}
            if raw["metrics"]["answerable"]:
                row["complete_evidence_at_5_delta"] = int(combined["metrics"]["complete_evidence_at_5"]) - int(raw["metrics"]["complete_evidence_at_5"])
            else:
                row["negative_candidates_delta"] = len(combined["selected"]["ids"]) - len(raw["selected"]["ids"])
            rows.append(row)
        compared[name] = rows
    return compared


def quality_gates(results, scope_controls, deltas):
    scopes_ok = all(row["expected_eligible"] == row["actual_eligible"] for row in scope_controls)
    gates = {}
    def threshold(rows, minimum):
        positive = [row for row in rows if row["metrics"]["answerable"]]
        found = sum(row["metrics"]["complete_evidence_at_5"] for row in positive)
        return {"numerator": found, "denominator": len(positive), "minimum_count": minimum, "passed": found >= minimum}
    for name, conditions in results.items():
        gates[name] = {}
        for condition, evaluated in conditions.items():
            rows = evaluated["cases"]
            coverage = {
                "all_positive_complete_at_5": threshold(rows, 65),
                "single_cross_language_complete_at_5": threshold([row for row in rows if row["mode"] in ("zh_to_en", "en_to_zh")], 20),
                "multi_complete_at_5": threshold([row for row in rows if row["mode"] == "mixed_multi"], 10),
            }
            negative = [row for row in rows if not row["metrics"]["answerable"]]
            gate = {"coverage": coverage,
                    "scope_controls": {"count": len(scope_controls), "leaks": sum(row["actual_eligible"] and not row["expected_eligible"] for row in scope_controls),
                                       "allowed_leaks": 0, "passed": scopes_ok, "meaning": "logical fixture filter, not model authorization or Store integration"},
                    "negative_review": {"count": len(negative), "with_candidates": sum(bool(row["selected"]["ids"]) for row in negative),
                                        "candidate_status": "candidate_requires_relevance_review", "abstention_tested": False,
                                        "automatic_acceptance_tested": False, "negative_quality_pass": None},
                    "coverage_and_scope_passed": scopes_ok and all(item["passed"] for item in coverage.values()),
                    "deployment_allowed": False,
                    "meaning": "passing can only support a next opt-in candidate evaluation; never answer correctness or default deployment"}
            if name == "bge_m3":
                checks = {}
                for comparison in deltas:
                    if comparison["condition"] != condition:
                        continue
                    compared = comparison["all_positive_queries"]
                    net = int(sum(row["complete_evidence_at_5_delta"] for row in compared))
                    topics = sorted({row["pack_id"] for row in compared})
                    losses = {topic: sum(row["pack_id"] == topic and row["complete_evidence_at_5_delta"] < 0 for row in compared) for topic in topics}
                    checks[comparison["baseline"]] = {"complete_count_delta": net, "minimum_delta": 0,
                                                       "topic_lost_previously_complete_queries": losses, "maximum_losses_per_topic": 1,
                                                       "passed": net >= 0 and all(count <= 1 for count in losses.values())}
                gate["baseline_non_regression"] = checks
                gate["next_opt_in_candidate_gate_passed"] = gate["coverage_and_scope_passed"] and len(checks) == 2 and all(row["passed"] for row in checks.values())
            gates[name][condition] = gate
    return gates


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--fixture-manifest-sha", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prepared", type=Path)
    parser.add_argument("--lexical-report", type=Path)
    parser.add_argument("--load-note", default="not recorded; not a latency baseline")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prepare-only", action="store_true")
    mode.add_argument("--run-model", action="store_true")
    args = parser.parse_args()
    require(not args.output.exists(), "Use a new output directory")
    root = args.root.resolve()
    method_file = Path(__file__).with_name("bge_m3_shadow_method_v1.json")
    require(sha_file(method_file) == METHOD_SHA, "Frozen method changed")
    method = load_json(method_file)
    data = checked_fixture(args.fixture, args.fixture_manifest_sha)
    directories, hashes = checked_models(root, method)
    args.output.mkdir(parents=True)
    report = {"probe": method["method_version"], "method_sha256": METHOD_SHA,
              "fixture_manifest_sha256": args.fixture_manifest_sha, "fixture_version": data["manifest"]["fixture_version"],
              "script_sha256": sha_file(Path(__file__)), "checker_sha256": sha_file(Path(checker.__file__)),
              "model_assets": hashes, "status": "preparing", "model_executed": False,
              "dependencies": {name: importlib.metadata.version(name) for name in ("torch", "transformers", "safetensors", "numpy", "tokenizers")},
              "python": sys.version, "platform": platform.platform(), "load_note": args.load_note,
              "scope_controls": [{"id": row["id"], "kind": row["control_kind"],
                                  "expected_eligible": row["expected_eligible"], "actual_eligible": checker.eligible(row["source"], row["context"])} for row in data["scope_controls"]],
              "scope_control_meaning": "logical source filter only; not vector permission or Store integration",
              "mode_counts": dict(Counter(row["mode"] for row in data["queries"])),
              "network_used": False, "devices_used": False, "packages_installed": False,
              "learner_db_opened": False, "production_recall_changed": False}
    require(all(row["expected_eligible"] == row["actual_eligible"] for row in report["scope_controls"]), "Scope control failure")
    request = {"schema_version": "lexical-challenge-request-v1", "fixture_manifest_sha256": args.fixture_manifest_sha,
               "sources": [{"id": row["id"], "text": row["text"]} for row in data["sources"]],
               "queries": [{key: row[key] for key in ("id", "text", "goal", "candidate_pool_ids")} for row in data["queries"]]}
    request_path = args.output / "lexical-request.json"
    write_json(request_path, request)
    report["lexical_request_sha256"] = sha_file(request_path)
    rows = {name: tokenize(data, directories[name], method["models"][name]) for name in MODELS}
    for name in MODELS:
        write_json(args.output / (name + "-token-inputs.json"), rows[name])
    report["inputs_per_model"] = {name: len(values) for name, values in rows.items()}
    if args.prepare_only:
        report["status"] = "prepared_no_retrieval"
        write_json(args.output / "report.json", report)
        print(json.dumps({"status": report["status"], "model_executed": False}))
        return
    require(args.prepared is not None and args.lexical_report is not None, "Run requires prepared report directory and actual Rust lexical report")
    prepared = load_json(args.prepared / "report.json")
    for field in ("method_sha256", "fixture_manifest_sha256", "script_sha256", "checker_sha256", "model_assets", "dependencies", "lexical_request_sha256"):
        require(prepared[field] == report[field], "Preparation provenance changed: " + field)
    require(prepared["status"] == "prepared_no_retrieval", "Expected prior preparation")
    for name in MODELS:
        old = load_json(args.prepared / (name + "-token-inputs.json"))
        require([(row["key"], row["text_sha256"], row["inputs"]) for row in old]
                == [(row["key"], row["text_sha256"], row["inputs"]) for row in rows[name]], "Token inputs changed")
    lexical = read_lexical(args.lexical_report, request_path, method, args.fixture_manifest_sha)
    results = {"lexical": {condition: evaluate_rankings(lexical[condition], data) for condition in CONDITIONS}}
    report["lexical_report_sha256"] = sha_file(args.lexical_report)
    report["model_executed"] = None
    try:
        import torch
        torch.set_num_threads(2)
        torch.set_num_interop_threads(1)
        report["memory_before_models"] = memory_snapshot()
        report["runtime"] = {}
        for name in MODELS:
            vectors, runtime = encode(directories[name], rows[name], method["models"][name], name)
            report["runtime"][name] = runtime
            results[name] = {condition: evaluate_rankings(dense_rankings(vectors, data, condition), data) for condition in CONDITIONS}
            write_json(args.output / (name + "-encodings.json"), rows[name])
            del vectors
            gc.collect()
            print(json.dumps({"model_completed": name, "inputs": len(rows[name])}), flush=True)
        deltas = comparisons(results)
        report.update(status="completed", model_executed=True, results=results,
                      comparisons=deltas, goal_comparisons=goal_comparisons(results),
                      quality_gate=quality_gates(results, report["scope_controls"], deltas),
                      memory_after_models=memory_snapshot())
    except Exception as error:
        report.update(status="failed", error_type=type(error).__name__, error=str(error)[:1000], results=results)
        raise
    finally:
        write_json(args.output / "report.json", report)
    print(json.dumps({"status": report["status"], "model_executed": report["model_executed"]}))


if __name__ == "__main__":
    main()
