"""Fixed-schema and label checks for diagnostic_postfreeze v1/v2. No retrieval.

Checks structure and exact citations, not truth or semantic entailment.
Standard library only. --self-test mutates copies in memory.
"""
import argparse
from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import re

VERSION = "education_memory_diagnostic_postfreeze_v1_20261104"
VERSION_V2 = "education_memory_diagnostic_postfreeze_v2_20261104"
SPEC_SHA = "14d04db43e70cd24e84063d53756baa64bda4e9e7736a592f2c0e34d67cb982c"
METHOD_SHA = "5296ca5b5116e213b020b9aabe68f6d524926e8ba4b69649bf6e706fce65e18c"
PACK_IDS = ("germination", "paper_bridge", "fractions", "reading", "loops", "evaporation")
MODES = {"same_zh": 12, "same_en": 12, "zh_to_en": 12, "en_to_zh": 12,
         "mixed_single": 12, "mixed_multi": 12, "negative_zh": 6, "negative_en": 6, "negative_mixed": 12}
LANGUAGES = {"zh": 30, "en": 30, "mixed": 36}
V2_CROSS_LANGUAGE_ALTERNATIVES = {"germination_q02", "paper_bridge_q02", "fractions_q02",
                                  "loops_q02", "evaporation_q02"}


def version_profile(version):
    require(version in (VERSION, VERSION_V2), "unsupported fixture version")
    modes = dict(MODES)
    if version == VERSION_V2:
        modes.update(mixed_single=13, mixed_multi=11)
    return modes, (19 if version == VERSION_V2 else 12), (4 if version == VERSION_V2 else 3)

SOURCE_KEYS = set("id pack_id text language learner_id namespace activity_id scope session_id event_kind source_type version status accepted support_level claim_type hardware_verified".split())
QUERY_KEYS = set("id pack_id text language mode goal context candidate_pool_ids tags".split())
LABEL_KEYS = set("query_id answerable sufficient_evidence_sets claim_checks forbidden_claims context_only_ids distractor_reason no_evidence_reason annotation_origin".split())
CONTEXT_KEYS = set("learner_id namespace activity_id scope session_id".split())
CONTROL_KEYS = set("id source context reference_source_id control_kind expected_eligible same_text_as_reference".split())
CITATION_KEYS = set("source_id version start_char end_char quote".split())
EVENT_KINDS = {"observation", "learner_statement", "plan", "learner_answer", "correction"}
CLAIM_KINDS = {"reported_observation", "condition_limit", "unaccepted_plan", "accepted_plan",
               "task_specific_response", "temporary_request", "explicit_correction", "stated_goal"}
HAN = re.compile(r"[\u3400-\u4dbf\u4e00-\u9fff]")
LATIN_WORD = re.compile(r"[A-Za-z]+(?:['’][A-Za-z]+)*")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def keys(value, expected, where):
    require(isinstance(value, dict) and set(value) == expected, f"{where}: wrong keys")


def nonempty(value, where):
    require(isinstance(value, str) and bool(value.strip()), f"{where}: expected nonempty text")


def unique_ids(rows, key, where):
    ids = [row[key] for row in rows]
    require(len(ids) == len(set(ids)), f"{where}: duplicate ID")
    require(all(isinstance(s, str) and re.fullmatch(r"[a-z0-9_]+", s) for s in ids), f"{where}: invalid ID")
    return set(ids)


def normalized(text):
    return " ".join(text.casefold().split())


def eligible(source, context):
    return (source["status"] == "active"
            and all(source[key] == context[key] for key in ("learner_id", "namespace", "activity_id", "scope"))
            and source["version"] == context.get("expected_source_version", source["version"])
            and source["scope"] == "current_activity"
            and source["source_type"] == "simulated" and context["namespace"] == "simulator")


def source_schema(row, where):
    keys(row, SOURCE_KEYS, where)
    for key in SOURCE_KEYS - {"version", "accepted", "hardware_verified"}:
        nonempty(row[key], f"{where}.{key}")
    require(row["pack_id"] in PACK_IDS and row["language"] in ("zh", "en"), f"{where}: pack/language")
    require(row["event_kind"] in EVENT_KINDS and row["claim_type"] in CLAIM_KINDS, f"{where}: source type")
    kinds = {"observation": {"reported_observation"},
             "learner_statement": {"condition_limit", "temporary_request", "stated_goal"},
             "plan": {"accepted_plan", "unaccepted_plan"},
             "learner_answer": {"task_specific_response"}, "correction": {"explicit_correction"}}
    require(row["claim_type"] in kinds[row["event_kind"]], f"{where}: event/claim type mismatch")
    require(row["source_type"] == "simulated" and row["hardware_verified"] is False, f"{where}: real claim")
    require(row["namespace"] in ("simulator", "real") and row["scope"] == "current_activity", f"{where}: scope")
    require(row["status"] in ("active", "invalidated") and type(row["version"]) is int and row["version"] >= 1,
            f"{where}: version/status")
    require(row["support_level"] in ("unknown", "hinted", "independent"), f"{where}: support")
    if row["event_kind"] == "plan":
        require(type(row["accepted"]) is bool, f"{where}: plan accepted must be bool")
        require(row["claim_type"] == ("accepted_plan" if row["accepted"] else "unaccepted_plan"), f"{where}: plan type")
    else:
        require(row["accepted"] is None, f"{where}: non-plan acceptance")
    if row["event_kind"] == "learner_answer":
        require(row["support_level"] in ("hinted", "independent"), f"{where}: answer support missing")
    else:
        require(row["support_level"] == "unknown", f"{where}: non-answer support label")
    if row["language"] == "zh":
        require(HAN.search(row["text"]) and 25 <= len(row["text"]) <= 220, f"{where}: Chinese source bounds")
    else:
        require(not HAN.search(row["text"]) and 20 <= len(LATIN_WORD.findall(row["text"])) <= 140,
                f"{where}: English source bounds")


def check_records(sources, queries, labels, controls, version=VERSION):
    modes, expected_alternatives, max_alternatives = version_profile(version)
    require((len(sources), len(queries), len(labels), len(controls)) == (60, 96, 96, 12), "fixed record counts")
    source_ids = unique_ids(sources, "id", "sources")
    query_ids = unique_ids(queries, "id", "queries")
    require(unique_ids(labels, "query_id", "labels") == query_ids, "query-label bijection")
    unique_ids(controls, "id", "controls")
    require(not source_ids.intersection(query_ids), "global ID overlap")
    require(Counter(row["pack_id"] for row in sources) == {pack: 10 for pack in PACK_IDS}, "source pack counts")
    require(Counter(row["pack_id"] for row in queries) == {pack: 16 for pack in PACK_IDS}, "query pack counts")
    require(Counter(row["mode"] for row in queries) == modes, "language-mode counts")
    require(Counter(row["language"] for row in queries) == LANGUAGES, "query language counts")
    for pack in PACK_IDS:
        expected_modes = {key: count // 6 for key, count in MODES.items()}
        if version == VERSION_V2 and pack == "loops":
            expected_modes.update(mixed_single=3, mixed_multi=1)
        require(Counter(row["mode"] for row in queries if row["pack_id"] == pack)
                == expected_modes, pack + ": mode distribution")
    for name, rows in (("sources", sources), ("queries", queries)):
        texts = [normalized(row["text"]) for row in rows]
        require(len(texts) == len(set(texts)), f"{name}: repeated normalized text")
    source_map = {row["id"]: row for row in sources}
    query_map = {row["id"]: row for row in queries}
    for row in sources:
        source_schema(row, row["id"])
        require(row["status"] == "active" and row["version"] == 1, "main source must be current")
    for row in queries:
        qid = row["id"]
        keys(row, QUERY_KEYS, qid)
        nonempty(row["text"], qid)
        nonempty(row["goal"], qid + ".goal")
        keys(row["context"], CONTEXT_KEYS, qid + ".context")
        require(all(isinstance(v, str) and v for v in row["context"].values()), qid + ": context fields")
        require(row["context"]["session_id"] == "synthetic_" + row["pack_id"], qid + ": context session")
        require(isinstance(row["candidate_pool_ids"], list) and len(row["candidate_pool_ids"]) == 60
                and set(row["candidate_pool_ids"]) == source_ids, qid + ": equal global candidate pool")
        require(all(eligible(source, row["context"]) for source in sources), qid + ": ineligible main pool source")
        require(isinstance(row["tags"], list) and row["tags"] and len(set(row["tags"])) == len(row["tags"]), qid + ": tags")
        require(all(isinstance(tag, str) and re.fullmatch(r"[a-z_]+", tag) for tag in row["tags"]), qid + ": tag type")
        expected = "mixed" if row["mode"].startswith("mixed") or row["mode"] == "negative_mixed" else (
            "zh" if row["mode"] in ("same_zh", "zh_to_en", "negative_zh") else "en")
        require(row["language"] == expected, qid + ": mode/language mismatch")
        if row["language"] == "en":
            require(not HAN.search(row["text"]) and LATIN_WORD.search(row["text"]), qid + ": English query text")
        else:
            require(HAN.search(row["text"]), qid + ": Han text missing")
            if row["language"] == "mixed":
                require(len(LATIN_WORD.findall(row["text"])) >= 2, qid + ": mixed wording missing")
    alternatives_count = positive = negative = multi = 0
    tag_counts = Counter()
    for row in labels:
        qid = row["query_id"]
        query = query_map[qid]
        keys(row, LABEL_KEYS, qid + ".label")
        require(type(row["answerable"]) is bool, qid + ": answerable type")
        require(row["annotation_origin"] == "method-aware_agent_authored_not_blind", qid + ": authorship")
        require(isinstance(row["sufficient_evidence_sets"], list), qid + ": evidence sets type")
        options = row["sufficient_evidence_sets"]
        require(len(options) <= max_alternatives, qid + ": alternative-set count exceeds schema")
        require(row["answerable"] == bool(options) == (not query["mode"].startswith("negative_")), qid + ": negative/positive label")
        require(isinstance(row["forbidden_claims"], list) and row["forbidden_claims"], qid + ": forbidden claims missing")
        for claim in row["forbidden_claims"]:
            nonempty(claim, qid + ".forbidden")
        require(isinstance(row["context_only_ids"], list) and set(row["context_only_ids"]) <= source_ids
                and len(row["context_only_ids"]) == len(set(row["context_only_ids"])), qid + ": context IDs")
        nonempty(row["distractor_reason"], qid + ".distractor_reason")
        tag_counts.update(query["tags"])
        if not options:
            negative += 1
            require(row["claim_checks"] == [], qid + ": absent evidence cannot have supported claims")
            nonempty(row["no_evidence_reason"], qid + ".no_evidence_reason")
            require("execution_status" not in query["tags"] and "goal_status" not in query["tags"],
                    qid + ": not-done evidence cannot be relabelled absent")
            continue
        positive += 1
        require(row["no_evidence_reason"] is None, qid + ": positive absence reason")
        sets = []
        for option in options:
            require(isinstance(option, list) and 1 <= len(option) <= 3 and len(set(option)) == len(option), qid + ": evidence set bounds")
            require(set(option) <= source_ids and all(eligible(source_map[sid], query["context"]) for sid in option), qid + ": source scope")
            sets.append(frozenset(option))
        require(len(set(sets)) == len(sets), qid + ": duplicate alternatives")
        require(not any(a < b for a in sets for b in sets), qid + ": redundant nonminimal superset")
        required = set().union(*sets)
        require(not required.intersection(row["context_only_ids"]), qid + ": context labelled as required")
        require(all(source_map[sid]["pack_id"] == query["pack_id"] for sid in required), qid + ": cross-pack answer leak")
        mode = query["mode"]
        if mode == "mixed_multi":
            multi += 1
            require(all(len(option) >= 2 and {source_map[sid]["language"] for sid in option} == {"zh", "en"} for option in sets), qid + ": mixed multi needs both languages")
        elif version == VERSION_V2 and qid in ("loops_q02", "loops_q11"):
            # Reviewed singleton and incomparable two-source alternatives. A
            # singleton makes the question non-mandatory-multi, not every set.
            require(any(len(option) == 1 for option in sets), qid + ": singleton alternative missing")
        else:
            require(all(len(option) == 1 for option in sets), qid + ": single-evidence mode")
        expected_language = {"same_zh": "zh", "same_en": "en", "zh_to_en": "en", "en_to_zh": "zh"}.get(mode)
        if expected_language:
            if version == VERSION_V2 and qid in V2_CROSS_LANGUAGE_ALTERNATIVES:
                # Same-language primary evidence still exists; reviewed valid
                # alternatives can be cross-language. Required-cross-language
                # modes remain strict and their denominator stays 24.
                require(any(all(source_map[sid]["language"] == expected_language for sid in option)
                            for option in sets), qid + ": primary same-language evidence missing")
            else:
                require(all(source_map[sid]["language"] == expected_language for sid in required), qid + ": target language mismatch")
        if len(options) > 1:
            alternatives_count += 1
        require(isinstance(row["claim_checks"], list) and 1 <= len(row["claim_checks"]) <= 6, qid + ": claim check bounds")
        cited = set()
        for claim in row["claim_checks"]:
            keys(claim, {"claim", "citations"}, qid + ".claim")
            nonempty(claim["claim"], qid + ".claim_text")
            require(isinstance(claim["citations"], list) and 1 <= len(claim["citations"]) <= 6, qid + ": citation count bounds")
            require(len({citation["source_id"] for citation in claim["citations"]}) == len(claim["citations"]),
                    qid + ": duplicate citation cannot increase source count")
            for citation in claim["citations"]:
                keys(citation, CITATION_KEYS, qid + ".citation")
                sid = citation["source_id"]
                require(sid in required, qid + ": citation not in evidence alternatives")
                source = source_map[sid]
                start, end = citation["start_char"], citation["end_char"]
                require(type(start) is int and type(end) is int and 0 <= start < end <= len(source["text"]), qid + ": citation bounds")
                require(citation["version"] == source["version"] and citation["quote"] == source["text"][start:end], qid + ": citation text/version")
                cited.add(sid)
        require(cited == required, qid + ": incomplete citation coverage")
    require((positive, negative, multi) == (72, 24, modes["mixed_multi"]), "positive/negative/multi count")
    require(alternatives_count == expected_alternatives, "alternative-set question count")
    for pack in PACK_IDS:
        pack_rows = [row for row in labels if query_map[row["query_id"]]["pack_id"] == pack]
        require(sum(row["answerable"] for row in pack_rows) == 12, pack + ": positive count")
    for tag in ("numeric", "negation", "condition", "execution_status", "support_dependence", "multiple_evidence", "entity", "temporary_preference", "goal_status", "correction"):
        require(tag_counts[tag] >= 6, "missing cross-topic capability: " + tag)
    control_counts = Counter()
    control_source_ids = []
    for row in controls:
        keys(row, CONTROL_KEYS, row["id"])
        source_schema(row["source"], row["id"] + ".source")
        keys(row["context"], CONTEXT_KEYS | {"expected_source_version"}, row["id"] + ".context")
        require(type(row["context"]["expected_source_version"]) is int and row["context"]["expected_source_version"] == 1,
                row["id"] + ": expected source version")
        require(row["reference_source_id"] in source_ids, row["id"] + ": reference missing")
        reference = source_map[row["reference_source_id"]]
        require(row["source"]["text"] == reference["text"] and row["same_text_as_reference"] is True, row["id"] + ": control text changed")
        require(row["expected_eligible"] is False and not eligible(row["source"], row["context"]), row["id"] + ": source gate leaked")
        require(eligible(reference, row["context"]), row["id"] + ": reference should be eligible")
        field = {"foreign_learner": "learner_id", "same_text_other_owner": "learner_id", "foreign_namespace": "namespace",
                 "wrong_activity": "activity_id", "invalidated_source": "status", "version_mismatch": "version"}.get(row["control_kind"])
        require(field is not None and row["source"][field] != reference[field], row["id"] + ": missing deliberate mismatch")
        require({key for key in SOURCE_KEYS if row["source"][key] != reference[key]} == {"id", field}, row["id"] + ": unplanned control changes")
        control_source_ids.append(row["source"]["id"])
        control_counts.update([row["control_kind"]])
    require(len(set(control_source_ids)) == 12 and not set(control_source_ids).intersection(source_ids | query_ids), "control source ID collision")
    require(control_counts == {key: 2 for key in ("foreign_learner", "foreign_namespace", "wrong_activity", "invalidated_source", "version_mismatch", "same_text_other_owner")}, "control-kind counts")
    return {"sources": 60, "positive_queries": positive, "negative_queries": negative,
            "multi_evidence_queries": multi, "alternative_set_queries": alternatives_count,
            "single_cross_language_queries": modes["zh_to_en"] + modes["en_to_zh"],
            "scope_controls": len(controls), "capability_tag_counts": dict(sorted(tag_counts.items())),
            "query_language_counts": LANGUAGES, "query_mode_counts": modes,
            "max_evidence_alternatives": max_alternatives, "main_pool_eligible_in_all_contexts": True,
            "exact_citation_spans_checked": True, "semantic_entailment_automatically_verified": False}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_jsonl(path):
    lines = path.read_text(encoding="utf-8").splitlines()
    require(lines and all(line.strip() for line in lines), str(path) + ": empty record")
    return [json.loads(line) for line in lines]


def self_test(records, version=VERSION):
    mutations = []
    def reject(name, mutate):
        candidate = copy.deepcopy(records)
        mutate(*candidate)
        try:
            check_records(*candidate, version=version)
        except (ValueError, KeyError):
            mutations.append(name)
            return
        raise ValueError("checker accepted intentional corruption: " + name)
    reject("duplicate_source_id", lambda s, q, l, c: s[1].update(id=s[0]["id"]))
    reject("wrong_owner_main_source", lambda s, q, l, c: s[0].update(learner_id="wrong_owner"))
    reject("invalidated_main_source", lambda s, q, l, c: s[0].update(status="invalidated"))
    reject("citation_quote_changed", lambda s, q, l, c: l[0]["claim_checks"][0]["citations"][0].update(quote="not the source text"))
    reject("citation_version_changed", lambda s, q, l, c: l[0]["claim_checks"][0]["citations"][0].update(version=9))
    reject("nonminimal_superset", lambda s, q, l, c: l[0]["sufficient_evidence_sets"].append([s[0]["id"], s[1]["id"]]))
    reject("single_source_for_multi", lambda s, q, l, c: l[10].update(sufficient_evidence_sets=[[s[0]["id"]]]))
    reject("absent_label_with_claim", lambda s, q, l, c: l[12].update(claim_checks=l[0]["claim_checks"]))
    reject("control_reenabled", lambda s, q, l, c: c[0]["source"].update(learner_id=c[0]["context"]["learner_id"]))
    reject("missing_control", lambda s, q, l, c: c.pop())
    # The original ten corruption checks remain; add the previously omitted
    # schema total bound, with otherwise distinct singleton alternatives.
    def too_many_alternatives(sources, queries, labels, controls):
        limit = version_profile(version)[2]
        labels[0]["sufficient_evidence_sets"] = [[source["id"]] for source in sources[:limit + 1]]
    reject("too_many_alternative_sets", too_many_alternatives)
    return {"intentional_corruptions_rejected": len(mutations), "cases": mutations}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    root, fixture, output = args.root.resolve(), args.fixture.resolve(), args.output.resolve()
    require(not output.exists(), "report output already exists")
    manifest = json.loads((fixture / "manifest.json").read_text(encoding="utf-8"))
    version = manifest["fixture_version"]
    modes, _, max_alternatives = version_profile(version)
    require(manifest["method_sha256"] == METHOD_SHA
            and manifest["spec_sha256"] == SPEC_SHA, "manifest method/spec mismatch")
    require(sha(root / "docs/MEMORY_CHALLENGE_SPEC_20261104.md") == SPEC_SHA
            and sha(root / "rust/ai-tutor/tests/fixtures/lexical_shadow_method_v1.json") == METHOD_SHA,
            "frozen source method/spec changed")
    require(manifest["record_schema"]["file"] == "record_schemas.json"
            and sha(fixture / "record_schemas.json") == manifest["record_schema"]["sha256"], "record schema hash mismatch")
    schema = json.loads((fixture / "record_schemas.json").read_text(encoding="utf-8"))
    for key, fields in (("source", SOURCE_KEYS), ("query", QUERY_KEYS), ("label", LABEL_KEYS), ("scope_control", CONTROL_KEYS)):
        definition = schema["$defs"][key]
        require(set(definition["required"]) == set(definition["properties"]) == fields
                and definition["additionalProperties"] is False, "record schema/checker field mismatch: " + key)
    options_schema = schema["$defs"]["label"]["properties"]["sufficient_evidence_sets"]
    require(options_schema["maxItems"] == max_alternatives and options_schema["items"]["maxItems"] == 3,
            "record schema evidence bounds mismatch")
    require(manifest["expected_modes"] == modes and manifest["expected_languages"] == LANGUAGES,
            "manifest expected distribution mismatch")
    filenames = ("sources.jsonl", "queries.jsonl", "labels.jsonl", "scope_controls.jsonl")
    records = []
    for filename in filenames:
        require(sha(fixture / filename) == manifest["files"][filename]["sha256"], filename + ": hash mismatch")
        rows = load_jsonl(fixture / filename)
        require(len(rows) == manifest["files"][filename]["records"], filename + ": row count mismatch")
        records.append(rows)
    stats = check_records(*records, version=version)
    original = json.loads((root / "rust/ai-tutor/tests/fixtures/e5_education_recall_v2.json").read_text(encoding="utf-8-sig"))
    old_texts = {normalized(row["text"]) for row in original["documents"]} | {normalized(row["query"]) for row in original["cases"]}
    require(not old_texts.intersection(normalized(row["text"]) for row in records[0] + records[1]), "exact normalized v2 reuse")
    if version == VERSION_V2:
        require(sha(fixture / "errata.json") == manifest["errata"]["sha256"], "errata hash mismatch")
    report = {"static_validation_passed": True, "fixture_version": version,
              "manifest_sha256": sha(fixture / "manifest.json"), "checker_sha256": sha(Path(__file__)),
              "record_schema_sha256": sha(fixture / "record_schemas.json"),
              "method_sha256": METHOD_SHA, "spec_sha256": SPEC_SHA, **stats,
              "no_exact_normalized_v2_reuse": True,
              "checker_self_test": self_test(records, version) if args.self_test else {"run": False},
              "model_called": False, "retrieval_run": False, "semantic_quality_pass": False,
              "human_annotated": False, "blind_evaluation": False,
              "limits": ["Structural and quote checks do not prove entailment or natural-language minimality",
                         "Scope gate is the offline fixture contract, not a test of production Store",
                         "Same-text other-owner cases exercise the learner predicate again, not a new gate",
                         "No retrieval output was used; no deployment result is implied"]}
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("x", encoding="utf-8") as target:
        json.dump(report, target, ensure_ascii=False, sort_keys=True, indent=2)
        target.write("\n")
    print(json.dumps({"static_validation_passed": True, "positive_queries": 72, "negative_queries": 24,
                      "scope_controls": 12, "model_called": False, "retrieval_run": False,
                      "report": str(output)}))


if __name__ == "__main__":
    main()
