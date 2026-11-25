use super::*;
use serde_json::json;

fn request() -> Value {
    json!({"request_id":"segment-test","context":{"evidence":[{"event_id":"e1"}]}})
}
fn segment(sequence: usize, text: &str) -> Value {
    json!({"schema_version":"1.0","type":"segment","request_id":"segment-test",
        "sequence":sequence,"text":text,"used_evidence_ids":["e1"]})
}
fn final_record(count: usize, text: &str) -> Value {
    json!({"schema_version":"1.0","type":"final","request_id":"segment-test","segment_count":count,
        "reply":{"schema_version":"1.0","request_id":"segment-test","reply_text":text,
        "teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],
        "used_evidence_ids":["e1"],"needs_clarification":false}})
}
fn line(record: &Value) -> Vec<u8> {
    format!("{record}\n").into_bytes()
}
fn decoder() -> SegmentDecoder {
    SegmentDecoder::new(&request(), 7).unwrap()
}

#[test]
fn incremental_segments_preserve_multibyte_utf8_and_exact_whitespace() -> Result<()> {
    let mut parser = decoder();
    let first = line(&segment(0, " 中文😀 "));
    let mut observed = vec![];
    for (index, byte) in first.iter().enumerate() {
        let emitted = parser.push(&[*byte])?;
        if index + 1 < first.len() {
            ensure!(emitted.is_empty());
        }
        observed.extend(emitted);
    }
    observed.extend(parser.push(&line(&segment(1, "Next.")))?);
    ensure!(observed.len() == 2 && observed[0].text() == " 中文😀 ");
    ensure!(observed[0].binding().generation == 7);
    ensure!(
        parser
            .push(&line(&final_record(2, " 中文😀 Next.")))?
            .is_empty()
    );
    ensure!(parser.finish()?.reply()["reply_text"] == " 中文😀 Next.");
    Ok(())
}

#[test]
fn incomplete_json_or_unterminated_line_never_becomes_a_segment() -> Result<()> {
    for bytes in [
        b"{\"type\":\"segment\"".to_vec(),
        segment(0, "closed but no newline").to_string().into_bytes(),
        vec![0xe4, 0xb8],
    ] {
        let mut parser = decoder();
        ensure!(parser.push(&bytes)?.is_empty());
        ensure!(parser.finish().is_err());
    }
    Ok(())
}

#[test]
fn segment_rejects_wrong_request_sequence_evidence_unknown_or_duplicate_fields() -> Result<()> {
    for (field, value) in [
        ("request_id", json!("stale")),
        ("schema_version", json!("2")),
        ("sequence", json!(1)),
        ("used_evidence_ids", json!(["not-in-request"])),
        ("used_evidence_ids", json!(["e1", "e1"])),
        ("text", json!("  ")),
        ("tool", json!("Read")),
    ] {
        let mut changed = segment(0, "valid");
        changed[field] = value;
        ensure!(decoder().push(&line(&changed)).is_err());
    }
    let duplicate = format!(
        "{{\"text\":\"duplicate\",{}\n",
        &segment(0, "valid").to_string()[1..]
    );
    ensure!(decoder().push(duplicate.as_bytes()).is_err());
    Ok(())
}

#[test]
fn final_requires_exact_text_evidence_count_and_full_tutor_schema() -> Result<()> {
    let base = final_record(1, " first ");
    for changed in [
        {
            let mut v = base.clone();
            v["reply"]["reply_text"] = json!("first");
            v
        },
        {
            let mut v = base.clone();
            v["reply"]["used_evidence_ids"] = json!([]);
            v
        },
        {
            let mut v = base.clone();
            v["segment_count"] = json!(2);
            v
        },
        {
            let mut v = base.clone();
            v["reply"]["task_update_candidates"] =
                json!([{"field":"goal","value":"x","source_event_ids":["missing"]}]);
            v
        },
        {
            let mut v = base.clone();
            v["reply"]["thought"] = json!("forbidden");
            v
        },
    ] {
        let mut parser = decoder();
        ensure!(parser.push(&line(&segment(0, " first ")))?.len() == 1);
        ensure!(parser.push(&line(&changed)).is_err());
        ensure!(parser.finish().is_err());
    }
    Ok(())
}

#[test]
fn final_is_terminal_even_when_later_data_arrives_in_a_different_chunk() -> Result<()> {
    for extra in [
        line(&final_record(1, "a")),
        line(&segment(1, "b")),
        b"trailing".to_vec(),
    ] {
        let mut parser = decoder();
        parser.push(&line(&segment(0, "a")))?;
        parser.push(&line(&final_record(1, "a")))?;
        ensure!(parser.push(&extra).is_err());
        ensure!(parser.finish().is_err());
    }
    let mut parser = decoder();
    parser.push(&line(&segment(0, "a")))?;
    ensure!(parser.finish().is_err());
    Ok(())
}

#[test]
fn contract_bounds_each_line_segment_total_text_and_record_count() -> Result<()> {
    ensure!(decoder().push(&vec![b' '; MAX_LINE_BYTES + 1]).is_err());
    ensure!(decoder().push(&vec![b'\n'; MAX_OUTPUT_BYTES + 1]).is_err());
    ensure!(
        decoder()
            .push(&line(&segment(0, &"界".repeat(MAX_SEGMENT_CHARS + 1))))
            .is_err()
    );
    let mut parser = decoder();
    for index in 0..5 {
        parser.push(&line(&segment(index, &"a".repeat(320))))?;
    }
    ensure!(parser.push(&line(&segment(5, "x"))).is_err());
    let mut parser = decoder();
    for index in 0..MAX_SEGMENTS {
        parser.push(&line(&segment(index, "a")))?;
    }
    ensure!(parser.push(&line(&segment(MAX_SEGMENTS, "x"))).is_err());
    Ok(())
}

fn two_segments() -> Result<(Vec<TutorSegment>, ValidatedTutorReply)> {
    let mut parser = decoder();
    let mut segments = parser.push(&line(&segment(0, "first.")))?;
    segments.extend(parser.push(&line(&segment(1, "second.")))?);
    parser.push(&line(&final_record(2, "first.second.")))?;
    Ok((segments, parser.finish()?))
}
fn receipt(binding: &DeliveryBinding, sequence: usize, kind: ReceiptKind) -> DeliveryReceipt {
    DeliveryReceipt {
        binding: binding.clone(),
        sequence,
        kind,
    }
}

#[test]
fn ledger_requires_matching_generation_and_ordered_actual_receipts() -> Result<()> {
    let (segments, final_reply) = two_segments()?;
    let binding = segments[0].binding().clone();
    let mut ledger = DeliveryLedger::new(binding.clone())?;
    ensure!(ledger.accept(&segments[1]).is_err());
    for segment in &segments {
        ledger.accept(segment)?;
    }
    ensure!(ledger.accept(&segments[1]).is_err());
    ensure!(
        ledger
            .receipt(receipt(&binding, 1, ReceiptKind::Displayed))
            .is_err()
    );
    ensure!(
        ledger
            .receipt(receipt(&binding, 0, ReceiptKind::SpeechCompleted))
            .is_err()
    );
    let stale = DeliveryBinding {
        generation: 6,
        ..binding.clone()
    };
    ensure!(
        ledger
            .receipt(receipt(&stale, 0, ReceiptKind::Displayed))
            .is_err()
    );
    let other = DeliveryBinding {
        request_id: "other".into(),
        ..binding.clone()
    };
    ensure!(
        ledger
            .receipt(receipt(&other, 0, ReceiptKind::Displayed))
            .is_err()
    );
    ledger.receipt(receipt(&binding, 0, ReceiptKind::Displayed))?;
    ensure!(
        ledger
            .receipt(receipt(&binding, 0, ReceiptKind::Displayed))
            .is_err()
    );
    ledger.receipt(receipt(&binding, 1, ReceiptKind::Displayed))?;
    ensure!(
        ledger
            .receipt(receipt(&binding, 1, ReceiptKind::SpeechStarted))
            .is_err()
    );
    ledger.receipt(receipt(&binding, 0, ReceiptKind::SpeechStarted))?;
    ledger.receipt(receipt(&binding, 0, ReceiptKind::SpeechCompleted))?;
    ensure!(
        ledger
            .receipt(receipt(&binding, 0, ReceiptKind::SpeechCompleted))
            .is_err()
    );
    ledger.finish_validated(&final_reply)?;
    ledger.receipt(receipt(&binding, 1, ReceiptKind::SpeechStarted))?;
    ledger.receipt(receipt(&binding, 1, ReceiptKind::SpeechInterrupted))?;
    ensure!(ledger.segments()[0].speech == SpeechState::Completed);
    ensure!(ledger.segments()[1].speech == SpeechState::Interrupted);
    Ok(())
}

#[test]
fn failure_retains_delivered_prefix_but_rejects_late_completion_and_final() -> Result<()> {
    let (segments, final_reply) = two_segments()?;
    let binding = segments[0].binding().clone();
    let mut ledger = DeliveryLedger::new(binding.clone())?;
    for segment in &segments {
        ledger.accept(segment)?;
    }
    ledger.receipt(receipt(&binding, 0, ReceiptKind::Displayed))?;
    ledger.receipt(receipt(&binding, 0, ReceiptKind::SpeechStarted))?;
    ledger.invalidate();
    ensure!(
        ledger.segments()[0].displayed && ledger.segments()[0].speech == SpeechState::Interrupted
    );
    ensure!(
        !ledger.segments()[1].displayed && ledger.segments()[1].speech == SpeechState::NotStarted
    );
    ensure!(
        ledger
            .receipt(receipt(&binding, 0, ReceiptKind::SpeechCompleted))
            .is_err()
    );
    ensure!(ledger.finish_validated(&final_reply).is_err());
    let mut partial = DeliveryLedger::new(binding)?;
    partial.accept(&segments[0])?;
    ensure!(partial.finish_validated(&final_reply).is_err());
    Ok(())
}

#[test]
fn closing_speech_before_display_keeps_text_validation_and_dom_receipts_open() -> Result<()> {
    let (segments, final_reply) = two_segments()?;
    let binding = segments[0].binding().clone();
    let mut ledger = DeliveryLedger::new(binding.clone())?;
    ledger.close_speech();
    for segment in &segments {
        ledger.accept(segment)?;
    }
    ledger.receipt(receipt(&binding, 0, ReceiptKind::Displayed))?;
    ledger.receipt(receipt(&binding, 1, ReceiptKind::Displayed))?;
    ledger.finish_validated(&final_reply)?;
    ensure!(ledger.reserve_speech(0).is_err());
    ensure!(
        ledger
            .receipt(receipt(&binding, 0, ReceiptKind::SpeechStarted))
            .is_err()
    );
    ensure!(!ledger.speech_complete());
    Ok(())
}

#[test]
fn interrupted_prefix_never_authorizes_later_speech_or_restoration_as_completed() -> Result<()> {
    let (segments, _) = two_segments()?;
    let binding = segments[0].binding().clone();
    let mut ledger = DeliveryLedger::new(binding.clone())?;
    for segment in &segments {
        ledger.accept(segment)?;
    }
    ledger.receipt(receipt(&binding, 0, ReceiptKind::Displayed))?;
    ledger.receipt(receipt(&binding, 1, ReceiptKind::Displayed))?;
    ledger.reserve_speech(0)?;
    ledger.receipt(receipt(&binding, 0, ReceiptKind::SpeechStarted))?;
    ledger.receipt(receipt(&binding, 0, ReceiptKind::SpeechInterrupted))?;
    ensure!(ledger.reserve_speech(1).is_err());
    ensure!(
        ledger
            .receipt(receipt(&binding, 1, ReceiptKind::SpeechStarted))
            .is_err()
    );
    let mut invalid = ledger.segments().to_vec();
    invalid[1].speech = SpeechState::Completed;
    ensure!(DeliveryLedger::restore(binding, invalid, true, SpeechGate::Closed).is_err());
    Ok(())
}

#[test]
fn audited_clean_end_accepts_a_complete_final_without_newline_only() -> Result<()> {
    let final_bytes = final_record(1, " 中文😀 ").to_string().into_bytes();
    let mut strict = decoder();
    strict.push(&line(&segment(0, " 中文😀 ")))?;
    ensure!(strict.push(&final_bytes)?.is_empty());
    ensure!(contract_failure_code(&strict.finish().unwrap_err()) == Some("unterminated_record"));
    let mut clean = decoder();
    ensure!(clean.push(&line(&segment(0, " 中文😀 ")))?.len() == 1);
    for byte in final_bytes {
        ensure!(clean.push(&[byte])?.is_empty());
    }
    ensure!(clean.diagnostics()["pending_tail_shape"] == "complete_single_json");
    ensure!(clean.diagnostics()["pending_tail_record_kind"] == "final");
    ensure!(clean.finish_clean_end()?.reply()["reply_text"] == " 中文😀 ");
    Ok(())
}

#[test]
fn clean_end_rejects_incomplete_tail_segment_missing_final_and_extra_objects() -> Result<()> {
    let full = final_record(1, "a").to_string();
    for (tail, expected) in [
        (
            full[..full.len() - 1].as_bytes().to_vec(),
            "incomplete_json",
        ),
        (
            segment(1, "tail must not be emitted")
                .to_string()
                .into_bytes(),
            "tail_not_final",
        ),
        (b" \t".to_vec(), "missing_final"),
        (format!("{full} {{}}").into_bytes(), "invalid_json"),
        (vec![0xff], "invalid_utf8"),
        (b"{}".to_vec(), "record_schema_invalid"),
    ] {
        let mut parser = decoder();
        parser.push(&line(&segment(0, "a")))?;
        ensure!(parser.push(&tail)?.is_empty());
        let error = parser.finish_clean_end().unwrap_err();
        ensure!(
            contract_failure_code(&error) == Some(expected),
            "wrong code: {:?} expected {}",
            contract_failure_code(&error),
            expected
        );
    }
    Ok(())
}

#[test]
fn clean_end_uses_the_same_final_content_evidence_schema_and_count_validation() -> Result<()> {
    for (changed, expected) in [
        (
            {
                let mut v = final_record(1, "a");
                v["reply"]["reply_text"] = json!("different");
                v
            },
            "final_text_mismatch",
        ),
        (
            {
                let mut v = final_record(1, "a");
                v["reply"]["used_evidence_ids"] = json!([]);
                v
            },
            "final_evidence_mismatch",
        ),
        (
            {
                let mut v = final_record(1, "a");
                v["reply"]["unknown"] = json!("PRIVATE_CONTENT");
                v
            },
            "final_schema_invalid",
        ),
        (final_record(2, "a"), "final_count"),
    ] {
        let mut parser = decoder();
        parser.push(&line(&segment(0, "a")))?;
        parser.push(changed.to_string().as_bytes())?;
        let error = parser.finish_clean_end().unwrap_err();
        ensure!(contract_failure_code(&error) == Some(expected));
        ensure!(!error.to_string().contains("PRIVATE_CONTENT"));
    }
    Ok(())
}

#[test]
fn contract_diagnostics_report_shapes_without_retaining_generated_text() -> Result<()> {
    let mut parser = decoder();
    parser.push(&line(&segment(0, "PRIVATE_BODY")))?;
    parser.push(final_record(1, "PRIVATE_BODY").to_string().as_bytes())?;
    let diagnostics = parser.diagnostics();
    ensure!(diagnostics["validated_segments"] == 1);
    ensure!(diagnostics["pending_tail_record_kind"] == "final");
    ensure!(!diagnostics.to_string().contains("PRIVATE_BODY"));
    ensure!(!diagnostics.to_string().contains("segment-test"));
    Ok(())
}

#[test]
fn unknown_stage_rejects_newline_and_clean_eof_final_without_completing_prefix() -> Result<()> {
    for newline in [true, false] {
        let mut parser = decoder();
        let mut ledger = DeliveryLedger::new(DeliveryBinding {
            request_id: "segment-test".into(),
            generation: 7,
        })?;
        let prefix = parser.push(&line(&segment(0, "A validated prefix.")))?;
        assert_eq!(prefix.len(), 1);
        ledger.accept(&prefix[0])?;
        let mut final_value = final_record(1, "A validated prefix.");
        final_value["reply"]["task_update_candidates"] = json!([
            {"field":"stage","value":"static_observation","source_event_ids":["e1"]}]);
        let error = if newline {
            let error = parser.push(&line(&final_value)).unwrap_err();
            assert!(parser.finish().is_err());
            error
        } else {
            assert!(parser.push(final_value.to_string().as_bytes())?.is_empty());
            parser.finish_clean_end().unwrap_err()
        };
        assert_eq!(contract_failure_code(&error), Some("final_schema_invalid"));
        assert_eq!(error.to_string(), "最终TutorReply未通过完整校验");
        ledger.invalidate();
        assert_eq!(ledger.segments().len(), 1);
        assert!(ledger.segments()[0].validated && !ledger.segments()[0].displayed);
        assert_eq!(ledger.segments()[0].speech, SpeechState::NotStarted);
        let saved = serde_json::to_value(&ledger)?;
        assert_eq!(saved["final_validated"], false);
        assert_eq!(saved["invalidated"], true);
    }
    Ok(())
}

fn metadata_record(count: usize) -> Value {
    let mut value = final_record(count, "not supplied by metadata");
    value["type"] = json!("final_metadata");
    value["reply"].as_object_mut().unwrap().remove("reply_text");
    value
}

fn metadata_decoder() -> SegmentDecoder {
    SegmentDecoder::new_metadata(&request(), 7).unwrap()
}

fn metadata_rejection(value: &Value, newline: bool) -> Result<anyhow::Error> {
    let mut parser = metadata_decoder();
    ensure!(parser.push(&line(&segment(0, " exact 中文😀 ")))?.len() == 1);
    if newline {
        let error = parser.push(&line(value)).unwrap_err();
        ensure!(parser.finish().is_err());
        Ok(error)
    } else {
        ensure!(parser.push(value.to_string().as_bytes())?.is_empty());
        Ok(parser.finish_clean_end().unwrap_err())
    }
}

#[test]
fn metadata_final_derives_exact_fragmented_utf8_text_without_rewriting() -> Result<()> {
    let pieces = [" 中文😀 ", "Next.\t", "\n 尾部  "];
    let expected = pieces.concat();
    for newline in [true, false] {
        let mut parser = metadata_decoder();
        let mut emitted = vec![];
        for (sequence, text) in pieces.iter().enumerate() {
            let bytes = line(&segment(sequence, text));
            for (index, byte) in bytes.iter().enumerate() {
                let output = parser.push(&[*byte])?;
                if index + 1 < bytes.len() {
                    ensure!(output.is_empty());
                }
                emitted.extend(output);
            }
        }
        ensure!(emitted.len() == 3 && emitted.iter().all(|s| s.binding().generation == 7));
        let mut metadata = metadata_record(3);
        metadata["reply"]["task_update_candidates"] = json!([
            {"field":"goal","value":"保留 中文😀 来源","source_event_ids":["e1"]}]);
        let mut bytes = metadata.to_string().into_bytes();
        if newline {
            bytes.push(b'\n');
        }
        for byte in bytes {
            ensure!(parser.push(&[byte])?.is_empty());
        }
        if !newline {
            ensure!(parser.diagnostics()["pending_tail_record_kind"] == "final_metadata");
        }
        let reply = if newline {
            parser.finish()?
        } else {
            parser.finish_clean_end()?
        };
        ensure!(reply.reply()["reply_text"] == expected);
        ensure!(
            reply.reply()["task_update_candidates"] == metadata["reply"]["task_update_candidates"]
        );
        ensure!(metadata["reply"].get("reply_text").is_none());
        ensure!(expected.starts_with(' ') && expected.ends_with("  "));
    }
    Ok(())
}

#[test]
fn metadata_forbids_any_model_reply_text_including_null_and_exact_echo() -> Result<()> {
    for supplied in [
        Value::Null,
        json!(" exact 中文😀 "),
        json!("PRIVATE_REWRITTEN_BODY"),
        json!(""),
        json!([]),
    ] {
        let mut value = metadata_record(1);
        value["reply"]["reply_text"] = supplied;
        for newline in [true, false] {
            let error = metadata_rejection(&value, newline)?;
            ensure!(contract_failure_code(&error) == Some("metadata_reply_text_forbidden"));
            ensure!(!error.to_string().contains("PRIVATE_REWRITTEN_BODY"));
        }
    }
    // Escaped and duplicate nested keys still constitute a supplied text field.
    for extra in [
        "\"\\u0072eply_text\":null,",
        "\"reply_text\":\"PRIVATE_REWRITTEN_BODY\",\"reply_text\":null,",
    ] {
        let raw = metadata_record(1).to_string().replacen(
            "\"reply\":{",
            &format!("\"reply\":{{{extra}"),
            1,
        );
        let mut parser = metadata_decoder();
        parser.push(&line(&segment(0, " exact 中文😀 ")))?;
        parser.push(raw.as_bytes())?;
        let error = parser.finish_clean_end().unwrap_err();
        ensure!(contract_failure_code(&error) == Some("metadata_reply_text_forbidden"));
    }
    Ok(())
}

#[test]
fn metadata_final_keeps_outer_shape_inner_schema_count_and_citation_guards() -> Result<()> {
    let mut invalid = vec![];
    for count in [0, 2] {
        invalid.push((metadata_record(count), "final_count"));
    }
    for (field, value) in [
        ("request_id", json!("other")),
        ("schema_version", json!("2.0")),
    ] {
        let mut v = metadata_record(1);
        v[field] = value;
        invalid.push((v, "request_binding"));
    }
    for field in [
        "type",
        "schema_version",
        "request_id",
        "segment_count",
        "reply",
    ] {
        let mut v = metadata_record(1);
        v.as_object_mut().unwrap().remove(field);
        invalid.push((v, "record_schema_invalid"));
    }
    for field in [
        "schema_version",
        "request_id",
        "teaching_move",
        "task_update_candidates",
        "memory_candidates",
        "used_evidence_ids",
        "needs_clarification",
    ] {
        let mut v = metadata_record(1);
        v["reply"].as_object_mut().unwrap().remove(field);
        invalid.push((v, "final_schema_invalid"));
    }
    for outer_field in ["generation", "reply_text", "unexpected"] {
        let mut v = metadata_record(1);
        v[outer_field] = Value::Null;
        invalid.push((v, "record_schema_invalid"));
    }
    for reply in [Value::Null, json!([]), json!("PRIVATE_NOT_METADATA")] {
        let mut v = metadata_record(1);
        v["reply"] = reply;
        invalid.push((v, "final_schema_invalid"));
    }
    for (field, value) in [
        ("request_id", json!("other")),
        ("schema_version", json!("2.0")),
        ("thought", json!("PRIVATE_NOT_METADATA")),
        ("segments", json!(["forged body"])),
        ("needs_clarification", json!("false")),
        (
            "task_update_candidates",
            json!([{"field":"stage","value":"static_observation","source_event_ids":["e1"]}]),
        ),
        (
            "task_update_candidates",
            json!([{"field":"goal","value":"x","source_event_ids":["missing"]}]),
        ),
        ("used_evidence_ids", json!(["missing"])),
    ] {
        let mut v = metadata_record(1);
        v["reply"][field] = value;
        invalid.push((v, "final_schema_invalid"));
    }
    let mut missing_union = metadata_record(1);
    missing_union["reply"]["used_evidence_ids"] = json!([]);
    invalid.push((missing_union, "final_evidence_mismatch"));
    for (value, expected) in invalid {
        for newline in [true, false] {
            let error = metadata_rejection(&value, newline)?;
            ensure!(
                contract_failure_code(&error) == Some(expected),
                "wrong typed code {:?}, expected {expected}",
                contract_failure_code(&error)
            );
            ensure!(!error.to_string().contains("PRIVATE_NOT_METADATA"));
        }
    }
    let mut cited = metadata_record(1);
    cited["reply"]["used_evidence_ids"] = json!(["missing"]);
    ensure!(citation::typed_failure(&metadata_rejection(&cited, false)?).is_some());
    Ok(())
}

#[test]
fn metadata_protocol_is_explicit_without_legacy_fallback_and_is_terminal() -> Result<()> {
    for newline in [true, false] {
        let mut legacy = decoder();
        legacy.push(&line(&segment(0, " exact 中文😀 ")))?;
        let value = metadata_record(1);
        let error = if newline {
            legacy.push(&line(&value)).unwrap_err()
        } else {
            legacy.push(value.to_string().as_bytes())?;
            ensure!(legacy.diagnostics()["pending_tail_record_kind"] == "other");
            legacy.finish_clean_end().unwrap_err()
        };
        ensure!(contract_failure_code(&error) == Some("record_schema_invalid"));
        let legacy_final = final_record(1, " exact 中文😀 ");
        ensure!(
            contract_failure_code(&metadata_rejection(&legacy_final, newline)?)
                == Some("final_protocol_mismatch")
        );
    }
    // Default mode still requires the model's exact body; it never silently derives it.
    let mut legacy = decoder();
    legacy.push(&line(&segment(0, " a ")))?;
    ensure!(
        contract_failure_code(&legacy.push(&line(&final_record(1, "a"))).unwrap_err())
            == Some("final_text_mismatch")
    );
    for extra in [
        line(&metadata_record(1)),
        line(&final_record(1, " exact 中文😀 ")),
        line(&segment(1, "late")),
        b"trailing".to_vec(),
    ] {
        let mut parser = metadata_decoder();
        parser.push(&line(&segment(0, " exact 中文😀 ")))?;
        parser.push(&line(&metadata_record(1)))?;
        ensure!(
            contract_failure_code(&parser.push(&extra).unwrap_err()) == Some("text_after_final")
        );
        ensure!(parser.finish().is_err());
    }
    ensure!(SegmentDecoder::new_metadata(&request(), 0).is_err());
    Ok(())
}

#[test]
fn metadata_mode_does_not_bypass_segment_limits_or_accept_a_tail_segment() -> Result<()> {
    let mut forged = segment(0, "PRIVATE_BAD_SOURCE");
    forged["used_evidence_ids"] = json!(["missing"]);
    ensure!(
        contract_failure_code(&metadata_decoder().push(&line(&forged)).unwrap_err())
            == Some("segment_evidence")
    );
    ensure!(
        metadata_decoder()
            .push(&vec![b'\n'; MAX_OUTPUT_BYTES + 1])
            .is_err()
    );
    let mut parser = metadata_decoder();
    for sequence in 0..5 {
        parser.push(&line(&segment(sequence, &"界".repeat(320))))?;
    }
    ensure!(
        contract_failure_code(&parser.push(&line(&segment(5, "x"))).unwrap_err())
            == Some("reply_length")
    );
    let mut no_segments = metadata_decoder();
    ensure!(
        contract_failure_code(&no_segments.push(&line(&metadata_record(0))).unwrap_err())
            == Some("final_count")
    );
    for tail in [
        segment(1, "not a final").to_string(),
        format!("{} {{}}", metadata_record(1)),
    ] {
        let mut parser = metadata_decoder();
        parser.push(&line(&segment(0, " exact 中文😀 ")))?;
        parser.push(tail.as_bytes())?;
        ensure!(parser.finish_clean_end().is_err());
    }
    Ok(())
}
