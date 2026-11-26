//! Shared audited isolation and bounded transport primitives for ACP 2.3.1.
//! No model call or delivery occurs in this module.
use super::*;
use std::io::Read;

pub(super) const STDOUT_LIMIT: usize = 256 * 1024;
pub(super) const LINE_LIMIT: usize = 64 * 1024;
pub(super) const WIRE_LIMIT: usize = 512 * 1024;

/// Deliberately contains no remote message/data, prompt, path or error text.
/// Callers project this type, never `anyhow::Error::to_string()`, into metrics.
#[derive(Clone, Copy, Debug)]
pub(super) enum AcpFailure {
    Read,
    Write,
    WriteTimeout,
    Eof,
    InvalidJson,
    OutputLimit,
    InputLimit,
    Envelope,
    ResponseId,
    RemoteRpc(Option<i64>),
    Protocol,
    Capability,
    Profile,
    Prepare,
    Spawn,
    ProcessOwnership,
    Cancelled,
    Deadline,
    ConsumerClosed,
    Stderr,
    Cleanup,
    Operation,
}
impl AcpFailure {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Read => "acp_read_failed",
            Self::Write => "acp_write_failed",
            Self::WriteTimeout => "acp_write_timeout",
            Self::Eof => "acp_unexpected_eof",
            Self::InvalidJson => "acp_invalid_json",
            Self::OutputLimit => "acp_output_limit",
            Self::InputLimit => "acp_input_limit",
            Self::Envelope => "acp_invalid_envelope",
            Self::ResponseId => "acp_response_id_mismatch",
            Self::RemoteRpc(_) => "acp_remote_rpc_error",
            Self::Protocol => "acp_protocol_rejected",
            Self::Capability => "acp_disabled_capability",
            Self::Profile => "acp_profile_not_verified",
            Self::Prepare => "acp_preparation_failed",
            Self::Spawn => "acp_spawn_failed",
            Self::ProcessOwnership => "acp_process_ownership_failed",
            Self::Cancelled => "request_cancelled",
            Self::Deadline => "acp_deadline_exceeded",
            Self::ConsumerClosed => "acp_consumer_closed",
            Self::Stderr => "acp_stderr_failed_or_limited",
            Self::Cleanup => "acp_cleanup_failed",
            Self::Operation => "acp_operation_failed",
        }
    }
    pub(super) fn rpc_code(self) -> Option<i64> {
        if let Self::RemoteRpc(code) = self {
            code
        } else {
            None
        }
    }
}
impl std::fmt::Display for AcpFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // This text can reach state.errors and its classroom-facing details.
        // Machine diagnostics use code(), never this localized text.
        out.write_str(match self {
            Self::Cancelled => "请求已取消。",
            Self::Deadline | Self::WriteTimeout => "等待教学服务超时，本轮回复未完成。",
            Self::Read | Self::Write | Self::Eof => "教学服务连接中断，本轮回复未完成。",
            Self::RemoteRpc(_) => "教学服务未能完成本轮回复。",
            Self::Prepare | Self::Spawn | Self::ProcessOwnership | Self::Profile => {
                "教学服务尚未准备好，请检查服务状态。"
            }
            Self::Cleanup => "教学服务清理尚未确认，请检查服务状态。",
            Self::ConsumerClosed => "本轮回复的接收已结束。",
            _ => "教学服务返回异常，本轮回复未完成。",
        })
    }
}
impl std::error::Error for AcpFailure {}
pub(super) fn acp_failure(error: &anyhow::Error) -> Option<AcpFailure> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AcpFailure>().copied())
}

pub(super) struct AcpHome {
    pub(super) run: OwnedRun,
    pub(super) home: PathBuf,
    pub(super) work: PathBuf,
    pub(super) expected_prompt: String,
    pub(super) canary: Option<String>,
    pub(super) model_metadata: Value,
}

const THINKING_LEVELS: &[&str] = &[
    "off", "on", "minimal", "low", "medium", "high", "xhigh", "max",
];

pub(super) fn model_metadata(config: &toml::Value) -> Result<Value> {
    let alias = config
        .get("default_model")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow!("ACP 模型别名缺失"))?;
    let model = config
        .get("models")
        .and_then(|models| models.get(alias))
        .ok_or_else(|| anyhow!("ACP 模型元数据缺失"))?;
    let id = model
        .get("model")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| anyhow!("ACP 模型编号缺失"))?;
    let overrides = model.get("overrides");
    let field = |name: &str| {
        overrides
            .and_then(|v| v.get(name))
            .or_else(|| model.get(name))
    };
    let always = [
        model.get("capabilities"),
        overrides.and_then(|v| v.get("capabilities")),
    ]
    .into_iter()
    .flatten()
    .filter_map(toml::Value::as_array)
    .flatten()
    .any(|v| v.as_str() == Some("always_thinking"));
    let supported: Vec<_> = field("support_efforts")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter(|v| THINKING_LEVELS.contains(v))
        .collect();
    let minimum = ["minimal", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .find(|level| supported.contains(level));
    Ok(
        json!({"configured_alias":alias,"configured_model_id":id,"always_thinking":always,
        "support_efforts":supported,"min_supported_effort":minimum,"source":"isolated_config_model_metadata"}),
    )
}

pub(super) fn thinking_option(options: &Value) -> Result<Value> {
    let options = options
        .as_array()
        .ok_or_else(|| anyhow!("ACP 配置选项缺失"))?;
    let mut matches = options.iter().filter(|v| v["id"] == "thinking");
    let Some(option) = matches.next() else {
        return Ok(Value::Null);
    };
    ensure!(
        matches.next().is_none() && option["type"] == "select",
        "ACP thinking选项格式无效"
    );
    let current = option["currentValue"]
        .as_str()
        .filter(|v| THINKING_LEVELS.contains(v))
        .ok_or_else(|| anyhow!("ACP 当前thinking档位未核验"))?;
    let values = option["options"]
        .as_array()
        .ok_or_else(|| anyhow!("ACP thinking候选缺失"))?;
    ensure!(
        values.len() <= THINKING_LEVELS.len(),
        "ACP thinking候选超出上限"
    );
    let mut supported = vec![];
    for option in values {
        let value = option["value"]
            .as_str()
            .filter(|v| THINKING_LEVELS.contains(v))
            .ok_or_else(|| anyhow!("ACP thinking候选未核验"))?;
        ensure!(!supported.contains(&value), "ACP thinking候选重复");
        supported.push(value);
    }
    Ok(json!({"current_value":current,"options":supported}))
}

pub(super) fn effective_thinking_journal(bytes: &[u8]) -> Result<(Option<String>, usize)> {
    let raw = std::str::from_utf8(bytes).map_err(|_| anyhow!("ACP thinking证据编码无效"))?;
    let mut level = None;
    let mut updates = 0;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value =
            serde_json::from_str(line).map_err(|_| anyhow!("ACP thinking证据无效"))?;
        if ["profile.bind", "config.update"].contains(&s(&event, "type")) {
            ensure!(event["agentId"] == "main", "ACP thinking证据归属不一致");
            let effort = event.get("thinkingEffort");
            let legacy = event.get("thinkingLevel");
            ensure!(
                effort.is_none() || legacy.is_none() || effort == legacy,
                "ACP thinking证据字段冲突"
            );
            if let Some(value) = effort.or(legacy) {
                let value = value
                    .as_str()
                    .filter(|v| THINKING_LEVELS.contains(v))
                    .ok_or_else(|| anyhow!("ACP thinking证据档位未核验"))?;
                level = Some(value.to_owned());
                if event["type"] == "config.update" {
                    updates += 1;
                }
            }
        }
    }
    Ok((level, updates))
}

pub(super) fn prepare_home(
    provider: &KimiProvider,
    real: bool,
    learner_id: &str,
    request_id: Option<&str>,
    create_canary: bool,
) -> Result<AcpHome> {
    provider.audit_environment()?;
    let path = provider
        .runtime
        .join(format!("run-rust-acp-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&path)?;
    let run = OwnedRun {
        path: path.clone(),
        runtime: provider.runtime.clone(),
    };
    fs::write(
        path.join("owner.json"),
        json!({"ai_tutor_rust_owned":1,
        "pid":std::process::id(),"learner_id":learner_id,"request_id":request_id})
        .to_string(),
    )?;
    let home = path.join("home");
    let work = path.join("work");
    fs::create_dir_all(work.join(".kimi-code/agents"))?;
    fs::create_dir(&home)?;
    let source =
        fs::read_to_string(provider.root.join("prompts/tutor-agent.md"))?.replace("\r\n", "\n");
    ensure!(source.starts_with("---\n"), "ACP 教学 agent 格式不支持");
    let (front, body) = source[4..]
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("ACP 教学 agent 格式不支持"))?;
    ensure!(
        front
            .lines()
            .filter(|line| *line == "name: ai-tutor")
            .count()
            == 1,
        "ACP 教学 agent 名称未核验"
    );
    ensure!(
        !front.lines().any(|line| line.starts_with("override:")),
        "ACP 覆盖策略冲突"
    );
    let marker = format!("AI_TUTOR_ACP_PROFILE_{}", uuid::Uuid::new_v4().simple());
    #[cfg(test)]
    let marker = if let Some(marker) = &provider.diagnostic_profile_marker {
        ensure!(
            provider.diagnostic_model_alias.is_some()
                && marker.len() <= 96
                && marker.starts_with("AI_TUTOR_AB_PROFILE_")
                && marker
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'_'),
            "诊断成对提示标记无效"
        );
        marker.clone()
    } else {
        marker
    };
    let expected_prompt = format!("{}\n\nProfile verification marker: {marker}", body.trim());
    let overridden = format!(
        "---\n{}\noverride: true\n---\n\n{expected_prompt}\n",
        front.replace("name: ai-tutor", "name: agent")
    );
    fs::write(work.join(".kimi-code/agents/agent.md"), overridden)?;
    let agent_directory = fs::canonicalize(work.join(".kimi-code/agents"))?;
    let mut config = if real {
        provider.safe_config()?
    } else {
        toml::Value::try_from(json!({
            "default_model":"synthetic-local", "default_permission_mode":"manual",
            "telemetry":false,"auto_session_title":false,"builtin_product_skills":false,
            "merge_all_available_skills":false,"watch":{"enabled":false},
            "database":{"base":false,"search":false},"task":{"keep_alive_on_exit":false},
            "thinking":{"enabled":false},
            "loop_control":{"max_steps_per_turn":1,"max_attempts_per_step":1,"compaction_max_attempts":1},
            "providers":{"synthetic":{"type":"openai","base_url":"http://127.0.0.1:9/v1",
                "api_key":"SYNTHETIC_NOT_A_CREDENTIAL"}},
            "models":{"synthetic-local":{"provider":"synthetic","model":"synthetic",
                "max_context_size":32768,"capabilities":["tool_use"]}}
        }))?
    };
    // ACP searches project agents from the nearest .git ancestor, not cwd.
    config
        .as_table_mut()
        .ok_or_else(|| anyhow!("ACP 隔离配置不是 TOML 表"))?
        .insert(
            "extra_agent_dirs".into(),
            toml::Value::Array(vec![toml::Value::String(
                agent_directory.to_string_lossy().into_owned(),
            )]),
        );
    fs::write(home.join("config.toml"), toml::to_string(&config)?)?;
    fs::write(home.join("tui.toml"), "[upgrade]\nauto_install = false\n")?;
    if real {
        junction(
            &home.join("credentials"),
            &provider.normal_home.join("credentials"),
        )?;
    }
    let canary =
        create_canary.then(|| format!("SYNTHETIC_FILE_SECRET_{}", uuid::Uuid::new_v4().simple()));
    if let Some(value) = &canary {
        fs::write(work.join("secret-canary.txt"), value)?;
    }
    Ok(AcpHome {
        run,
        home,
        work,
        expected_prompt,
        canary,
        model_metadata: model_metadata(&config)?,
    })
}

pub(super) struct RpcReader<R> {
    inner: R,
    pub(super) pending: Vec<u8>,
    pub(super) received: usize,
    scanned: usize,
}
impl<R: AsyncRead + Unpin> RpcReader<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            pending: vec![],
            received: 0,
            scanned: 0,
        }
    }
    pub(super) async fn next(&mut self) -> Result<Value> {
        loop {
            if let Some(relative) = self.pending[self.scanned..]
                .iter()
                .position(|v| *v == b'\n')
            {
                let end = self.scanned + relative;
                ensure!(end <= LINE_LIMIT, AcpFailure::OutputLimit);
                let line: Vec<_> = self.pending.drain(..=end).collect();
                self.scanned = 0;
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                return serde_json::from_slice(&line).map_err(|_| AcpFailure::InvalidJson.into());
            }
            ensure!(self.pending.len() <= LINE_LIMIT, AcpFailure::OutputLimit);
            self.scanned = self.pending.len();
            let mut bytes = [0u8; 8192];
            let n = self
                .inner
                .read(&mut bytes)
                .await
                .map_err(|_| AcpFailure::Read)?;
            ensure!(n > 0, AcpFailure::Eof);
            ensure!(
                n <= STDOUT_LIMIT.saturating_sub(self.received),
                AcpFailure::OutputLimit
            );
            self.received += n;
            self.pending.extend_from_slice(&bytes[..n]);
        }
    }
}

pub(super) fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut bytes = vec![];
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "ACP 配置证据文件超出上限");
    Ok(bytes)
}

pub(super) fn validate_profile_journal(bytes: &[u8], expected_prompt: &str) -> Result<usize> {
    let raw = std::str::from_utf8(bytes).map_err(|_| anyhow!("ACP 有效配置证据编码无效"))?;
    ensure!(raw.ends_with('\n'), "ACP 有效配置证据尚未写完");
    let mut bindings = 0;
    let mut active = false;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value =
            serde_json::from_str(line).map_err(|_| anyhow!("ACP 有效配置证据无效"))?;
        let kind = event["type"]
            .as_str()
            .ok_or_else(|| anyhow!("ACP 证据类型缺失"))?;
        match kind {
            "profile.bind" => {
                ensure!(
                    event["agentId"] == "main" && event["profileName"] == "agent",
                    "ACP 未绑定教学默认 agent"
                );
                ensure!(
                    event["activeToolNames"]
                        .as_array()
                        .is_some_and(Vec::is_empty),
                    "ACP 有效工具列表并非空数组"
                );
                ensure!(
                    event["subagents"].as_array().is_some_and(Vec::is_empty),
                    "ACP 有效委派列表并非空数组"
                );
                ensure!(
                    event["systemPrompt"]
                        .as_str()
                        .is_some_and(|s| s.trim() == expected_prompt.trim()),
                    "ACP 有效教学策略与预期不一致"
                );
                active = true;
                bindings += 1;
            }
            "tools.set_active_tools" => {
                ensure!(
                    event["agentId"] == "main"
                        && event["names"].as_array().is_some_and(Vec::is_empty),
                    "ACP 工具列表发生变化"
                );
            }
            "tools.reset_active_tools" => bail!("ACP 工具列表被重置"),
            "config.update" => {
                ensure!(event["agentId"] == "main", "ACP 配置事件归属不一致");
                ensure!(
                    event.get("profileName").is_none_or(|v| v == "agent"),
                    "ACP agent配置发生变化"
                );
                ensure!(
                    event.get("systemPrompt").is_none_or(|v| v
                        .as_str()
                        .is_some_and(|s| s.trim() == expected_prompt.trim())),
                    "ACP 教学策略发生变化"
                );
            }
            "agent.switched" | "context.undone" => bail!("ACP 证据包含未支持的历史分支"),
            _ => {}
        }
    }
    ensure!(active && bindings > 0, "ACP 尚未提供有效空工具绑定证据");
    Ok(bindings)
}

pub(super) fn profile_path(home: &AcpHome, session: &str) -> Result<PathBuf> {
    ensure!(
        !session.is_empty()
            && session.len() <= 128
            && session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "ACP 会话编号格式无效"
    );
    let sessions = home.home.join("sessions");
    let root = fs::canonicalize(&home.home)?;
    let sessions_root = fs::canonicalize(&sessions).context("ACP 会话证据目录尚未生成")?;
    ensure!(
        sessions_root.starts_with(&root),
        "ACP 会话证据目录越出隔离目录"
    );
    let mut found = vec![];
    for entry in fs::read_dir(&sessions)
        .context("ACP 会话证据目录尚未生成")?
        .take(17)
    {
        let path = entry?.path().join(session).join("agents/main/wire.jsonl");
        if path.is_file() {
            let canonical = fs::canonicalize(&path)?;
            ensure!(
                canonical.starts_with(&sessions_root),
                "ACP 配置证据越出隔离目录"
            );
            found.push(canonical);
        }
    }
    ensure!(found.len() == 1, "ACP 有效配置证据缺失或重复");
    Ok(found.remove(0))
}

pub(super) async fn prove_profile(
    home: &AcpHome,
    session: &str,
    expected_thinking: Option<&str>,
    min_thinking_updates: usize,
) -> Result<Value> {
    let started = Instant::now();
    loop {
        let result = profile_path(home, session)
            .and_then(|path| read_bounded_file(&path, WIRE_LIMIT))
            .and_then(|bytes| {
                let bindings = validate_profile_journal(&bytes, &home.expected_prompt)?;
                let (thinking, updates) = effective_thinking_journal(&bytes)?;
                ensure!(
                    expected_thinking.is_none_or(|expected| thinking.as_deref() == Some(expected)),
                    "ACP 有效thinking与确认回执不一致"
                );
                ensure!(
                    updates >= min_thinking_updates,
                    "ACP thinking配置变更证据尚未写入"
                );
                Ok((bindings, thinking, updates))
            });
        match result {
            Ok((bindings, thinking, updates)) => {
                return Ok(
                    json!({"verified":true,"source":"isolated_main_wire_profile_bind",
                "profile_name":"agent","active_tools":[],"subagents":[],"binding_records":bindings,
                "thinking_level":thinking,
                "thinking_update_records":updates,
                "exact_system_prompt_match":true,"verification_ms":started.elapsed().as_secs_f64()*1000.0}),
                );
            }
            Err(error) if started.elapsed() >= Duration::from_secs(2) => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(40)).await,
        }
    }
}

pub(super) fn isolated_command(provider: &KimiProvider, home: &AcpHome) -> Command {
    let mut command = Command::new(&provider.executable);
    command
        .arg("acp")
        .current_dir(&home.work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for key in [
        "SystemRoot",
        "WINDIR",
        "PATH",
        "PATHEXT",
        "COMSPEC",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("KIMI_CODE_HOME", &home.home)
        .env("KIMI_DISABLE_TELEMETRY", "1")
        .env("KIMI_CODE_BUILTIN_PRODUCT_SKILLS", "false")
        .env("KIMI_CODE_WATCH", "false")
        .env("KIMI_LOOP_MAX_STEPS_PER_TURN", "1")
        .env("KIMI_LOOP_MAX_ATTEMPTS_PER_STEP", "1")
        .env("KIMI_CODE_BACKGROUND_KEEP_ALIVE_ON_EXIT", "false")
        .env("KIMI_LOG_LEVEL", "error");
    #[cfg(windows)]
    command.creation_flags(CREATION_NO_WINDOW);
    command
}
