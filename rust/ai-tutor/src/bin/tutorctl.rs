//! Explicit, local-only control plane for the Rust Tutor workstation.
//! This binary never opens a browser or launches a shell/subprocess.
use reqwest::{Client, Method, header};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode, time::Duration};

type Result<T> = std::result::Result<T, String>;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const HELP: &str = r#"AI Tutor CLI — local authenticated API control (Rust)
Usage: tutorctl [--root FOLDER] [--timeout-seconds 1..30] COMMAND [OPTIONS]

  help | --help
  status [--full]
  learners list
  learners create --name NAME --namespace real|simulator
  session select --learner ID
  session new [--learner ID]
  session context|finish|cancel [--session ID|--learner ID]
  activity report --source learner_report|operator_confirmed|simulated
      [--report TEXT] [--modules control,motor,distance,light|none]
      [--session ID|--learner ID]
  turn send --text TEXT [--session ID|--learner ID] [--wait] [--wait-seconds 1..180]
  memory list [--learner ID]
  memory correct --learner ID --memory ID --content TEXT
  memory delete --learner ID --memory ID --confirm
  camera list|pose|open|close [open: --device ID]
  camera track list|select|clear [select: --track KEY]
  audio devices|stop
  audio record start --session ID [--device NUMBER]
  audio record stop --session ID
  audio play --session ID --turn ID
  ptz status|stop
  ptz enable --camera-session ID --track KEY --target-generation NUMBER

Unspecified session targets resolve the currently active session, without switching it.
Only explicit session select/new changes the learner/activity. --session active is allowed.
Report-only commands never change modules; --modules none explicitly confirms an empty list.
turn --wait only observes the reply; it never confirms shown or starts speech.
Output is JSON except help. status is compact; --full explicitly includes learning context.
audio record stop has a separate bounded 90-second timeout for local transcription.
The CLI does not start the service, camera, microphone or model unless the command requests it.
"#;

#[derive(Debug)]
struct Options {
    root: Option<PathBuf>,
    timeout: Duration,
    command: String,
    values: BTreeMap<String, String>,
}
impl Options {
    fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
    fn flag(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    fn required(&self, key: &str) -> Result<&str> {
        self.value(key)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("缺少参数 {key}"))
    }
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut index = 0;
    let mut values = BTreeMap::new();
    // Global options may precede the command or follow it; values are never
    // reparsed as flags, so arbitrary command text remains data.
    while index < args.len() && matches!(args[index].as_str(), "--root" | "--timeout-seconds") {
        let key = args[index].clone();
        index += 1;
        let value = args.get(index).ok_or("全局参数缺少值")?.clone();
        if values.insert(key, value).is_some() {
            return Err("参数不可重复".into());
        }
        index += 1;
    }
    let first = args.get(index).map(String::as_str).unwrap_or("help");
    let count = match first {
        "help" | "--help" | "-h" | "status" => 1,
        "learners" | "session" | "activity" | "turn" | "memory" | "ptz" => 2,
        "camera" if args.get(index + 1).is_some_and(|v| v == "track") => 3,
        "audio" if args.get(index + 1).is_some_and(|v| v == "record") => 3,
        "camera" | "audio" => 2,
        _ => return Err("未知命令，请使用 tutorctl help".into()),
    };
    let command = if matches!(first, "help" | "--help" | "-h") {
        index = (index + usize::from(index < args.len())).min(args.len());
        "help".to_owned()
    } else {
        let words = args
            .get(index..index + count)
            .ok_or("命令不完整，请使用 tutorctl help")?;
        index += count;
        words.join(" ")
    };
    let (value_options, flags): (&[&str], &[&str]) = match command.as_str() {
        "help" | "learners list" | "camera list" | "camera pose" | "camera close"
        | "camera track list" | "camera track clear" | "audio devices" | "audio stop"
        | "ptz status" | "ptz stop" => (&[], &[]),
        "status" => (&[], &["--full"]),
        "learners create" => (&["--name", "--namespace"], &[]),
        "session select" | "session new" | "memory list" => (&["--learner"], &[]),
        "session context" | "session finish" | "session cancel" => {
            (&["--session", "--learner"], &[])
        }
        "activity report" => (
            &[
                "--source",
                "--report",
                "--modules",
                "--session",
                "--learner",
            ],
            &[],
        ),
        "turn send" => (
            &["--text", "--session", "--learner", "--wait-seconds"],
            &["--wait"],
        ),
        "memory correct" => (&["--learner", "--memory", "--content"], &[]),
        "memory delete" => (&["--learner", "--memory"], &["--confirm"]),
        "camera open" => (&["--device"], &[]),
        "camera track select" => (&["--track"], &[]),
        "audio record start" => (&["--session", "--device"], &[]),
        "audio record stop" => (&["--session"], &[]),
        "audio play" => (&["--session", "--turn"], &[]),
        "ptz enable" => (&["--camera-session", "--track", "--target-generation"], &[]),
        _ => return Err("未知命令，请使用 tutorctl help".into()),
    };
    while index < args.len() {
        let key = &args[index];
        index += 1;
        let value = if flags.contains(&key.as_str()) {
            String::new()
        } else if value_options.contains(&key.as_str())
            || matches!(key.as_str(), "--root" | "--timeout-seconds")
        {
            let value = args.get(index).ok_or("参数缺少值")?.clone();
            index += 1;
            value
        } else {
            return Err("存在未知参数或多余位置参数，请使用 tutorctl help".into());
        };
        if values.insert(key.clone(), value).is_some() {
            return Err("参数不可重复".into());
        }
    }
    let timeout = number(
        values
            .get("--timeout-seconds")
            .map(String::as_str)
            .unwrap_or("10"),
        1,
        30,
    )?;
    let options = Options {
        root: values.get("--root").map(PathBuf::from),
        timeout: Duration::from_secs(timeout),
        command,
        values,
    };
    validate(&options)?;
    Ok(options)
}

fn number(raw: &str, min: u64, max: u64) -> Result<u64> {
    raw.parse::<u64>()
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or("数字参数超出允许范围".into())
}
fn id(raw: &str) -> Result<&str> {
    if raw.is_empty()
        || raw.len() > 150
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("编号格式无效".into());
    }
    Ok(raw)
}
fn bounded(raw: &str, max: usize) -> Result<&str> {
    let text = raw.trim();
    if text.is_empty() || text.chars().count() > max {
        return Err("文字为空或超过长度限制".into());
    }
    Ok(text)
}
fn validate(o: &Options) -> Result<()> {
    if o.flag("--session") && o.flag("--learner") {
        return Err("--session 与 --learner 不可同时指定".into());
    }
    for key in ["--session", "--learner", "--memory", "--turn"] {
        if let Some(value) = o.value(key) {
            id(value)?;
        }
    }
    match o.command.as_str() {
        "learners create" => {
            bounded(o.required("--name")?, 48)?;
            if !matches!(o.required("--namespace")?, "real" | "simulator") {
                return Err("--namespace 只允许 real 或 simulator".into());
            }
        }
        "session select" => {
            id(o.required("--learner")?)?;
        }
        "activity report" => {
            report_payload(o)?;
        }
        "turn send" => {
            bounded(o.required("--text")?, 4000)?;
            if o.flag("--wait-seconds") && !o.flag("--wait") {
                return Err("--wait-seconds 必须与 --wait 一起使用".into());
            }
            number(o.value("--wait-seconds").unwrap_or("120"), 1, 180)?;
        }
        "memory correct" | "memory delete" => {
            id(o.required("--learner")?)?;
            id(o.required("--memory")?)?;
            if o.value("--learner") == Some("active") {
                return Err("更正或删除记忆必须提供明确档案编号".into());
            }
            if o.command == "memory correct" {
                bounded(o.required("--content")?, 800)?;
            }
            if o.command == "memory delete" && !o.flag("--confirm") {
                return Err("删除具体记忆必须显式提供 --confirm".into());
            }
        }
        "camera track select" => {
            bounded(o.required("--track")?, 150)?;
        }
        "ptz enable" => {
            ptz_enable_payload(o)?;
        }
        "audio record start" | "audio record stop" | "audio play" => {
            id(o.required("--session")?)?;
            if o.value("--session") == Some("active") {
                return Err("音频操作必须显式指定当前会话编号".into());
            }
            if o.command == "audio play" {
                id(o.required("--turn")?)?;
            }
            if let Some(device) = o.value("--device") {
                number(device, 0, 1024)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn report_payload(o: &Options) -> Result<Value> {
    let source = o.required("--source")?;
    if !matches!(
        source,
        "learner_report" | "operator_confirmed" | "simulated"
    ) {
        return Err("报告来源无效；不支持虚构硬件事件".into());
    }
    let mut payload = json!({"source_type": source});
    if let Some(report) = o.value("--report") {
        payload["report"] = json!(bounded(report, 1000)?);
    }
    if let Some(modules) = o.value("--modules") {
        let modules: Vec<_> = if modules == "none" {
            vec![]
        } else {
            modules.split(',').collect()
        };
        if modules.len() > 4
            || modules
                .iter()
                .any(|module| !matches!(*module, "control" | "motor" | "distance" | "light"))
        {
            return Err("--modules 仅允许已知模块逗号列表，或 none".into());
        }
        let mut unique = modules.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != modules.len() {
            return Err("模块不可重复".into());
        }
        payload["modules"] = json!(modules);
    }
    if payload.get("report").is_none() && payload.get("modules").is_none() {
        return Err("请提供 --report 或 --modules".into());
    }
    Ok(payload)
}

fn ptz_enable_payload(o: &Options) -> Result<Value> {
    let camera_session = bounded(o.required("--camera-session")?, 80)?;
    id(camera_session)?;
    let track = bounded(o.required("--track")?, 150)?;
    let generation = number(o.required("--target-generation")?, 0, u64::MAX)?;
    Ok(
        json!({"mode":"external_yolo","camera_session_id":camera_session,"track_key":track,"target_generation":generation}),
    )
}

fn validate_ptz_target(state: &Value, payload: &Value) -> Result<()> {
    if state["ptz"]["ready_for_follow"] != true || state["ptz"]["stop_fault"] == true {
        return Err("物理跟随尚未就绪；请检查标定、厂商模式与停止状态。此命令不能解除限制".into());
    }
    let camera = &state["camera"];
    if camera["state"] != "running"
        || camera["selected_status"] != "visible"
        || camera["association_ambiguous"] != false
    {
        return Err("当前视觉目标不可见或关联不明确；请重新选择当前画面内目标".into());
    }
    if camera["camera_session_id"] != payload["camera_session_id"]
        || camera["selected_track_key"] != payload["track_key"]
        || camera["target_generation"] != payload["target_generation"]
    {
        return Err("指定视觉目标或代次已变化；请读取最新状态并重新明确指定".into());
    }
    Ok(())
}

struct LocalApi {
    client: Client,
    base: String,
    host: String,
    token: String,
}
fn request_timeout(path: &str) -> Option<Duration> {
    (path == "/api/audio/record/stop").then_some(Duration::from_secs(90))
}
impl LocalApi {
    fn load(o: &Options) -> Result<Self> {
        let root = find_root(o.root.clone())?;
        let bytes = std::fs::read(root.join("runtime/server.json"))
            .map_err(|_| "无法读取本机服务描述文件；请先启动 AI Tutor".to_owned())?;
        if bytes.len() > 8192 {
            return Err("本机服务描述文件格式无效".into());
        }
        let descriptor: Value =
            serde_json::from_slice(&bytes).map_err(|_| "本机服务描述文件格式无效".to_owned())?;
        let port = descriptor["port"]
            .as_u64()
            .filter(|p| (1..=65535).contains(p))
            .ok_or("本机服务端口无效")?;
        let token = descriptor["token"]
            .as_str()
            .filter(|t| {
                !t.is_empty()
                    && t.len() <= 512
                    && t.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
            .ok_or("本机服务凭证格式无效")?
            .to_owned();
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(o.timeout)
            .build()
            .map_err(|_| "无法初始化本机连接".to_owned())?;
        let host = format!("127.0.0.1:{port}");
        Ok(Self {
            client,
            base: format!("http://{host}"),
            host,
            token,
        })
    }
    async fn request(&self, method: Method, path: &str, payload: Option<Value>) -> Result<Value> {
        if !path.starts_with("/api/") || path.contains(['?', '#']) {
            return Err("本机接口路径无效".into());
        }
        let mut cookie = header::HeaderValue::from_str(&format!("ai_tutor_local={}", self.token))
            .map_err(|_| "本机凭证无效".to_owned())?;
        cookie.set_sensitive(true);
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header(header::HOST, &self.host)
            .header(header::COOKIE, cookie);
        if let Some(timeout) = request_timeout(path) {
            request = request.timeout(timeout);
        }
        if let Some(payload) = payload {
            request = request.json(&payload);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "本机请求失败或超时；请检查服务是否运行".to_owned())?;
        let status = response.status();
        if status.is_redirection() {
            return Err("本机服务返回重定向，已拒绝跟随".into());
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE as u64)
        {
            return Err("本机响应超过长度限制".into());
        }
        let mut bytes = vec![];
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "读取本机响应失败或超时".to_owned())?
        {
            if bytes.len() + chunk.len() > MAX_RESPONSE {
                return Err("本机响应超过长度限制".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let data: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| "本机服务返回了无效 JSON".to_owned())?
        };
        if !status.is_success() {
            let message = data["detail"]
                .as_str()
                .or_else(|| data["error"].as_str())
                .unwrap_or("本机接口拒绝了操作");
            let message = message.replace(&self.token, "[REDACTED]");
            return Err(format!(
                "HTTP {}：{}",
                status.as_u16(),
                message.chars().take(1200).collect::<String>()
            ));
        }
        Ok(data)
    }
    async fn state(&self) -> Result<Value> {
        self.request(Method::GET, "/api/state", None).await
    }
}

fn find_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return Ok(root);
    }
    let mut candidates = vec![std::env::current_dir().map_err(|_| "无法确定项目目录".to_owned())?];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    for candidate in candidates {
        for ancestor in candidate.ancestors() {
            if ancestor.join("runtime/server.json").is_file() {
                return Ok(ancestor.to_path_buf());
            }
        }
    }
    Err("未找到本机服务，请使用 --root 指定项目目录并先启动 AI Tutor".into())
}
fn current_session<'a>(state: &'a Value, o: &Options) -> Result<&'a Value> {
    let session = &state["session"];
    if session["session_id"].as_str().is_none_or(str::is_empty) {
        return Err("当前没有学习活动，请先 session select 或 new".into());
    }
    if o.value("--session")
        .is_some_and(|sid| sid != "active" && session["session_id"].as_str() != Some(sid))
        || o.value("--learner")
            .is_some_and(|lid| lid != "active" && session["learner_id"].as_str() != Some(lid))
    {
        return Err("指定对象不是当前活动；请先显式 session select".into());
    }
    Ok(session)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let value = value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("本机状态缺少 {key}"))?;
    id(value)
}
fn select_fields(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}
fn compact_status(state: &Value) -> Value {
    json!({"ok":true,"implementation":state["implementation"],"learner_count":state["learners"].as_array().map(Vec::len).unwrap_or(0),
        "session":select_fields(&state["session"], &["session_id","learner_id","activity_id","generation","status","namespace"]),
        "camera":select_fields(&state["camera"], &["state","camera_session_id","selected_status","selected_track_key","target_generation","association_ambiguous","frame_age_ms","fps"]),
        "kimi":select_fields(&state["kimi"], &["available","version"]),
        "audio":select_fields(&state["audio"], &["recording","speaking","state","asr_ready","tts_ready"]),
        "ptz":select_fields(&state["ptz"], &["status","mode","ready_for_follow","external_yolo_enabled"]),
        "metrics":select_fields(&state["metrics"], &["requests","failed","cancelled","retries","reported_tokens","last_latency_ms","uptime_seconds","memory_mb"])})
}

async fn execute(api: &LocalApi, o: &Options) -> Result<Value> {
    match o.command.as_str() {
        "status" => { let state = api.state().await?; Ok(if o.flag("--full") { state } else { compact_status(&state) }) }
        "learners list" => Ok(api.state().await?["learners"].clone()),
        "learners create" => api.request(Method::POST,"/api/learners",Some(json!({"name":o.required("--name")?.trim(),"namespace":o.required("--namespace")?,"language":"zh","help_style":"one_step"}))).await,
        "session select" | "session new" => {
            let learner = match o.value("--learner") {
                Some(lid) if lid != "active" => lid.to_owned(),
                _ => field(current_session(&api.state().await?, o)?, "learner_id")?.to_owned(),
            };
            api.request(Method::POST,"/api/sessions",Some(json!({"learner_id":learner,"resume":o.command=="session select"}))).await
        }
        "session context" | "session finish" | "session cancel" | "activity report" | "turn send" => {
            let state = api.state().await?;
            let session = current_session(&state, o)?;
            let sid = field(session,"session_id")?;
            if o.command == "activity report" {
                return api.request(Method::POST,&format!("/api/activities/{}/events",field(session,"activity_id")?),Some(report_payload(o)?)).await;
            }
            if o.command == "turn send" {
                let text = o.required("--text")?.trim();
                let result = api.request(Method::POST,&format!("/api/sessions/{sid}/turns"),Some(json!({"text":text}))).await?;
                let request_id = result["request_id"].as_str();
                if !o.flag("--wait") || request_id.is_none() {
                    return Ok(json!({"ok":true,"session_id":sid,"request":result,"input":text,"status":"submitted","delivery_confirmed":false}));
                }
                return wait_for_turn(api, sid, request_id.unwrap(), &result["generation"], text, Duration::from_secs(number(o.value("--wait-seconds").unwrap_or("120"),1,180)?)).await;
            }
            let action = o.command.split(' ').nth(1).unwrap();
            api.request(if action=="context"{Method::GET}else{Method::POST},&format!("/api/sessions/{sid}/{action}"),if action=="context"{None}else{Some(json!({}))}).await
        }
        "memory list" | "memory correct" | "memory delete" => {
            let lid = match o.value("--learner") { Some(lid) if lid != "active" => lid.to_owned(), _ => field(current_session(&api.state().await?,o)?,"learner_id")?.to_owned() };
            let base = format!("/api/learners/{lid}/memories");
            match o.command.as_str() {
                "memory correct" => api.request(Method::PATCH,&format!("{base}/{}",o.required("--memory")?),Some(json!({"content":o.required("--content")?.trim()}))).await,
                "memory delete" => api.request(Method::DELETE,&format!("{base}/{}",o.required("--memory")?),None).await,
                _ => api.request(Method::GET,&base,None).await,
            }
        }
        "camera list" => api.request(Method::GET,"/api/devices/cameras",None).await,
        "camera pose" => api.request(Method::GET,"/api/camera/native-pose",None).await,
        "camera open" => api.request(Method::POST,"/api/camera/open",Some(o.value("--device").map(|device|json!({"device_id":device})).unwrap_or(json!({})))).await,
        "camera close" => api.request(Method::POST,"/api/camera/close",Some(json!({}))).await,
        "camera track select" => api.request(Method::POST,"/api/tracking/select",Some(json!({"track_key":o.required("--track")?}))).await,
        "camera track clear" => api.request(Method::POST,"/api/tracking/clear",Some(json!({}))).await,
        "camera track list" => { let mut frame = api.request(Method::GET,"/api/video/frame",None).await?; if let Some(frame)=frame.as_object_mut(){frame.remove("jpeg");} Ok(frame) }
        "audio devices" => api.request(Method::GET,"/api/audio/devices",None).await,
        "audio stop" => api.request(Method::POST,"/api/audio/playback/stop",Some(json!({}))).await,
        "audio record start" | "audio record stop" | "audio play" => {
            let state = api.state().await?;
            let sid = field(current_session(&state,o)?,"session_id")?;
            let (path,payload) = match o.command.as_str() {
                "audio record start" => { let mut payload=json!({"session_id":sid}); if let Some(device)=o.value("--device"){payload["device_id"]=json!(number(device,0,1024)?);} ("/api/audio/record/start",payload) },
                "audio record stop" => {
                    let generation = state["audio"]["recording_generation"].as_u64().ok_or("音频状态缺少录音代次，请更新程序")?;
                    ("/api/audio/record/stop",json!({"session_id":sid,"recording_generation":generation}))
                },
                _ => ("/api/audio/playback",json!({"session_id":sid,"turn_id":o.required("--turn")?})),
            };
            api.request(Method::POST,path,Some(payload)).await
        }
        "ptz status" => Ok(api.state().await?["ptz"].clone()),
        "ptz stop" => api.request(Method::POST,"/api/ptz/stop",Some(json!({}))).await,
        "ptz enable" => {
            let payload = ptz_enable_payload(o)?;
            validate_ptz_target(&api.state().await?, &payload)?;
            api.request(Method::POST, "/api/ptz/enable", Some(payload)).await
        }
        _ => Err("未知命令".into()),
    }
}

fn turn_snapshot(state: &Value, sid: &str, rid: &str, generation: &Value) -> Option<Value> {
    let session = &state["session"];
    if session["session_id"] != sid || &session["generation"] != generation {
        return Some(
            json!({"ok":false,"status":"superseded","reply":null,"delivery_confirmed":false}),
        );
    }
    if let Some(turn) = session["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|turn| turn["role"] == "assistant" && turn["request_id"] == rid)
    {
        if turn["status"] == "streaming" {
            let stream = &session["tutor_stream"];
            if stream["request_id"] != rid || &stream["generation"] != generation {
                return None;
            }
            let phase = stream["phase"].as_str();
            if phase == Some("generating") {
                return None;
            }
            let completed = phase == Some("completed");
            let generated = matches!(phase, Some("final_pending" | "completed"));
            return Some(
                json!({"ok":generated,"status":if generated{"generated"}else{phase.unwrap_or("error")},"reply":select_fields(turn,&["turn_id","content","status","generation"]),"delivery_confirmed":completed}),
            );
        }
        let status = turn["status"].as_str();
        return Some(
            json!({"ok":!matches!(status,Some("cancelled"|"failed"|"interrupted")),"status":turn["status"],"reply":select_fields(turn,&["turn_id","content","status","generation"]),"delivery_confirmed":matches!(status,Some("shown"|"speech_started"|"speech_completed"|"speech_interrupted"))}),
        );
    }
    match session["status"].as_str() {
        Some("error" | "finished") => Some(
            json!({"ok":false,"status":session["status"],"reply":null,"delivery_confirmed":false}),
        ),
        _ => None,
    }
}
fn validated_segment_text(stream: &Value) -> Option<String> {
    if stream["final_validated"] != true {
        return None;
    }
    let mut segments = stream["segments"].as_array()?.clone();
    if segments.is_empty() {
        return None;
    }
    segments.sort_by_key(|segment| segment["sequence"].as_u64());
    Some(
        segments
            .iter()
            .filter_map(|segment| segment["text"].as_str())
            .collect(),
    )
}

async fn wait_for_turn(
    api: &LocalApi,
    sid: &str,
    rid: &str,
    generation: &Value,
    text: &str,
    limit: Duration,
) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(
                json!({"ok":false,"session_id":sid,"request_id":rid,"generation":generation,"input":text,"status":"timeout","reply":null,"delivery_confirmed":false}),
            );
        }
        let state = match tokio::time::timeout(remaining, api.state()).await {
            Ok(state) => state?,
            Err(_) => {
                return Ok(
                    json!({"ok":false,"session_id":sid,"request_id":rid,"generation":generation,"input":text,"status":"timeout","reply":null,"delivery_confirmed":false}),
                );
            }
        };
        if let Some(mut result) = turn_snapshot(&state, sid, rid, generation) {
            if result["ok"] == true && result["reply"]["status"] == "streaming" {
                let stream = api
                    .request(
                        Method::GET,
                        &format!("/api/sessions/{sid}/streams/{rid}"),
                        None,
                    )
                    .await?;
                if let Some(content) = validated_segment_text(&stream) {
                    result["reply"]["content"] = json!(content);
                    result["reply"]["status"] = json!("generated");
                }
            }
            result["session_id"] = json!(sid);
            result["request_id"] = json!(rid);
            result["generation"] = generation.clone();
            result["input"] = json!(text);
            return Ok(result);
        }
        tokio::time::sleep(
            Duration::from_millis(350)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
}

fn redact(value: Value, token: &str) -> Value {
    match value {
        Value::String(text) => Value::String(if token.is_empty() {
            text
        } else {
            text.replace(token, "[REDACTED]")
        }),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact(value, token))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let secret = matches!(
                        key.to_ascii_lowercase().as_str(),
                        "token"
                            | "access_token"
                            | "api_key"
                            | "authorization"
                            | "cookie"
                            | "secret"
                    );
                    (
                        if token.is_empty() {
                            key
                        } else {
                            key.replace(token, "[REDACTED]")
                        },
                        if secret {
                            json!("[REDACTED]")
                        } else {
                            redact(value, token)
                        },
                    )
                })
                .collect(),
        ),
        other => other,
    }
}
#[tokio::main]
async fn main() -> ExitCode {
    let args = match std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string().map_err(|_| ()))
        .collect::<std::result::Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(_) => {
            eprintln!("{}", json!({"ok":false,"error":"命令行包含无法读取的文字"}));
            return ExitCode::from(1);
        }
    };
    let options = match parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{}", json!({"ok":false,"error":error}));
            return ExitCode::from(1);
        }
    };
    if options.command == "help" {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let api = match LocalApi::load(&options) {
        Ok(api) => api,
        Err(error) => {
            eprintln!("{}", json!({"ok":false,"error":error}));
            return ExitCode::from(1);
        }
    };
    match execute(&api, &options).await {
        Ok(value) => {
            let failed = value["ok"] == false;
            println!("{}", redact(value, &api.token));
            if failed {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("{}", redact(json!({"ok":false,"error":error}), &api.token));
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[test]
    fn only_local_transcription_overrides_the_ordinary_request_timeout() {
        assert_eq!(
            request_timeout("/api/audio/record/stop"),
            Some(Duration::from_secs(90))
        );
        for path in [
            "/api/state",
            "/api/audio/record/start",
            "/api/audio/playback/stop",
            "/api/sessions/s/turns",
        ] {
            assert_eq!(
                request_timeout(path),
                None,
                "ordinary requests keep their client timeout"
            );
        }
    }
    fn opts(args: &[&str]) -> Options {
        parse(args.iter().map(|v| (*v).into()).collect()).unwrap()
    }
    fn ptz_options() -> Options {
        opts(&[
            "ptz",
            "enable",
            "--camera-session",
            "camera_a",
            "--track",
            "camera_a:track-7",
            "--target-generation",
            "7",
        ])
    }
    fn ready_ptz_state() -> Value {
        json!({"camera":{"state":"running","camera_session_id":"camera_a","selected_track_key":"camera_a:track-7","target_generation":7,"selected_status":"visible","association_ambiguous":false},"ptz":{"ready_for_follow":true,"stop_fault":false}})
    }
    #[test]
    fn ptz_enable_requires_explicit_target_and_rejects_bypass_options() {
        assert!(parse(vec!["ptz".into(), "enable".into()]).is_err());
        let options = ptz_options();
        let mut arguments = vec![
            "ptz",
            "enable",
            "--camera-session",
            "camera_a",
            "--track",
            "camera_a:track-7",
            "--target-generation",
            "7",
            "--force",
        ];
        assert!(parse(arguments.iter().map(|v| (*v).to_owned()).collect()).is_err());
        arguments.pop();
        arguments.pop();
        arguments.push("-1");
        assert!(parse(arguments.iter().map(|v| (*v).to_owned()).collect()).is_err());
        assert_eq!(
            ptz_enable_payload(&options).unwrap(),
            json!({"mode":"external_yolo","camera_session_id":"camera_a","track_key":"camera_a:track-7","target_generation":7})
        );
    }
    #[test]
    fn ptz_preflight_rejects_unready_lost_ambiguous_and_stale_targets() {
        let payload = ptz_enable_payload(&ptz_options()).unwrap();
        assert!(validate_ptz_target(&ready_ptz_state(), &payload).is_ok());
        for (section, key, value) in [
            ("ptz", "ready_for_follow", json!(false)),
            ("ptz", "stop_fault", json!(true)),
            ("camera", "state", json!("closed")),
            ("camera", "selected_status", json!("unselected")),
            ("camera", "association_ambiguous", json!(true)),
            ("camera", "camera_session_id", json!("camera_b")),
            ("camera", "selected_track_key", json!("other")),
            ("camera", "target_generation", json!(8)),
        ] {
            let mut state = ready_ptz_state();
            state[section][key] = value;
            assert!(validate_ptz_target(&state, &payload).is_err());
        }
    }
    #[tokio::test]
    async fn unready_ptz_never_sends_an_enable_request() {
        let mut state = ready_ptz_state();
        state["ptz"]["ready_for_follow"] = json!(false);
        let (api, task) = http_fixture("200 OK", state).await;
        assert!(execute(&api, &ptz_options()).await.is_err());
        assert!(task.await.unwrap().starts_with("GET /api/state HTTP/1.1"));
    }
    #[tokio::test]
    async fn ptz_enable_contract_checks_state_then_calls_only_existing_guarded_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let api = LocalApi {
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            base: format!("http://{host}"),
            host,
            token: "synthetic-cli-test-token".into(),
        };
        let task = tokio::spawn(async move {
            let mut requests = vec![];
            for data in [
                ready_ptz_state(),
                json!({"status":"ARMING","mode":"external_yolo"}),
            ] {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(2), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = vec![];
                let (end, length) = loop {
                    let mut chunk = [0u8; 2048];
                    let count =
                        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
                            .await
                            .unwrap()
                            .unwrap();
                    assert!(count > 0 && bytes.len() + count <= 16384);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while bytes.len() < end + length {
                    let mut chunk = [0u8; 2048];
                    let count =
                        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
                            .await
                            .unwrap()
                            .unwrap();
                    assert!(count > 0 && bytes.len() + count <= 16384);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let route = String::from_utf8_lossy(&bytes[..end])
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned();
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[end..end + length]).unwrap()
                };
                requests.push((route, body));
                let body = data.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
            requests
        });
        let options = ptz_options();
        assert_eq!(execute(&api, &options).await.unwrap()["status"], "ARMING");
        let requests = task.await.unwrap();
        assert_eq!(requests[0].0, "GET /api/state HTTP/1.1");
        assert_eq!(requests[1].0, "POST /api/ptz/enable HTTP/1.1");
        assert_eq!(requests[1].1, ptz_enable_payload(&options).unwrap());
    }
    async fn http_fixture(
        status: &str,
        data: Value,
    ) -> (LocalApi, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("runtime")).unwrap();
        std::fs::write(
            root.path().join("runtime/server.json"),
            json!({"port":port,"token":"synthetic-cli-test-token"}).to_string(),
        )
        .unwrap();
        let mut options = opts(&["status"]);
        options.root = Some(root.path().into());
        let api = LocalApi::load(&options).unwrap();
        let status = status.to_owned();
        let task = tokio::spawn(async move {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let size = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(size > 0 && bytes.len() + size < 16384);
                bytes.extend_from_slice(&chunk[..size]);
                if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            let body = data.to_string();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nLocation: http://127.0.0.1:{port}/redirected\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            drop(socket);
            // A wait must not POST shown; a redirect must not forward credentials.
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
            String::from_utf8(bytes).unwrap()
        });
        (api, task)
    }
    #[tokio::test]
    async fn waiting_only_gets_state_and_never_confirms_delivery() {
        let state = json!({"session":{"session_id":"s","generation":1,"status":"idle","turns":[{"role":"assistant","request_id":"r","turn_id":"t","status":"generated","content":"只调整停车距离"}]}});
        let (api, task) = http_fixture("200 OK", state).await;
        let result = wait_for_turn(&api, "s", "r", &json!(1), "测试", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(result["status"], "generated");
        assert_eq!(result["delivery_confirmed"], false);
        assert!(task.await.unwrap().starts_with("GET /api/state HTTP/1.1"));
    }
    #[tokio::test]
    async fn redirects_are_rejected_without_a_second_request() {
        let (api, task) = http_fixture("302 Found", json!({"detail":"redirect"})).await;
        assert!(api.state().await.unwrap_err().contains("重定向"));
        task.await.unwrap();
    }
    #[test]
    fn strict_arguments_and_confirmed_deletion() {
        for args in [
            vec!["statuz"],
            vec!["status", "--wat"],
            vec!["status", "--full", "--full"],
            vec!["memory", "delete", "--learner", "l", "--memory", "m"],
            vec!["ptz", "enable"],
            vec!["turn", "send", "--text", "test", "--wait-seconds", "999"],
            vec!["session", "context", "--session", "../other"],
        ] {
            assert!(parse(args.into_iter().map(String::from).collect()).is_err());
        }
        assert!(parse(vec!["--help".into()]).is_ok());
    }
    #[test]
    fn report_only_omits_inventory_and_none_is_explicit_empty() {
        let o = opts(&[
            "activity",
            "report",
            "--source",
            "operator_confirmed",
            "--report",
            "第二次停车更早",
        ]);
        let p = report_payload(&o).unwrap();
        assert!(p.get("modules").is_none());
        assert_eq!(p["report"], "第二次停车更早");
        let o = opts(&[
            "activity",
            "report",
            "--source",
            "learner_report",
            "--modules",
            "none",
        ]);
        assert_eq!(report_payload(&o).unwrap()["modules"], json!([]));
    }
    #[test]
    fn shell_like_text_is_only_json_data() {
        let raw = "$(Get-Content private) `echo` & test; --root";
        let o = opts(&["turn", "send", "--text", raw]);
        assert_eq!(o.required("--text").unwrap(), raw);
        assert!(
            serde_json::to_string(&json!({"text":o.required("--text").unwrap()}))
                .unwrap()
                .contains("private")
        );
    }
    #[test]
    fn compact_status_does_not_contain_names_chat_or_memory() {
        let full = json!({"learners":[{"name":"PRIVATE NAME"}],"session":{"session_id":"s","state":{"goal":"PRIVATE GOAL"},"turns":[{"content":"PRIVATE CHAT"}],"recalled_memories":["PRIVATE MEMORY"]}});
        let compact = compact_status(&full).to_string();
        assert!(!compact.contains("PRIVATE"));
        assert!(compact.contains("learner_count"));
    }
    #[test]
    fn validated_segments_supply_reply_text_without_a_display_receipt() {
        let mut stream = json!({"final_validated":true,"segments":[{"sequence":1,"text":"第二句"},{"sequence":0,"text":"第一句"}]});
        assert_eq!(validated_segment_text(&stream).as_deref(), Some("第一句第二句"));
        stream["final_validated"] = json!(false);
        assert_eq!(validated_segment_text(&stream), None);
        stream["final_validated"] = json!(true);
        stream["segments"] = json!([]);
        assert_eq!(validated_segment_text(&stream), None);
    }

    #[test]
    fn segmented_wait_uses_stream_phase_without_treating_failure_or_prefix_as_success() {
        let mut state = json!({"session":{"session_id":"s","generation":2,"status":"generating","turns":[{"role":"assistant","request_id":"r","turn_id":"t","status":"streaming","content":"已显示前缀","generation":2}],"tutor_stream":{"request_id":"r","generation":2,"phase":"generating"}}});
        assert!(turn_snapshot(&state, "s", "r", &json!(2)).is_none());
        state["session"]["tutor_stream"]["phase"] = json!("final_pending");
        let pending = turn_snapshot(&state, "s", "r", &json!(2)).unwrap();
        assert_eq!(pending["ok"], true);
        assert_eq!(pending["status"], "generated");
        assert_eq!(pending["delivery_confirmed"], false);
        state["session"]["tutor_stream"]["phase"] = json!("completed");
        let completed = turn_snapshot(&state, "s", "r", &json!(2)).unwrap();
        assert_eq!(completed["ok"], true);
        assert_eq!(completed["delivery_confirmed"], true);
        for phase in ["failed", "cancelled", "interrupted"] {
            state["session"]["tutor_stream"]["phase"] = json!(phase);
            let failed = turn_snapshot(&state, "s", "r", &json!(2)).unwrap();
            assert_eq!(failed["ok"], false, "{phase}");
            assert_eq!(failed["status"], phase);
            assert_eq!(failed["delivery_confirmed"], false);
            assert_eq!(failed["reply"]["content"], "已显示前缀");
        }
    }

    #[test]
    fn no_cli_delivery_confirmation_is_inferred_from_generated_reply() {
        let state = json!({"session":{"session_id":"s","generation":3,"status":"idle","turns":[{"role":"assistant","request_id":"r","status":"generated","content":"下一步","turn_id":"t"}]}});
        let v = turn_snapshot(&state, "s", "r", &json!(3)).unwrap();
        assert_eq!(v["delivery_confirmed"], false);
        assert_eq!(v["status"], "generated");
        assert_eq!(
            turn_snapshot(&state, "s", "old", &json!(2)).unwrap()["status"],
            "superseded"
        );
    }
    #[test]
    fn output_and_errors_redact_tokens_recursively() {
        let token = "fixture-secret-token";
        let value = redact(
            json!({"error":format!("bad credential {token}"),"nested":{"token":token,"cookie":"other secret","reported_tokens":37},"text":[token]}),
            token,
        );
        assert!(!value.to_string().contains(token));
        assert!(!value.to_string().contains("other secret"));
        assert_eq!(value["nested"]["reported_tokens"], 37);
    }
    #[test]
    fn explicit_mismatched_session_never_switches_learner() {
        let state = json!({"session":{"session_id":"s","learner_id":"a"}});
        assert!(
            current_session(&state, &opts(&["session", "context", "--session", "other"])).is_err()
        );
        assert!(current_session(&state, &opts(&["session", "context", "--learner", "b"])).is_err());
    }
}
