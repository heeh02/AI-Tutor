"""Encode exact real-Store projection inputs with the existing fixed BGE helper.

Default validation reads only synthetic JSON/source files. Explicit --run-model
loads the existing local BGE weights on CPU; neither mode performs retrieval.
"""
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

METHOD_SHA = "18404e264f84bb6576e5dd88fc4b5431134b3608e05f717024808290e7f64689"
SCRIPT_DIR = Path(__file__).resolve().parent
CONDITIONS = ("current_query", "query_plus_goal")
PREFIXES = {
    "trial_observation": "据报告的尝试与观察",
    "concept_evidence": "本次回答证据，不代表已掌握",
    "accepted_pending_experiment": "已接受、尚待完成的实验",
    "correction": "明确更正",
    "learning_goal": "明确表达的学习目标",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def checked_json(path, expected):
    require(path.stat().st_size < 8 * 1024 * 1024, "JSON input exceeds bound")
    data = path.read_bytes()
    require(sha_bytes(data) == expected, "Frozen file changed: " + path.name)
    return json.loads(data)


def read_jsonl(path, expected):
    require(path.stat().st_size < 8 * 1024 * 1024, "JSONL input exceeds bound")
    data = path.read_bytes()
    require(sha_bytes(data) == expected, "Frozen JSONL changed: " + path.name)
    return [json.loads(line) for line in data.decode("utf-8").splitlines()]


def projection_kind(source):
    pair = (source["claim_type"], source["event_kind"])
    return {
        ("reported_observation", "observation"): "trial_observation",
        ("task_specific_response", "learner_answer"): "concept_evidence",
        ("explicit_correction", "correction"): "correction",
        ("stated_goal", "learner_statement"): "learning_goal",
    }.get(pair) or ("accepted_pending_experiment" if pair == ("accepted_plan", "plan") and source["accepted"] is True else None)


def validate_payload(report, inputs, sources, queries, method, fixture_files):
    require(report["status"] == "projection_completed_no_encoding", "Projection did not complete")
    require(report["fixture_manifest_sha256"] == method["fixture_manifest_sha256"]
            and report["method_sha256"] == method["projection_method_sha256"]
            and report["probe_sha256"] == method["projection_probe_sha256"], "Projection provenance mismatch")
    require(report["original_fixture_files"] == fixture_files, "Original fixture bindings changed")
    require(all(report[key] is False for key in ("models_executed", "ranking_executed", "production_changed", "devices_used")), "Unexpected projection side effect")
    projection = report["projection"]
    require((projection["projected_count"], projection["excluded_count"], projection["live_memory_evidence_rechecked"]) == (42, 18, 42), "Incomplete live projection")
    require(projection["encoder_profile"] == method["profile"], "Encoder profile changed")
    require(projection["persistent_preference_coverage"] == 0 and projection["measured_hint_effectiveness_coverage"] == 0, "Invented coverage")
    require(len(projection["jobs"]) == 12 and all(row["status"] == "applied" and row["model_call_started"] is False for row in projection["jobs"]), "Projection job completion mismatch")
    require(len(projection["sessions"]) == 6 and all(row["progress_state_unchanged"] is True for row in projection["sessions"]), "Projection changed teaching progress")
    decisions = projection["decisions"]
    require(len(decisions) == len(sources) == 60 and len(queries) == 96, "Incomplete original corpus")
    require([row["source_id"] for row in decisions] == [row["id"] for row in sources], "Source order or identity changed")
    expected, projected_sources = [], []
    excluded = Counter()
    kinds = Counter()
    for row, source in zip(decisions, sources):
        require(row["original_source"] == source and row["original_text_sha256"] == sha_bytes(source["text"].encode("utf-8")), "Original source changed")
        kind = projection_kind(source)
        require(row["projection_kind"] == kind and row["projected"] is (kind is not None), "Unsupported source promoted or supported source omitted")
        if kind is None:
            reason = {"condition_limit":"unsupported_condition_limit_kind", "unaccepted_plan":"proposal_not_accepted", "temporary_request":"temporary_request_not_persistent_preference"}[source["claim_type"]]
            require(row["exclusion_reason"] == reason and "encoding_text" not in row, "Excluded source acquired encoder text")
            excluded[reason] += 1
            continue
        evidence = row["memory_evidence"]
        memory, metadata = evidence["memory"], evidence["education_metadata"]
        text = row["encoding_text"]
        require(text == PREFIXES[kind] + "：" + source["text"] == memory["content"], "Actual projected text changed")
        require(row["encoding_text_sha256"] == sha_bytes(text.encode("utf-8")), "Projected text hash mismatch")
        require(memory["memory_id"] == row["memory_id"] and memory["source_event_ids"] == [row["event_id"]], "Memory/source identity mismatch")
        require(row["event_version"] == source["version"] == 1 and metadata["support_level"] == source["support_level"]
                and metadata["claim_type"] == source["claim_type"] and metadata["kind"] == kind
                and metadata["source_grade"] == "simulated" and metadata["source_count"] == 1, "Projection grade, version or support changed")
        require(len(evidence["sources"]) == 1, "Unexpected source merge")
        live = evidence["sources"][0]
        require(live["event_id"] == row["event_id"] and live["event_version"] == 1
                and live["event_kind"] == source["event_kind"] and live["event_status"] == "active"
                and live["content"]["text"] == source["text"] and live["content"]["support_level"] == source["support_level"], "Live source evidence changed")
        if source["accepted"] is not None:
            require(live["content"]["accepted"] is source["accepted"], "Plan acceptance changed")
        require("explicit_preference" not in live["content"] and "preference_scope" not in live["content"], "Invented preference markers")
        expected.append({"key":"source:" + source["id"], "input_kind":"projected_memory", "source_ids":[source["id"]], "text":text, "text_sha256":row["encoding_text_sha256"]})
        projected_sources.append({"id":source["id"], "text":text})
        kinds[kind] += 1
    require(dict(kinds) == projection["kind_counts"] and dict(excluded) == projection["exclusion_counts"] and len(projected_sources) == 42, "Projection counts disagree")
    for condition in CONDITIONS:
        for query in queries:
            text = query["text"] if condition == "current_query" else query["text"] + " " + query["goal"] + " "
            expected.append({"key":condition + ":" + query["id"], "input_kind":condition, "query_id":query["id"], "text":text, "text_sha256":sha_bytes(text.encode("utf-8"))})
    require(len(inputs) == 234 and inputs == expected, "Encoder input order/text/hash changed, duplicated or incomplete")
    return {"sources":projected_sources, "queries":queries}


def checked_projection(root, directory, expected_report, method):
    require(re.fullmatch(r"[0-9a-f]{64}", expected_report) is not None, "Explicit lowercase report SHA256 required")
    report = checked_json(directory / "report.json", expected_report)
    projection_method = checked_json(SCRIPT_DIR / "typed_projection_method_v1.json", method["projection_method_sha256"])
    require(report["method"] == projection_method, "Embedded projection method changed")
    fixture = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2"
    manifest = checked_json(fixture / "manifest.json", method["fixture_manifest_sha256"])
    data = {name:read_jsonl(fixture / (name + ".jsonl"), manifest["files"][name + ".jsonl"]["sha256"])
            for name in ("sources", "queries", "labels", "scope_controls")}
    require((len(data["labels"]), len(data["scope_controls"])) == (96,12), "Original label/control binding changed")
    require(report["encoder_inputs"]["file"] == "encoder_inputs.jsonl" and report["encoder_inputs"]["records"] == 234, "Wrong projection input artifact")
    inputs = read_jsonl(directory / "encoder_inputs.jsonl", report["encoder_inputs"]["sha256"])
    adapted = validate_payload(report, inputs, data["sources"], data["queries"], method, manifest["files"])
    return report, inputs, adapted


def load_helper(method):
    for name, expected in method["helper_files"].items():
        require(sha_file(SCRIPT_DIR / name) == expected, "Frozen encoder helper changed: " + name)
    return importlib.import_module("bge_m3_shadow")


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as output:
        json.dump(value, output, ensure_ascii=False, indent=2, allow_nan=False)
        output.write("\n")


def memory_summary(runtime, end):
    samples = [runtime["memory"][name] for name in ("before_load", "after_load", "after_encoding", "after_unload")] + [end]
    rss = [row["VmRSS_bytes"] for row in samples if row.get("available")]
    hwm = [row["VmHWM_bytes"] for row in samples if row.get("available")]
    return {"maximum_observed_rss_bytes":max(rss) if rss else None,
            "hwm_decrease_observed":any(after < before for before, after in zip(hwm, hwm[1:])),
            "scope":"whole-process samples; maximum observed RSS is not an exclusive model or exact peak allocation"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--projection", type=Path, required=True)
    parser.add_argument("--projection-report-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--validate-only", action="store_true")
    modes.add_argument("--run-model", action="store_true")
    parser.add_argument("--load-note", default="not recorded; not a latency baseline")
    args = parser.parse_args()
    require(not args.output.exists(), "Use a new output directory")
    method = checked_json(SCRIPT_DIR / "typed_projection_encode_method_v1.json", METHOD_SHA)
    started = time.perf_counter()
    report, inputs, adapted = checked_projection(args.root.resolve(), args.projection.resolve(), args.projection_report_sha256, method)
    helper = load_helper(method)
    args.output.mkdir(parents=True, exist_ok=False)
    result = {"status":"validated_no_encoding", "method":method, "method_sha256":METHOD_SHA,
              "script_sha256":sha_file(Path(__file__)), "projection_report_sha256":args.projection_report_sha256,
              "projection_inputs_sha256":report["encoder_inputs"]["sha256"], "fixture_manifest_sha256":method["fixture_manifest_sha256"],
              "profile":method["profile"], "input_count":len(inputs), "projected_source_ids":[row["id"] for row in adapted["sources"]],
              "helper_hashes":method["helper_files"], "model_executed":False, "ranking_executed":False,
              "store_vector_imported":False, "learner_db_opened":False, "devices_used":False,
              "network_used":False, "packages_installed":False, "production_changed":False,
              "load_note":args.load_note, "python":sys.version, "platform":platform.platform()}
    try:
        if args.run_model:
            require(sys.platform == "linux", "Frozen model runtime is the existing Linux/WSL CPU environment")
            directory = args.root.resolve() / "models/memory/bge-m3"
            verify_start = time.perf_counter()
            result["assets"] = [helper.assets.verify(directory / asset[0], asset) for asset in helper.assets.ASSETS]
            result["asset_verification_ms"] = (time.perf_counter() - verify_start) * 1000
            require(helper.assets.REVISION == method["profile"]["revision"], "Asset revision changed")
            spec = json.loads((SCRIPT_DIR / "bge_m3_shadow_method_v1.json").read_text(encoding="utf-8"))["models"]["bge_m3"]
            result["dependencies"] = {name:importlib.metadata.version(name) for name in ("torch", "transformers", "tokenizers", "numpy")}
            import torch
            torch.set_num_threads(2)
            torch.set_num_interop_threads(1)
            require(torch.get_num_threads() == 2 and torch.get_num_interop_threads() == 1, "Thread configuration changed")
            result["memory_before_tokenizer"] = helper.memory_snapshot()
            tokens = helper.tokenize(adapted, directory, spec)
            require([(row["key"], row["text_sha256"]) for row in tokens] == [(row["key"], row["text_sha256"]) for row in inputs], "Helper rewrote/reordered encoder inputs")
            write_json(args.output / "token_inputs.json", tokens)
            result["token_inputs_sha256"] = sha_file(args.output / "token_inputs.json")
            result["token_count_range"] = [min(row["token_count"] for row in tokens), max(row["token_count"] for row in tokens)]
            result["tokenization_total_ms"] = sum(row["tokenization_ms"] for row in tokens)
            result["model_execution_attempted"] = True
            result["model_executed"] = None  # A failed helper may stop before or during inference.
            vectors, runtime = helper.encode(directory, tokens, spec, "bge_m3")
            require(len(vectors) == len(tokens) == 234, "Incomplete encoded vectors")
            for row in tokens:
                vector = row["encoding"]["pooled_vector"]
                require(len(vector) == 1024 and all(math.isfinite(v) for v in vector)
                        and abs(sum(v*v for v in vector)-1.0) <= 0.001, "Invalid normalized BGE vector")
                require(sha_bytes(struct.pack("<1024f", *vector)) == row["encoding"]["pooled_sha256_f32le"], "FP32 serialization mismatch")
            write_json(args.output / "bge_m3-encodings.json", tokens)
            result.update(status="encoding_completed_no_retrieval", model_executed=True, runtime=runtime,
                          encodings_sha256=sha_file(args.output / "bge_m3-encodings.json"), encoded_count=len(tokens))
            result["memory_at_end"] = helper.memory_snapshot()
            result["memory_summary"] = memory_summary(runtime, result["memory_at_end"])
            result["timing_scope"] = method["timing_scope"]
    except Exception as error:
        result.update(status="failed", error_type=type(error).__name__, error=str(error)[:1000])
        raise
    finally:
        result["elapsed_seconds"] = time.perf_counter() - started
        write_json(args.output / "report.json", result)
    print(json.dumps({"status":result["status"], "inputs":234, "model_executed":result["model_executed"], "store_vector_imported":False}), flush=True)


if __name__ == "__main__":
    main()
