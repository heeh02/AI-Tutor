"""Validate exact typed48 Store outputs; explicit --run-model performs CPU encoding only."""
import argparse
from collections import Counter
import hashlib
import importlib
import importlib.metadata
import json
import math
from pathlib import Path
import platform
import re
import struct
import sys
import time

HERE = Path(__file__).resolve().parent
METHOD_SHA = "025115dcbcd7bac73e3c68d0811dfa85c14742de8d6c6a3a4f296c23c180903a"
UTILITY_SHA = "707ef0cbb2512723b99c751e8df619856cf382b429559d500b92501e765625e5"
if hashlib.sha256((HERE / "typed_projection_encode.py").read_bytes()).hexdigest() != UTILITY_SHA:
    raise ValueError("Frozen encoding utility changed")
utility = importlib.import_module("typed_projection_encode")
require = utility.require
PREFIXES = {**utility.PREFIXES, "context_constraint": "特定任务的条件与推断边界"}


def projection_kind(source):
    if (source["claim_type"], source["event_kind"], source["support_level"]) == ("condition_limit", "learner_statement", "unknown"):
        return "context_constraint"
    return utility.projection_kind(source)


def validate_payload(report, inputs, data, method, fixture_files):
    require(report["status"] == "projection_completed_no_encoding", "Projection not completed")
    for actual, expected in (("method_sha256", "projection_method_sha256"), ("probe_sha256", "projection_probe_sha256"),
                             ("common_sha256", "projection_common_sha256"), ("fixture_manifest_sha256", "fixture_manifest_sha256")):
        require(report[actual] == method[expected], "Projection source provenance changed")
    require(report["original_fixture_files"] == fixture_files, "Fixture binding changed")
    require(all(report[k] is False for k in ("models_executed", "ranking_executed", "production_changed", "devices_used")), "Unexpected side effects")
    p = report["projection"]
    require((p["projected_count"], p["excluded_count"], p["live_memory_evidence_rechecked"]) == (48, 12, 48), "Incomplete live projection")
    require(p["encoder_profile"] == method["profile"], "Profile changed")
    require(p["persistent_preference_coverage"] == p["measured_hint_effectiveness_coverage"] == 0, "Invented coverage")
    require(p["embedding_work_first_batch_count"] == 32 and p["embedding_work_total_coverage"] is False, "Projection pretends all vectors indexed")
    require(len(p["sessions"]) == 6 and all(s["progress_state_unchanged"] is True for s in p["sessions"]), "Teaching state changed")
    require(len(p["jobs"]) == 12 and [j["candidate_count"] for j in p["jobs"]] == [5, 3] * 6
            and all(j["status"] == "applied" and j["model_call_started"] is False for j in p["jobs"]), "Wrong synthetic jobs")
    decisions = p["decisions"]
    require(len(decisions) == len(data["sources"]) == 60 and len(data["queries"]) == len(data["labels"]) == 96
            and len(data["scope_controls"]) == 12, "Incomplete original corpus")
    expected, sources, owner_ids, memory_ids = [], [], set(), set()
    kinds, exclusions = Counter(), Counter()
    for decision, source in zip(decisions, data["sources"]):
        require(decision["source_id"] == source["id"] and decision["original_source"] == source
                and decision["original_text_sha256"] == utility.sha_bytes(source["text"].encode()), "Original source rewritten or reordered")
        kind = projection_kind(source)
        require(decision["projection_kind"] == kind and decision["projected"] is (kind is not None), "Wrong projection mapping")
        if kind is None:
            reason = {"unaccepted_plan": "proposal_not_accepted", "temporary_request": "temporary_request_not_persistent_preference"}.get(source["claim_type"])
            require(reason is not None and decision["exclusion_reason"] == reason
                    and "encoding_text" not in decision and "memory_id" not in decision, "Excluded source gained memory")
            exclusions[reason] += 1
            continue
        evidence = decision["memory_evidence"]
        memory, metadata, event = evidence["memory"], evidence["education_metadata"], decision["live_imported_event"]
        text = PREFIXES[kind] + "：" + source["text"]
        memory_kind = {"trial_observation": "reported_progress", "accepted_pending_experiment": "pending_experiment"}.get(kind, kind)
        require(decision["encoding_text"] == memory["content"] == text
                and decision["encoding_text_sha256"] == utility.sha_bytes(text.encode()), "Actual memory text or hash changed")
        require(memory["memory_id"] == decision["memory_id"] and memory["kind"] == memory_kind
                and memory["source_event_ids"] == [decision["event_id"]] and memory["source_type"] == "simulated"
                and memory["scope"] == "current_activity" and memory["activity_scope"] == "modular_rover"
                and memory["epoch"] == 0 and memory["status"] == "active" and evidence["namespace"] == "simulator", "Memory identity/scope changed")
        require(metadata == {"kind": kind, "claim_type": source["claim_type"], "support_level": source["support_level"],
                             "importance": dict.fromkeys(("goal_relevance", "continuity", "novel_change", "explicit_request"), 0),
                             "source_grade": "simulated", "source_count": 1}, "Typed metadata changed")
        content = {"text": source["text"], "support_level": source["support_level"], "synthetic_fixture": True, "fixture_source_id": source["id"]}
        if source["accepted"] is not None:
            content["accepted"] = source["accepted"]
        require(event["content"] == content and event["event_id"] == decision["event_id"]
                and event["event_kind"] == source["event_kind"] and event["event_version"] == source["version"] == 1
                and event["event_status"] == "active" and event["source_type"] == "simulated"
                and event["learner_id"] == memory["learner_id"], "Imported source changed")
        require(evidence["sources"] == [event] and evidence["source_links"] == [{"event_id": event["event_id"], "event_version": 1}], "Source links changed")
        owner_ids.add(memory["learner_id"])
        memory_ids.add(memory["memory_id"])
        expected.append({"key": "source:" + source["id"], "input_kind": "projected_memory", "source_ids": [source["id"]],
                         "text": text, "text_sha256": decision["encoding_text_sha256"]})
        sources.append({"id": source["id"], "text": text})
        kinds[kind] += 1
    require(len(owner_ids) == 1 and len(memory_ids) == len(sources) == 48 and kinds["context_constraint"] == 6, "Memory count or owner changed")
    require(dict(kinds) == p["kind_counts"] and dict(exclusions) == p["exclusion_counts"], "Counts changed")
    for condition in utility.CONDITIONS:
        for query in data["queries"]:
            text = query["text"] if condition == "current_query" else query["text"] + " " + query["goal"] + " "
            expected.append({"key": condition + ":" + query["id"], "input_kind": condition, "query_id": query["id"],
                             "text": text, "text_sha256": utility.sha_bytes(text.encode())})
    require(len(inputs) == 240 and inputs == expected, "Exact input order/content/count changed")
    return {"sources": sources, "queries": data["queries"]}


def checked_projection(root, directory, report_sha, method):
    require(re.fullmatch(r"[0-9a-f]{64}", report_sha) is not None, "Explicit lowercase report SHA required")
    report = utility.checked_json(directory / "report.json", report_sha)
    projection_method = utility.checked_json(HERE / "typed48_projection_method_v1.json", method["projection_method_sha256"])
    require(report["method"] == projection_method, "Embedded method differs")
    fixture = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2"
    manifest = utility.checked_json(fixture / "manifest.json", method["fixture_manifest_sha256"])
    data = {name: utility.read_jsonl(fixture / (name + ".jsonl"), manifest["files"][name + ".jsonl"]["sha256"])
            for name in ("sources", "queries", "labels", "scope_controls")}
    require(report["encoder_inputs"]["file"] == "encoder_inputs.jsonl" and report["encoder_inputs"]["records"] == 240, "Wrong encoder artifact")
    inputs = utility.read_jsonl(directory / "encoder_inputs.jsonl", report["encoder_inputs"]["sha256"])
    adapted = validate_payload(report, inputs, data, method, manifest["files"])
    require(report["baseline42_report_sha256"] == projection_method["baseline42_report_sha256"]
            and report["baseline234_inputs_sha256"] == projection_method["baseline234_inputs_sha256"], "Baseline binding changed")
    legacy = utility.read_jsonl(root / "runtime/goal-baselines/20261104/typed-projection-first/encoder_inputs.jsonl", report["baseline234_inputs_sha256"])
    indexed = {row["key"]: row for row in inputs}
    require(len(legacy) == 234 and all(indexed.get(row["key"]) == row for row in legacy), "Old 42 texts or 192 queries changed")
    return report, inputs, adapted


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--projection", type=Path, required=True)
    parser.add_argument("--projection-report-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--validate-only", action="store_true")
    mode.add_argument("--run-model", action="store_true")
    parser.add_argument("--load-note", default="not recorded; not a latency baseline")
    args = parser.parse_args()
    require(not args.output.exists(), "Use a new output directory")
    method = utility.checked_json(HERE / "typed48_encode_method_v1.json", METHOD_SHA)
    require(method["utility_sha256"] == UTILITY_SHA, "Utility binding changed")
    started = time.perf_counter()
    projection, inputs, adapted = checked_projection(args.root.resolve(), args.projection.resolve(), args.projection_report_sha256, method)
    helper = utility.load_helper(method)
    args.output.mkdir(parents=True, exist_ok=False)
    result = {"status": "validated_no_encoding", "method": method, "method_sha256": METHOD_SHA,
              "script_sha256": utility.sha_file(Path(__file__)), "projection_report_sha256": args.projection_report_sha256,
              "projection_inputs_sha256": projection["encoder_inputs"]["sha256"], "profile": method["profile"],
              "fixture_manifest_sha256": method["fixture_manifest_sha256"], "input_count": len(inputs),
              "projected_source_ids": [s["id"] for s in adapted["sources"]], "model_executed": False,
              "ranking_executed": False, "store_vector_imported": False, "learner_db_opened": False,
              "devices_used": False, "network_used": False, "packages_installed": False, "production_changed": False,
              "load_note": args.load_note, "python": sys.version, "platform": platform.platform()}
    try:
        if args.run_model:
            require(sys.platform == "linux", "Frozen model runtime is existing Linux/WSL CPU")
            directory = args.root.resolve() / "models/memory/bge-m3"
            before = time.perf_counter()
            result["assets"] = [helper.assets.verify(directory / asset[0], asset) for asset in helper.assets.ASSETS]
            result["asset_verification_ms"] = (time.perf_counter() - before) * 1000
            require(helper.assets.REVISION == method["profile"]["revision"], "Model revision changed")
            spec = json.loads((HERE / "bge_m3_shadow_method_v1.json").read_text("utf-8"))["models"]["bge_m3"]
            result["dependencies"] = {name: importlib.metadata.version(name) for name in ("torch", "transformers", "tokenizers", "numpy")}
            import torch
            torch.set_num_threads(2)
            torch.set_num_interop_threads(1)
            require(torch.get_num_threads() == 2 and torch.get_num_interop_threads() == 1, "Wrong thread configuration")
            result["memory_before_tokenizer"] = helper.memory_snapshot()
            tokens = helper.tokenize(adapted, directory, spec)
            require([(r["key"], r["text_sha256"]) for r in tokens] == [(r["key"], r["text_sha256"]) for r in inputs], "Helper rewrote inputs")
            utility.write_json(args.output / "token_inputs.json", tokens)
            result["token_inputs_sha256"] = utility.sha_file(args.output / "token_inputs.json")
            result["token_count_range"] = [min(r["token_count"] for r in tokens), max(r["token_count"] for r in tokens)]
            result["tokenization_total_ms"] = sum(r["tokenization_ms"] for r in tokens)
            result.update(model_execution_attempted=True, model_executed=None)
            vectors, runtime = helper.encode(directory, tokens, spec, "bge_m3")
            require(len(vectors) == len(tokens) == 240, "Incomplete encoded vectors")
            for row in tokens:
                vector = row["encoding"]["pooled_vector"]
                require(len(vector) == 1024 and all(map(math.isfinite, vector)) and abs(sum(v*v for v in vector)-1) <= .001, "Invalid BGE vector")
                require(utility.sha_bytes(struct.pack("<1024f", *vector)) == row["encoding"]["pooled_sha256_f32le"], "FP32 hash mismatch")
            utility.write_json(args.output / "bge_m3-encodings.json", tokens)
            result.update(status="encoding_completed_no_retrieval", model_executed=True, runtime=runtime,
                          encoded_count=len(tokens), encodings_sha256=utility.sha_file(args.output / "bge_m3-encodings.json"))
            result["memory_at_end"] = helper.memory_snapshot()
            result["memory_summary"] = utility.memory_summary(runtime, result["memory_at_end"])
            result["timing_scope"] = method["timing_scope"]
    except Exception as error:
        result.update(status="failed", error_type=type(error).__name__, error=str(error)[:1000])
        raise
    finally:
        result["elapsed_seconds"] = time.perf_counter() - started
        utility.write_json(args.output / "report.json", result)
    print(json.dumps({"status": result["status"], "inputs": 240, "model_executed": result["model_executed"], "store_vector_imported": False}), flush=True)


if __name__ == "__main__":
    main()
