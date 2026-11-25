"""Apply the pre-retrieval semantic review to a NEW diagnostic v2 directory.

The frozen v1 is input only. No models, retrieval, source rewriting or scores.
"""
import argparse
import copy
import datetime
import hashlib
import json
from pathlib import Path

import check_diagnostic_postfreeze as checker


FROZEN = {
    "sources.jsonl": "b96d0232e8927ae676345f9463e87a641160c0256a0d16a0c3aad60005da55fd",
    "queries.jsonl": "79e835b504ac045da63f8ab74f15de1ccfd1faeb0981a7fa6dc026c399abbbaa",
    "labels.jsonl": "75287aba94518b0b50dc78ea37c81231996999991bc8675a6a2780e02677c20e",
    "scope_controls.jsonl": "7fffe45855e063a5e5e35748ee9e118d9e57ab24b2fe017acbe132d2459238cf",
    "record_schemas.json": "0209f75e8c108bd34e8a49b96dbcecc84f30c7032cac70d8a2ecbb9ca484f394",
    "manifest.json": "a0af509b5ae8a5cd25b12b4ad7491c42104239f563e8e3627959d73bb3aabbc6",
}
ARITHMETIC_POLICY = {
    "id": "reported_fact_deterministic_arithmetic_v1",
    "allowed": "Deterministic arithmetic over explicitly reported completed actions or measurements; identify the answer as derived.",
    "forbidden": "Do not infer execution, observed results, mastery, units or missing measurements from a plan, code intention or absence of evidence.",
    "examples": ["50 mL - 42 mL = 8 mL", "reported initial 0 and three actual additions of 2 imply counter 6"],
    "new_observation": False,
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        json.dump(value, target, ensure_ascii=False, sort_keys=True, indent=2)
        target.write("\n")


def write_rows(path, rows):
    with path.open("x", encoding="utf-8", newline="\n") as target:
        for row in rows:
            target.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def revise(sources, queries, labels):
    source_map = {row["id"]: row for row in sources}
    query_map = {row["id"]: row for row in queries}
    label_map = {row["query_id"]: row for row in labels}
    changes = []

    def update(qid, reason, options=None, claim=None, query_changes=None):
        label, query = label_map[qid], query_map[qid]
        before_label, before_query = copy.deepcopy(label), copy.deepcopy(query)
        if options is not None:
            label["sufficient_evidence_sets"] = options
        if claim is not None:
            label["claim_checks"][0]["claim"] = claim
        if options is not None or claim is not None:
            ids = sorted({sid for option in label["sufficient_evidence_sets"] for sid in option})
            label["claim_checks"][0]["citations"] = [
                {"source_id": sid, "version": source_map[sid]["version"], "start_char": 0,
                 "end_char": len(source_map[sid]["text"]), "quote": source_map[sid]["text"]}
                for sid in ids
            ]
            label["context_only_ids"] = [sid for sid in label["context_only_ids"] if sid not in ids]
        if query_changes:
            query.update(query_changes)
        changes.append({
            "query_id": qid,
            "reason": reason,
            "supporting_source_ids": sorted({sid for option in label["sufficient_evidence_sets"] for sid in option}),
            "label_changes": {key: {"before": before_label[key], "after": label[key]}
                              for key in label if label[key] != before_label[key]},
            "query_changes": {key: {"before": before_query[key], "after": query[key]}
                              for key in query if query[key] != before_query[key]},
        })

    update("germination_q02", "s02直接报告A/B每日水量不同，足以否定光照单因果；题目不要求额外报告未测温度。",
           [["germination_s03"], ["germination_s02"]],
           "不能；A、B的浇水量不同，现有比较不能把差异单独归因于光照。")
    update("paper_bridge_q02", "s02明确跨度没有保持一致，已足以否定折法单因果；不强制取得重复总结s03。",
           [["paper_bridge_s03"], ["paper_bridge_s02"]],
           "不能；两座桥的跨度未保持一致，不能把差异单独归因于折法。")
    update("fractions_q02", "s02明确E原味/F草莓味，是独立的混杂条件；无需先重报完整配比才可否定水比例单因果。",
           [["fractions_s03"], ["fractions_s02"]],
           "不能；E、F使用的酸奶口味不同，不能把甜度差异只归因于水的比例。")
    update("reading_q02", "s01的原始摘录及明确未写是否淋雨已足以否定直接证明；不能将其降为仅背景。",
           [["reading_s03"], ["reading_s01"]])
    update("loops_q02", "s02单条明确H与G初值不同；另有G初值s01或s09加H初值s05的独立最小组合。包含s02的双来源超集不是最小集。",
           [["loops_s03"], ["loops_s02"], ["loops_s01", "loops_s05"], ["loops_s09", "loops_s05"]],
           "不够；G和H的初值不同，不能仅凭最后数不同判定循环体错误。")
    update("evaporation_q02", "s02已明确气流与容器形状未匹配，足以否定开口面积单因果；题目不要求额外流速/水温测量缺失陈述。",
           [["evaporation_s03"], ["evaporation_s02"]],
           "不能；气流与容器形状未匹配，不能把减少量差异全部归因于开口面积。")
    update("paper_bridge_q09", "此题问为何七枚不是成功承载量；s01直接报告六枚成功、第七枚触桌，与s09均充分，不要求证明更正事件历史。",
           [["paper_bridge_s09"], ["paper_bridge_s01"]])
    update("loops_q11", "s03写明已实际执行三次加二及初值0/1，允许确定算术后单条中文充分。保留原双来源替代，取消必须双语多来源的错误分类，门槛不降低。",
           [["loops_s01", "loops_s02"], ["loops_s09", "loops_s02"], ["loops_s03"]],
           "G初值0、H初值1，都实际执行3次加2；由这些已发生事实计算可得G为6、H为7。这是确定算术推导，不是新增运行记录。",
           {"mode": "mixed_single"})
    update("loops_q07", "原文尚未改好不等于完全没有开始改；保持未运行/无新输出的明确否定。",
           claim="No; the code changes are not complete, and no rerun or new output has been recorded.")
    update("germination_q10", "s10把记录界定为学习目标而非完成试验的证据，不能升级为所有现实试验均未完成的断言。",
           claim="想在控制浇水时了解黑暗是否改变露根数量；这条是学习目标，没有提供已完成控制试验的证据。")
    update("reading_q09", "检索前澄清必须追溯更正说明中的误记来源，不能仅凭原始观察时间相同就充当更正事件证据。",
           query_changes={"text": "更正说明，小岚那句中的 six o'clock 误记从哪来、正确几点，有无 morning or afternoon 的证据？"})
    update("loops_q09", "检索前明确问误记关系及更正是否新增运行，避免题目只问两个数字却用标签强制引用更正事件。",
           query_changes={"text": "G 的 correction 说把哪个数误记成了 output？正确输出是多少，这次更正是否新增运行？"})
    return changes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, output = args.root.resolve(), args.output.resolve()
    original = root / "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v1"
    checker.require(not output.exists(), "new v2 output directory already exists")
    checker.require(output != original and original not in output.parents, "v2 cannot be written inside frozen v1")
    for name, expected in FROZEN.items():
        checker.require(sha(original / name) == expected, "frozen v1 changed: " + name)
    checker.require(sha(root / "docs/MEMORY_CHALLENGE_SPEC_20261104.md") == checker.SPEC_SHA,
                    "frozen specification changed")
    checker.require(sha(root / "rust/ai-tutor/tests/fixtures/lexical_shadow_method_v1.json") == checker.METHOD_SHA,
                    "frozen lexical method changed")
    sources, queries, labels, controls = [checker.load_jsonl(original / name) for name in
        ("sources.jsonl", "queries.jsonl", "labels.jsonl", "scope_controls.jsonl")]
    checker.check_records(sources, queries, labels, controls)
    changes = revise(sources, queries, labels)
    stats = checker.check_records(sources, queries, labels, controls, version=checker.VERSION_V2)
    schema = json.loads((original / "record_schemas.json").read_text(encoding="utf-8"))
    old_title = schema["title"]
    schema["title"] = checker.VERSION_V2 + " record schemas"
    schema["$defs"]["label"]["properties"]["sufficient_evidence_sets"]["maxItems"] = 4
    errata = {
        "version": "diagnostic_postfreeze_pre_retrieval_semantic_errata_v2",
        "parent_manifest_sha256": FROZEN["manifest.json"],
        "frozen_v1_files": FROZEN,
        "review_origin": "separate_agent_semantic_review_of_all_60_sources_96_queries_and_12_controls",
        "human_annotated": False, "blind_evaluation": False,
        "retrieval_run_before_revision": False, "model_called": False,
        "arithmetic_policy": ARITHMETIC_POLICY,
        "changes": changes,
        "schema_changes": [
            {"field": "title", "before": old_title, "after": schema["title"]},
            {"field": "$defs.label.properties.sufficient_evidence_sets.maxItems", "before": 3, "after": 4,
             "reason": "loops_q02 has four independent minimal alternatives; all other record bounds and scope/version rules remain unchanged."},
        ],
        "checker_correction": "v1 schema already capped alternatives at 3 but its checker omitted that total bound; checker now enforces 3 for v1 and 4 for v2, with an eleventh corruption case.",
        "unchanged": {
            "byte_identical_files": ["sources.jsonl", "scope_controls.jsonl"],
            "negative_labels": 24, "positive_labels": 72,
            "scope_controls": 12, "single_cross_language_queries": 24,
            "evaluation_thresholds": "all unchanged; required multi evidence is 10 of 11, not a lowered threshold",
        },
        "review_boundaries": [
            "No negative query has a recorded answer to its requested specific value/person fact. Context-only absence/limitation statements remain citeable explanations, including loops_q13 and evaporation_q13.",
            "Prompted versus independent answers, temporary versus permanent requests, and accepted versus executed plans remain distinct; no learner ability is inferred from age.",
            "This review identifies semantic corrections, not a human/blind benchmark or automatic proof of exhaustive natural-language alternatives.",
            "Same-language primary categories can include the five reviewed cross-language alternatives; the 24 mandatory cross-language cases retain strict target-language checks.",
            "mixed_single denotes existence of a singleton sufficient set; loops_q11 retains incomparable two-source alternatives without claiming they are necessary.",
        ],
    }
    output.mkdir(parents=True, exist_ok=False)
    for name in ("sources.jsonl", "scope_controls.jsonl"):
        (output / name).write_bytes((original / name).read_bytes())
    write_rows(output / "queries.jsonl", queries)
    write_rows(output / "labels.jsonl", labels)
    write_json(output / "record_schemas.json", schema)
    write_json(output / "errata.json", errata)
    manifest = json.loads((original / "manifest.json").read_text(encoding="utf-8"))
    manifest.update(
        fixture_version=checker.VERSION_V2,
        created_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        authoring_source_sha256=sha(Path(__file__)),
        parent_fixture={"fixture_version": checker.VERSION, "manifest_sha256": FROZEN["manifest.json"]},
        errata={"file": "errata.json", "sha256": sha(output / "errata.json")},
        arithmetic_policy=ARITHMETIC_POLICY,
        expected_modes=stats["query_mode_counts"],
    )
    manifest["record_schema"]["sha256"] = sha(output / "record_schemas.json")
    for name in manifest["files"]:
        manifest["files"][name]["sha256"] = sha(output / name)
    manifest["limitations"].extend(errata["review_boundaries"][-3:])
    write_json(output / "manifest.json", manifest)
    for name, expected in FROZEN.items():
        checker.require(sha(original / name) == expected, "v1 changed during revision: " + name)
    print(json.dumps({"fixture_version": checker.VERSION_V2, "manifest_sha256": sha(output / "manifest.json"),
                      "errata_sha256": sha(output / "errata.json"), "changed_query_ids": len(changes),
                      "multi_evidence_queries": stats["multi_evidence_queries"], "v1_unchanged": True,
                      "retrieval_run": False, "model_called": False, "output": str(output)}))


if __name__ == "__main__":
    main()
