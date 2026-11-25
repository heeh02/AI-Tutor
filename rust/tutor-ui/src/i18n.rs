use serde_json::Value;

pub fn ui_language(value: &Value) -> &'static str {
    if value.as_str() == Some("en") {
        "en"
    } else {
        "zh"
    }
}
pub fn reading_level(value: &Value) -> &'static str {
    match value.as_str() {
        Some("plain") => "plain",
        Some("technical") => "technical",
        _ => "standard",
    }
}
pub fn speech_speed(value: &Value) -> &'static str {
    match value.as_str() {
        Some("slow") => "slow",
        Some("fast") => "fast",
        _ => "normal",
    }
}

/// Localize only the structured style; an explicit request stays verbatim even
/// when its text happens to match a translated interface label.
pub fn session_preference_text(language: &str, value: &Value) -> Option<String> {
    value.as_object().filter(|fields| !fields.is_empty())?;
    let mut parts = Vec::new();
    let style = match value["help_style"].as_str() {
        Some("one_step") => Some("一次一步"),
        Some("guided") => Some("适量提示"),
        Some("direct") => Some("直接解释"),
        _ => None,
    };
    if let Some(style) = style {
        parts.push(t(language, style).to_owned());
    }
    let request = crate::model::text(&value["explicit_request"]);
    if !request.is_empty() {
        parts.push(request);
    }
    Some(if parts.is_empty() {
        t(language, "本次活动已保存临时提示偏好。").to_owned()
    } else {
        parts.join("\n")
    })
}

/// Translate interface copy only. Learner reports, model replies, names and
/// diagnostic messages remain verbatim; teaching language is independent.
pub fn t<'a>(language: &str, source: &'a str) -> &'a str {
    if language != "en" {
        return source;
    }
    match source {
        "继续活动" => "Continuing activity",
        "请求未被接受，请核对当前活动。" => {
            "The request was not accepted. Check the current activity."
        }
        "请先结束录音并处理转写，再继续过去的活动。" => {
            "Finish recording and review the transcript before continuing a past activity."
        }
        "请先保存当前模块或观察报告，再继续过去的活动。" => {
            "Save the current modules or observation report before continuing a past activity."
        }
        "教师工作台" => "Teacher workbench",
        "功能积木 · 思考、搭建、再试一次" => {
            "Functional modules · Think, build, try again"
        }
        "本机工作台已连接" => "Connected to local workstation",
        "正在连接本机服务" => "Connecting to local service",
        "让每一次尝试，都成为下一步的起点。" => {
            "Let each experiment lead to the next step."
        }
        "围绕真实功能模块，把想法变成行为；用观察和记忆，连接每一次学习。" => {
            "Build ideas with functional modules and connect learning through observations and memory."
        }
        "当前学习者档案" => "Current learner profile",
        "尚未选择" => "Not selected",
        "匿名档案" => "Anonymous profile",
        "真实学习记录" => "Real learning records",
        "独立模拟空间" => "Separate simulation space",
        "请在教师工作台创建或选择" => "Create or select in the teacher workbench",
        "模拟" => "Simulation",
        "从一个匿名档案开始。" => "Start with an anonymous profile.",
        "教师选择档案后，Tutor 才会记录和召回这位学习者的进展。" => {
            "Select a profile to save and recall this learner's progress."
        }
        "设置学习者" => "Choose learner",
        "当前为独立模拟空间。模块、活动和记忆均标记为模拟，不作为真实硬件验证。" => {
            "Simulation space: modules, activities and memories are simulated, not hardware verification."
        }
        "一起想下一步" => "Think about the next step",
        "Kimi · 远程推理" => "Kimi · Remote reasoning",
        "本次活动已结束" => "This activity is finished",
        "学习记录已保存。下一次开始时，重新确认现在接上的模块。" => {
            "Progress is saved. Confirm the currently connected modules before the next activity."
        }
        "开始下一次活动" => "Start next activity",
        "相关学习记忆" => "Related learning memories",
        "这些是历史报告。先确认今天的搭建，再继续实验或提出新想法。" => {
            "These are past reports. Confirm today's build before continuing or trying a new idea."
        }
        "今天，你想让积木做什么？" => "What would you like your modules to do today?",
        "说说你的想法，或描述刚才试跑时观察到的现象。" => {
            "Share an idea or describe what you observed in your last test."
        }
        "主控、电机、测距、灯" => "Controller, motor, distance sensor, light",
        "从模块的能力开始，一次尝试一个改变。" => {
            "Start with what the modules can do. Change one thing at a time."
        }
        "发送" => "Send",
        "停止" => "Stop",
        "按住说话" => "Hold to speak",
        "松开结束" => "Release to finish",
        "正在准备本机录音…" => "Preparing local recording…",
        "正在本机录音 · 松开结束" => "Recording locally · Release to finish",
        "正在本机转写…" => "Transcribing locally…",
        "正在生成 · 可停止或提交新问题" => {
            "Generating · Stop or submit a new question"
        }
        "文字输入 / 按住说话" => "Type / Hold to speak",
        "自动朗读" => "Read aloud automatically",
        "朗读" => "Read aloud",
        "本次活动已结束，请开始下一次活动" => {
            "This activity has ended. Start the next activity."
        }
        "描述你的目标、搭建或观察结果…" => {
            "Describe your goal, build, or observations…"
        }
        "先选择一个学习者档案…" => "Select a learner profile first…",
        "转写后可修改再发送。语音在本机处理，教学文字会发送到 Kimi。" => {
            "Review and edit the transcript before sending. Audio stays local; teaching text is sent to Kimi."
        }
        "正在结合当前活动与相关记忆思考…" => {
            "Thinking about this activity and related memories…"
        }
        "取回上次问题，检查后再发送" => "Restore the last question for review",
        "结束并保存" => "Finish and save",
        "本次学习回顾" => "Activity review",
        "学习正在发生" => "Learning in progress",
        "当前目标" => "Current goal",
        "功能模块" => "Functional modules",
        "当前活动会自动保存" => "This activity is saved automatically",
        "本次学习记录已保存" => "This activity has been saved",
        "学习者档案" => "Learner profiles",
        "选择档案" => "Select profile",
        "选择匿名档案" => "Select anonymous profile",
        "真实记录" => "Real records",
        "独立模拟" => "Separate simulation",
        "创建" => "Create",
        "只使用匿名名称。视觉目标由教师另行选择。" => {
            "Use anonymous names. Visual targets are selected separately."
        }
        "开启新的学习活动" => "Start a new activity",
        "结束本次活动并保存" => "Finish this activity and save",
        "当前模块与证据" => "Current modules and evidence",
        "来源" => "Source",
        "教师确认" => "Teacher confirmation",
        "学习者报告" => "Learner report",
        "确认当前勾选模块" => "Confirm selected modules",
        "保存模块变更与报告" => "Save module changes and report",
        "记录本次观察" => "Save this observation",
        "教学与本地音频" => "Teaching and local audio",
        "教学语言" => "Teaching language",
        "提示方式" => "Guidance style",
        "档案提示方式" => "Profile guidance style",
        "本次活动偏好" => "Preferences for this activity",
        "本次活动已保存临时提示偏好。" => {
            "Temporary guidance preferences are saved for this activity."
        }
        "本次偏好会影响当前指导。修改提示方式或恢复档案设置可清除本次偏好。" => {
            "These preferences affect current guidance. Change the guidance style or restore the profile setting to clear them."
        }
        "恢复档案提示方式" => "Restore profile guidance",
        "一次一步" => "One step at a time",
        "直接解释" => "Direct explanation",
        "适量提示" => "Guided hints",
        "阅读表达" => "Reading style",
        "短句、少术语" => "Short sentences, fewer terms",
        "常规说明" => "Standard explanations",
        "术语与完整解释" => "Technical terms and full explanations",
        "朗读语速" => "Speech speed",
        "较慢" => "Slower",
        "正常" => "Normal",
        "较快" => "Faster",
        "按本次需要选择表达方式，不根据年龄推断能力。" => {
            "Choose the style you need; ability is not inferred from age."
        }
        "麦克风" => "Microphone",
        "系统默认输入" => "System default input",
        "停止本地播报" => "Stop local speech",
        "取消" => "Cancel",
        "更正" => "Correct",
        "保存更正" => "Save correction",
        "刷新" => "Refresh",
        "学习记忆" => "Learning memories",
        "运行状态" => "System status",
        "看见当前目标" => "Current visual target",
        "仅人脸检测" => "Face detection only",
        "自动选择已发现的物理相机" => "Auto-select detected physical camera",
        "本机当前画面" => "Live local preview",
        "二进制预览" => "Binary preview",
        "兼容预览（HTTP）" => "Compatibility preview (HTTP)",
        "重试二进制预览" => "Retry binary preview",
        "使用兼容预览" => "Use compatibility preview",
        "录音字幕（未发送）" => "Live captions · Not sent",
        "字幕状态暂不可用；结束录音后检查转写结果。" => {
            "Caption status is unavailable. Check the transcript after recording."
        }
        "正在整理最终转写…" => "Preparing the final transcript…",
        "松开后进行本地转写。" => "Local transcription starts after you release.",
        "等待本次录音的转写状态…" => {
            "Waiting for this recording's transcription status…"
        }
        "正在识别，临时文字可能修订…" => {
            "Recognizing speech; temporary text may change…"
        }
        "松开后检查并编辑，再发送。" => "Release, review and edit, then send.",
        "识别状态" => "Recognition status",
        "正在连接摄像头" => "Connecting camera",
        "摄像头连接中断" => "Camera connection interrupted",
        "摄像头尚未开启" => "Camera is off",
        "当前没有可用画面。" => "No current frame is available.",
        "关闭摄像头" => "Close camera",
        "重新开启摄像头" => "Reopen camera",
        "开启摄像头" => "Open camera",
        "镜像显示" => "Mirror preview",
        "当前视觉目标（画面内）" => "Selected visual target",
        "选择人脸不会切换学习者档案" => {
            "Selecting a face does not switch the learner profile"
        }
        "目标在画面内" => "Target is visible",
        "目标暂时丢失" => "Target temporarily lost",
        "临时视觉目标" => "Temporary visual target",
        "等待教师选择" => "Waiting for selection",
        "临时目标" => "Temporary target",
        "已选目标 · 暂时丢失" => "Selected target · Temporarily lost",
        "暂时暂停 · 等待原目标" => "Paused · Waiting for the selected target",
        "正在核验设备 · 可随时停止" => "Verifying device · Stop remains available",
        "正在停止跟随" => "Stopping physical following",
        "物理跟随已启用" => "Physical following enabled",
        "方向已标定 · 等待启用" => "Direction calibrated · Ready to enable",
        "方向已标定 · 等待有效目标" => {
            "Direction calibrated · Waiting for a valid target"
        }
        "物理跟随关闭 · 尚未通过标定" => {
            "Physical following off · Calibration not verified"
        }
        "清除" => "Clear",
        "点击脸框只选择画面内目标，不会开启物理云台跟随。" => {
            "Selecting a face box does not enable physical camera following."
        }
        "画面只在本机处理，不发送给任何远程教学服务。临时标签不代表身份、说话者或情绪。" => {
            "Video stays local and is never sent to a remote teaching service. Temporary labels do not identify a person, speaker, or emotion."
        }
        "物理云台跟随" => "Physical camera following",
        "云台高级控制与标定" => "Advanced camera control and calibration",
        "启用物理跟随（成人试用）" => "Enable physical following (adult trial)",
        "发送中" => "Sending",
        "切换档案" => "Switching profile",
        "创建档案" => "Creating profile",
        "更新教学设置" => "Saving teaching settings",
        "更新界面语言" => "Saving interface language",
        "保存活动报告" => "Saving observation",
        "结束活动" => "Finishing activity",
        "新建活动" => "Starting activity",
        "本地播报" => "Local speech",
        "停止中" => "Stopping",
        "主控" => "Controller",
        "电机" => "Motor",
        "测距" => "Distance sensor",
        "灯" => "Light",
        "已报告" => "Reported",
        "当前未知" => "Currently unknown",
        "未报告连接" => "Connection not reported",
        "已结束" => "Finished",
        "提出想法" => "Set a goal",
        "确认模块" => "Check modules",
        "寻找能力" => "Explore capabilities",
        "搭建检查" => "Check the build",
        "准备测试" => "Prepare a test",
        "观察结果" => "Observe the result",
        "比较与修改" => "Compare and revise",
        "回顾学习" => "Reflect on learning",
        "已暂停" => "Paused",
        "活动结束" => "Activity finished",
        "等待活动" => "Waiting for an activity",
        "还没有记录本次观察" => "No observation has been recorded yet",
        "等待学习者提出想法" => "Waiting for the learner's idea",
        "连接状态来自报告；人脸摄像头不识别积木。" => {
            "Connections are reported, not detected. The face camera does not identify modules."
        }
        "最新观察 / 报告" => "Latest observation / report",
        "已有尝试与观察" => "Reported attempts and observations",
        "等待记录本次尝试或观察" => {
            "Waiting for an attempt or observation to be recorded"
        }
        "待确认 / 下一次实验" => "To confirm / next experiment",
        "从当前目标决定下一步" => "Choose the next step from the current goal",
        "下次从相关记忆继续，并重新确认搭建。" => {
            "Continue from relevant memories next time and check the current build again."
        }
        "完成尝试后，可结束并保存本次活动。" => {
            "After your attempt, you can finish and save this activity."
        }
        "报告来源" => "Report source",
        "仅填写观察不会改变模块。全部取消勾选并确认，可明确记录当前未连接模块。" => {
            "An observation alone does not change the module list. To report no connected modules, clear all boxes and confirm."
        }
        "教师活动报告" => "Teacher activity report",
        "本次试验的观察，如：停车亮灯成功，想让间隙更大" => {
            "Describe this trial, for example: it stopped and the light came on; next I want a wider gap"
        }
        "模块确认已保存；观察报告按填写内容记录。" => {
            "Module confirmation saved; the observation was recorded as entered."
        }
        "本次观察已保存，当前模块保持原有报告。" => {
            "Observation saved; the reported module list is unchanged."
        }
        "可检查，也可忘记" => "Review or forget records",
        "默认不保存原始视频、脸部裁剪或录音。教学文本通过 Kimi 远程处理。教师可纠正记忆或清除此档案的数据。" => {
            "Raw video, face crops and recordings are not saved by default. Teaching text is processed remotely by Kimi. Teachers can correct memories or clear this profile's data."
        }
        "清除当前档案学习数据" => "Clear this profile's learning data",
        "清除这个档案的学习数据？" => "Clear this profile's learning data?",
        "忘记这条学习记忆？" => "Forget this learning memory?",
        "相关派生记录和本应用创建的对话记录也将清理。此操作无法撤销。" => {
            "Related derived records and conversations created by this app will also be removed. This cannot be undone."
        }
        "确认清除档案数据" => "Clear profile data",
        "确认忘记这条记忆" => "Forget this memory",
        "档案：" => "Profile: ",
        "关闭提示" => "Dismiss message",
        "操作未完成。请检查连接或设置后重试；已有输入不会因此被自动发送。" => {
            "The action did not finish. Check the connection or settings and try again. Existing input will not be sent automatically."
        }
        "查看服务详情（原文）" => "Service details (original wording)",
        "录音响应缺少有效编号，请停止后重新录音。" => {
            "The recording could not be identified. Stop and record again."
        }
        "录音请求已过期，请重新录音。" => {
            "This recording is no longer current. Please record again."
        }
        "请先处理输入草稿和未采用文字，再切换档案或活动。" => {
            "Resolve or clear your draft and pending text before changing profile or activity."
        }
        "请先等待本次发送结果，再切换档案或活动。" => {
            "Wait for this send result before changing profile or activity."
        }
        "合并后超过4000字，请先精简草稿；未采用文字仍已保留。" => {
            "The combined text exceeds 4,000 characters. Shorten your draft first; the pending text is still preserved."
        }
        "没有识别到清楚的语音，请重录或输入文字。" => {
            "No clear speech was recognized. Record again or type your message."
        }
        "已删除该档案的记忆与活动记录" => {
            "This profile's memories and activity records have been deleted."
        }
        "请在学习记忆中选择具体条目并忘记，或发送“忘记全部记忆”。本次请求未发送远程模型。" => {
            "Choose a specific item under Learning memories to forget, or use the profile's clear-data control. This request was not sent to the remote model."
        }
        "请先保存或清空比较草稿，再切换档案或活动。" => {
            "Save or clear the comparison draft before changing profiles or sessions."
        }
        "本轮回复未完成。可检查详情，或取回问题后再发送。" => {
            "This reply is incomplete. Review the details, or restore the question before sending again."
        }
        "请检查后端状态后重试。" => "Check the service status and try again.",
        "新匿名档案名称" => "New anonymous profile name",
        "新档案名称，如 学习者 A" => "New profile name, for example Learner A",
        "档案数据空间" => "Profile data space",
        "给 Tutor 的问题或观察" => "Question or observation for the tutor",
        "AI Tutor 首页" => "AI Tutor home",
        "本地语音播报（仅当前有效轮次）" => {
            "Read aloud locally (current valid turns only)"
        }
        "停止生成和播报" => "Stop generation and speech",
        "已取消" => "Cancelled",
        "待投递" => "Awaiting display",
        "朗读已打断" => "Speech interrupted",
        "教学指导" => "Tutor guidance",
        _ => source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn destructive_and_recovery_actions_have_english_copy() {
        for label in [
            "清除这个档案的学习数据？",
            "确认忘记这条记忆",
            "相关派生记录和本应用创建的对话记录也将清理。此操作无法撤销。",
            "没有识别到清楚的语音，请重录或输入文字。",
            "本轮回复未完成。可检查详情，或取回问题后再发送。",
            "教师活动报告",
            "确认当前勾选模块",
        ] {
            let english = t("en", label);
            assert_ne!(english, label);
            assert!(!english
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
        }
    }
    #[test]
    fn session_preference_notice_keeps_explicit_requests_verbatim() {
        assert_eq!(session_preference_text("en", &Value::Null), None);
        assert_eq!(session_preference_text("en", &json!({})), None);
        assert_eq!(
            session_preference_text("en", &json!({"explicit_request":{"content":"发送"}})),
            Some("发送".into())
        );
        assert_eq!(
            session_preference_text(
                "en",
                &json!({"help_style":"one_step","explicit_request":"Please explain why.\n这次先别给答案。"})
            ),
            Some("One step at a time\nPlease explain why.\n这次先别给答案。".into())
        );
        assert!(session_preference_text("zh", &json!({"source_event_ids":["event-1"]})).is_some());
    }
    #[test]
    fn older_state_uses_independent_defaults_without_inferred_age_or_ability() {
        assert_eq!(ui_language(&Value::Null), "zh");
        assert_eq!(reading_level(&Value::Null), "standard");
        assert_eq!(speech_speed(&Value::Null), "normal");
        assert_eq!(reading_level(&json!("age-7")), "standard");
        assert_eq!(speech_speed(&json!(1.2)), "normal");
    }
    #[test]
    fn interface_language_does_not_translate_reports_or_change_learning_preferences() {
        assert_eq!(t("en", "发送"), "Send");
        assert_eq!(t("zh", "发送"), "发送");
        assert_eq!(t("en", "第二次停车间隙更大"), "第二次停车间隙更大");
        assert_eq!(reading_level(&json!("plain")), "plain");
        assert_eq!(reading_level(&json!("technical")), "technical");
        assert_eq!(speech_speed(&json!("fast")), "fast");
    }
}
