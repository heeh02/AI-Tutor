//! One meaning for report provenance across activities, comparisons and memory.
pub fn source_label(language: &str, source: &str) -> &'static str {
    let (zh, en) = match source {
        "learner_report" => ("学习者自述", "Learner report"),
        "operator_confirmed" => ("人工报告确认", "Human-confirmed report"),
        "simulated" => ("模拟记录", "Simulated record"),
        "manual_report" => ("人工报告", "Human report"),
        "hardware" | "real_hardware" => ("硬件事件", "Hardware event"),
        "historical" => ("历史记录", "Historical record"),
        "assistant_suggestion" => ("教学建议", "Tutor suggestion"),
        "model_interpreted_report" => ("根据报告整理", "Interpreted report"),
        "unknown" => ("当前未知", "Currently unknown"),
        _ => ("来源未注明", "Source not specified"),
    };
    if language == "en" { en } else { zh }
}
pub fn source_description(language: &str, source: &str, plain: bool) -> &'static str {
    let (zh, en) = match (source, plain) {
        ("learner_report", true) => (
            "这是学习者说的，未经硬件验证。",
            "The learner reported this. It is not hardware verified.",
        ),
        ("operator_confirmed", true) => (
            "有人确认了这份报告，仍未经硬件验证。",
            "A person confirmed the report. It is still not hardware verified.",
        ),
        ("simulated", _) => (
            "这是模拟空间中的记录，不能作为真实实验或学习表现的证据。",
            "This belongs to the simulation space and is not evidence of real experiments or learning performance.",
        ),
        ("learner_report", false) => (
            "这是学习者的自述，不等于独立观察或硬件验证。",
            "This is a learner report, not independent observation or hardware verification.",
        ),
        ("operator_confirmed", false) => (
            "人工确认针对报告本身，不等于硬件验证，也不自动证明实验完成或知识已掌握。",
            "Human confirmation applies to the report, not hardware verification. It does not establish experiment completion or mastery.",
        ),
        _ => (
            "没有可用的来源等级，不推断其可信程度。",
            "No source grade is available; credibility is not inferred.",
        ),
    };
    if language == "en" { en } else { zh }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_and_simulation_do_not_claim_objective_verification() {
        assert_eq!(
            source_label("en", "operator_confirmed"),
            "Human-confirmed report"
        );
        assert!(
            source_description("en", "operator_confirmed", true).contains("not hardware verified")
        );
        assert!(source_description("en", "simulated", false).contains("not evidence of real"));
        assert_eq!(
            source_label("en", "opaque-internal-source"),
            "Source not specified"
        );
    }
}
