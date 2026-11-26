//! Native real-Kimi acceptance harness. All learning fixtures are SIMULATED.
use ai_tutor::store::s;
use anyhow::{Result, anyhow, bail, ensure};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

struct Acceptance {
    root: PathBuf,
    client: Client,
    base: String,
    cookie: String,
    checks: Vec<Value>,
    turns: Vec<Value>,
}
impl Acceptance {
    fn new(root: PathBuf) -> Result<Self> {
        let descriptor: Value =
            serde_json::from_slice(&std::fs::read(root.join("runtime/server.json"))?)?;
        ensure!(
            descriptor["implementation"] == "Rust",
            "本次验收只允许原生 Rust 服务"
        );
        let port = descriptor["port"]
            .as_u64()
            .ok_or_else(|| anyhow!("本机服务端口缺失"))?;
        let token = descriptor["token"]
            .as_str()
            .ok_or_else(|| anyhow!("本机服务凭证缺失"))?;
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            root,
            client,
            base: format!("http://127.0.0.1:{port}"),
            cookie: format!("ai_tutor_local={token}"),
            checks: vec![],
            turns: vec![],
        })
    }
    async fn api(&self, method: Method, path: &str, data: Value) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("Cookie", &self.cookie);
        if !data.is_null() {
            request = request.json(&data)
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow!("本机验收请求连接失败"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|_| anyhow!("本机接口返回格式无效"))?;
        ensure!(
            status.is_success(),
            "本机接口返回{status}: {}",
            s(&value, "detail")
        );
        Ok(value)
    }
    async fn state(&self) -> Result<Value> {
        self.api(Method::GET, "/api/state", Value::Null).await
    }
    fn check(&mut self, name: &str, condition: bool, detail: &str) {
        println!("{} {name}", if condition { "PASS" } else { "FAIL" });
        self.checks
            .push(json!({"name":name,"status":if condition{"PASS"}else{"FAIL"},"detail":detail}));
    }
    async fn select(&self, lid: &str, resume: bool) -> Result<Value> {
        self.api(
            Method::POST,
            "/api/sessions",
            json!({"learner_id":lid,"resume":resume}),
        )
        .await
    }
    async fn learner(&self, name: &str) -> Result<Value> {
        self.api(
            Method::POST,
            "/api/learners",
            json!({"name":name,"namespace":"simulator","language":"zh","help_style":"one_step"}),
        )
        .await
    }
    async fn turn(&mut self, sid: &str, text: &str, label: &str) -> Result<Value> {
        let started = Instant::now();
        let request = self
            .api(
                Method::POST,
                &format!("/api/sessions/{sid}/turns"),
                json!({"text":text}),
            )
            .await?;
        let rid = s(&request, "request_id");
        ensure!(!rid.is_empty(), "本轮没有发起模型请求");
        while started.elapsed() < Duration::from_secs(105) {
            let state = self.state().await?;
            let session = &state["session"];
            ensure!(s(session, "session_id") == sid, "验收期间活动被切换");
            if let Some(reply) = session["turns"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|turn| s(turn, "request_id") == rid && s(turn, "role") == "assistant")
            {
                self.api(
                    Method::POST,
                    &format!("/api/sessions/{sid}/delivery"),
                    json!({"turn_id":reply["turn_id"],"status":"shown"}),
                )
                .await?;
                println!("{label}: {}", s(reply, "content"));
                self.turns.push(json!({"label":label,"input":text,"reply":reply["content"],"latency_ms":started.elapsed().as_millis(),"request_id":rid}));
                return Ok(self.state().await?["session"].clone());
            }
            if s(session, "status") == "error" {
                bail!("真实模型调用失败；错误记录：{}", state["errors"])
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        bail!("真实教学请求超时")
    }
    async fn inventory(&self, session: &Value, modules: Value) -> Result<Value> {
        self.api(
            Method::POST,
            &format!("/api/activities/{}/events", s(session, "activity_id")),
            json!({"modules":modules,"source_type":"simulated"}),
        )
        .await
    }
    fn report(&self, suffix: &str) -> Result<()> {
        let mut output = format!(
            "# Rust 原生真实 Kimi 教学 API 验收{suffix}\n\n时间：{}（UTC+08:00）。\n\n本报告通过 Rust 验收程序调用运行中的 Rust 服务与真实认证 Kimi Code2.3.1。全部学习者与小车试跑描述都是**合成模拟输入**，严格写入 simulator 独立空间；不声称成人/儿童实际参与、实体小车运行或真人语音完成。摄像头运行独立，本程序不打开、关闭或保存摄像头。显示确认由验收程序提交，不替代人工 UI 验收。\n\n| 项目 | 状态 | 证据 |\n|---|---|---|\n",
            chrono::Utc::now()
                .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                .to_rfc3339()
        );
        for check in &self.checks {
            output.push_str(&format!(
                "| {} | {} | {} |\n",
                s(check, "name"),
                s(check, "status"),
                s(check, "detail").replace('|', "/")
            ));
        }
        output.push_str("\n## 实际模型对话\n\n");
        for turn in &self.turns {
            output.push_str(&format!(
                "### {} — {}ms\n\n输入：{}\n\n真实 Kimi：{}\n\n",
                s(turn, "label"),
                turn["latency_ms"],
                s(turn, "input"),
                s(turn, "reply")
            ));
        }
        let path = self.root.join("docs/RUST_E2E_REPORT.md");
        if !suffix.is_empty() && path.exists() {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
            write!(file, "\n\n{output}")?;
        } else {
            std::fs::write(path, output)?;
        }
        Ok(())
    }
    async fn run(&mut self) -> Result<()> {
        let state = self.state().await?;
        self.check(
            "Rust 服务与真实 Kimi 已验证",
            state["implementation"]
                .as_str()
                .is_some_and(|s| s.starts_with("Rust"))
                && state["kimi"]["available"] == true,
            "Rust native provider verification",
        );
        let a = self.learner("模拟验收 Synthetic A").await?;
        let b = self.learner("模拟验收 Synthetic B").await?;
        let aid = s(&a, "learner_id");
        let bid = s(&b, "learner_id");
        let session = self.select(aid, false).await?;
        let sid = s(&session, "session_id");
        let session = self
            .turn(sid, "我想让小车到墙前停下来，再亮绿灯。", "1. 目标")
            .await?;
        self.inventory(&session, json!(["control", "motor", "light"]))
            .await?;
        let session = self
            .turn(
                sid,
                "这是模拟搭建，当前报告有主控、电机和灯。我还缺少什么能力？给我一个提示。",
                "2. 缺少测距能力",
            )
            .await?;
        self.check(
            "模块信息来源持续标为模拟",
            session["state"]["modules_source"] == "simulated",
            "hardware_verified=false",
        );
        self.turn(
            sid,
            "我报告测距模块接好了；这是模拟活动中的口头报告，尚未硬件验证。",
            "3. 报告连接",
        )
        .await?;
        let session=self.turn(sid,"第一次模拟试跑，据我报告它能停下来，也亮绿灯，但是离墙太近，想要大一点的间隙。请记住第一次据报告已经成功。","4. 成功后改进").await?;
        let result = s(&session["state"], "reported_result");
        self.check(
            "首轮报告成功没有误记为失败",
            !result.is_empty() && !result.contains("失败"),
            result,
        );
        self.turn(
            sid,
            "这次一次只告诉我一步，我想让它更早停。",
            "5. 当次帮助偏好",
        )
        .await?;
        let memories = self
            .api(
                Method::GET,
                &format!("/api/learners/{aid}/memories"),
                Value::Null,
            )
            .await?;
        self.check(
            "当次偏好不固化为永久特征",
            !memories.as_array().unwrap().iter().any(|m| {
                s(m, "kind") == "help_preference" && s(m, "scope") == "cross_session_explicit"
            }),
            "",
        );
        let session=self.turn(sid,"我只改了停车距离，其他条件一样。第二次模拟试跑，据我观察更早停下、亮绿灯，离墙间隙更大了。请记住进展；下次我想移动墙，再测试是否仍按距离停车，这个实验还没做。","6. 比较与未完成实验").await?;
        let memories = self
            .api(
                Method::GET,
                &format!("/api/learners/{aid}/memories"),
                Value::Null,
            )
            .await?;
        self.check(
            "快记忆保存已显示提示",
            session["state"]["last_delivered_hint"] == self.turns.last().unwrap()["reply"],
            "",
        );
        self.check(
            "慢记忆保存有来源的模拟进展",
            memories.as_array().unwrap().iter().any(|m| {
                s(m, "kind") == "reported_progress"
                    && s(m, "source_type") == "simulated"
                    && !m["source_event_ids"].as_array().unwrap().is_empty()
            }),
            "",
        );
        self.check(
            "保留移动墙的待做实验",
            memories
                .as_array()
                .unwrap()
                .iter()
                .any(|m| s(m, "kind") == "pending_experiment" && s(m, "content").contains('墙')),
            "",
        );
        self.check(
            "模拟记忆没有提升为真实来源",
            memories
                .as_array()
                .unwrap()
                .iter()
                .all(|m| s(m, "source_type") == "simulated"),
            "",
        );
        self.api(
            Method::POST,
            &format!("/api/sessions/{sid}/finish"),
            json!({}),
        )
        .await?;
        let second = self.select(aid, false).await?;
        let second_sid = s(&second, "session_id");
        self.check(
            "新活动确认当前搭建而不恢复历史连接",
            second["state"]["modules"].is_null()
                && second["state"]["requires_inventory_confirmation"] == true,
            "",
        );
        self.check(
            "新活动选择性召回历史",
            !second["recalled_memories"].as_array().unwrap().is_empty(),
            "",
        );
        self.turn(
            second_sid,
            "继续上次的小车停车距离活动。我们上次还留下了什么没做？先确认今天搭建。",
            "7. 跨活动召回",
        )
        .await?;
        let other = self.select(bid, false).await?;
        self.check(
            "B 没有 A 的对话或记忆",
            other["turns"].as_array().unwrap().is_empty()
                && other["recalled_memories"].as_array().unwrap().is_empty(),
            "",
        );
        let a_session = self.select(aid, true).await?;
        self.api(
            Method::POST,
            &format!("/api/sessions/{}/turns", s(&a_session, "session_id")),
            json!({"text":"请继续解释如何只改变一个变量。"}),
        )
        .await?;
        self.select(bid, true).await?;
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let switched = self.state().await?;
        self.check(
            "切档案取消旧请求且 B 保持空",
            switched["session"]["learner_id"] == bid
                && switched["session"]["turns"].as_array().unwrap().is_empty()
                && switched["kimi"]["running"] == 0,
            "",
        );
        let c = self.learner("模拟验收更正删除").await?;
        let cid = s(&c, "learner_id");
        let fixture = self.select(cid, false).await?;
        let csid = s(&fixture, "session_id");
        self.turn(
            csid,
            "请记住我的模拟口头报告：第一次小车停车并亮绿灯成功，移动墙实验尚未做。",
            "8. 更正测试初始报告",
        )
        .await?;
        let before = self
            .api(
                Method::GET,
                &format!("/api/learners/{cid}/memories"),
                Value::Null,
            )
            .await?;
        let old_progress = before
            .as_array()
            .unwrap()
            .iter()
            .find(|m| s(m, "kind") == "reported_progress")
            .map(|m| s(m, "memory_id").to_owned());
        self.turn(
            csid,
            "刚才说错了，第一次并没有停下来，绿灯也没有亮。请更正刚才那条停车成功的模拟记录。",
            "9. 自然语言更正",
        )
        .await?;
        let corrected = self
            .api(
                Method::GET,
                &format!("/api/learners/{cid}/memories"),
                Value::Null,
            )
            .await?;
        self.check(
            "自然更正使原成功记录退出活动召回",
            old_progress.as_ref().is_some_and(|old| {
                !corrected
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| s(m, "memory_id") == old)
            }) && corrected
                .as_array()
                .unwrap()
                .iter()
                .any(|m| s(m, "kind") == "correction"),
            "",
        );
        if let Some(memory) = corrected.as_array().unwrap().first() {
            let old = s(memory, "memory_id");
            let replacement = self
                .api(
                    Method::PATCH,
                    &format!("/api/learners/{cid}/memories/{old}"),
                    json!({"content":"教师更正：仅有模拟口头报告，实体硬件未验证。"}),
                )
                .await?;
            self.check(
                "教师可再次更正记忆",
                !s(&replacement, "memory_id").is_empty(),
                "",
            );
        }
        self.api(
            Method::DELETE,
            &format!("/api/learners/{cid}/data"),
            Value::Null,
        )
        .await?;
        let deleted = self
            .api(
                Method::GET,
                &format!("/api/learners/{cid}/memories"),
                Value::Null,
            )
            .await?;
        self.check(
            "删除清除记忆和派生对话",
            deleted.as_array().unwrap().is_empty()
                && self.state().await?["session"]["turns"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            "",
        );
        self.select(aid, true).await?;
        std::fs::write(
            self.root.join("runtime/e2e-rust-state.json"),
            serde_json::to_vec_pretty(
                &json!({"learner_a":aid,"learner_b":bid,"session_one":sid,"session_two":second_sid,"namespace":"simulator","memories":memories}),
            )?,
        )?;
        self.report("")?;
        ensure!(
            self.checks.iter().all(|c| c["status"] == "PASS"),
            "原生验收存在失败项目，请查看报告"
        );
        Ok(())
    }
    /// A bounded real-model regression for the current product lifecycle changes.
    /// Teacher observations deliberately finish without an additional model call.
    async fn workflow(&mut self) -> Result<()> {
        let before = self.state().await?;
        ensure!(
            before["kimi"]["available"] == true,
            "Kimi 尚未通过无工具运行核验"
        );
        let a = self.learner("产品流程验收 Synthetic A").await?;
        let b = self.learner("产品流程验收 Synthetic B").await?;
        let lid = s(&a, "learner_id");
        let session = self.select(lid, false).await?;
        let sid = s(&session, "session_id");
        self.turn(
            sid,
            "这是模拟活动。我想让小车到墙前停下来，再亮绿灯。",
            "产品回归1：表达目标",
        )
        .await?;
        self.inventory(&session, json!(["control", "motor", "distance", "light"]))
            .await?;
        let first = self.turn(sid, "第一次模拟试跑，据我报告已能停车和亮绿灯，但是离墙太近。我想要更大的间隙，这次只给我一个修改建议。", "产品回归2：成功后改进").await?;
        self.check(
            "据报告成功后仍可继续改进",
            !s(&first["state"], "reported_result").is_empty()
                && !s(&first["state"], "reported_result").contains("失败"),
            "真实Kimi活动更新，来源为simulated",
        );
        let final_observation = "第二次模拟试验只改了停车距离，其他条件不变；小车更早停车，绿灯正常，间隙更大。下次移动墙的位置再测试，这个实验还没做。";
        let final_event = self
            .api(
                Method::POST,
                &format!("/api/activities/{}/events", s(&session, "activity_id")),
                json!({"source_type":"simulated","report":final_observation}),
            )
            .await?;
        self.check(
            "只补报告保留已记录模块",
            self.state().await?["session"]["state"]["modules"]
                == json!(["control", "motor", "distance", "light"]),
            "未提交modules字段",
        );
        self.api(
            Method::POST,
            &format!("/api/sessions/{sid}/finish"),
            json!({}),
        )
        .await?;
        let memories = self
            .api(
                Method::GET,
                &format!("/api/learners/{lid}/memories"),
                Value::Null,
            )
            .await?;
        let saved = memories
            .as_array()
            .unwrap()
            .iter()
            .find(|m| s(m, "content").contains(final_observation));
        self.check(
            "结束时保存最后教师观察及来源",
            saved.is_some_and(|m| {
                m["source_type"] == "simulated"
                    && m["source_event_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&final_event["event_id"]))
            }),
            "没有为收尾额外调用模型，没有宣称硬件验证",
        );
        self.api(
            Method::POST,
            &format!("/api/sessions/{sid}/finish"),
            json!({}),
        )
        .await?;
        let again = self
            .api(
                Method::GET,
                &format!("/api/learners/{lid}/memories"),
                Value::Null,
            )
            .await?;
        self.check(
            "结束操作幂等",
            memories.as_array().unwrap().len() == again.as_array().unwrap().len(),
            "重复结束不增加重复记忆",
        );
        let next = self.select(lid, true).await?;
        let next_sid = s(&next, "session_id");
        self.check(
            "新活动召回最终观察且重新确认模块",
            next["session_id"] != session["session_id"]
                && next["state"]["modules"].is_null()
                && next["state"]["requires_inventory_confirmation"] == true
                && next["recalled_memories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| s(m, "content").contains(final_observation)),
            "历史配置不会被当成当前连接",
        );
        self.turn(
            next_sid,
            "继续上次的小车活动。上次的第二次试验结果和留到今天的实验是什么？请先确认今天的模块。",
            "产品回归3：下一次继续",
        )
        .await?;
        self.api(Method::POST, &format!("/api/activities/{}/events",s(&next,"activity_id")), json!({"source_type":"simulated","modules":["control","motor","distance","light"],"report":"今天重新确认四个模块，当前尚未试跑。"})).await?;
        if let Some(saved) = saved {
            self.api(
                Method::PATCH,
                &format!("/api/learners/{lid}/memories/{}", s(saved, "memory_id")),
                json!({"content":"教师更正：上次第二次试验的间隙记录有误；移动墙实验仍未做。"}),
            )
            .await?;
            let state = self.state().await?;
            self.check(
                "更正历史保留今天的独立事实",
                state["session"]["state"]["modules"]
                    == json!(["control", "motor", "distance", "light"])
                    && state["session"]["state"]["reported_result"]
                        == "今天重新确认四个模块，当前尚未试跑。",
                "历史更正不清空整个活动",
            );
        }
        let other = self.select(s(&b, "learner_id"), true).await?;
        self.check(
            "其他档案保持隔离",
            other["turns"].as_array().unwrap().is_empty()
                && other["recalled_memories"].as_array().unwrap().is_empty(),
            "独立模拟档案B",
        );
        ensure!(
            self.checks.iter().all(|c| c["status"] == "PASS"),
            "产品流程验收存在失败项目"
        );
        Ok(())
    }
    async fn resume(&mut self) -> Result<()> {
        let ids: Value = serde_json::from_slice(&std::fs::read(
            self.root.join("runtime/e2e-rust-state.json"),
        )?)?;
        let a = s(&ids, "learner_a");
        let session = self.select(a, true).await?;
        self.check(
            "实际进程重启后历史仍可召回",
            !session["recalled_memories"].as_array().unwrap().is_empty(),
            "",
        );
        self.check(
            "实际重启后当前搭建仍需重新确认",
            session["state"]["modules"].is_null()
                && session["state"]["modules_source"] == "unknown",
            "",
        );
        self.turn(s(&session,"session_id"),"重新打开了。继续上次的小车停车距离活动，上次进展和未完成实验是什么？先确认今天的模块。","进程重启后的继续").await?;
        let b = self.select(s(&ids, "learner_b"), true).await?;
        self.check(
            "实际重启后 B 仍不串记忆",
            b["turns"].as_array().unwrap().is_empty()
                && b["recalled_memories"].as_array().unwrap().is_empty(),
            "",
        );
        self.select(a, true).await?;
        self.report(" — 实际进程重启")?;
        Ok(())
    }
}
fn root(args: &[String]) -> Result<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--root") {
        return Ok(std::fs::canonicalize(
            args.get(i + 1).ok_or_else(|| anyhow!("Missing root"))?,
        )?);
    }
    for base in [
        std::env::current_dir()?,
        std::env::current_exe()?.parent().unwrap().to_owned(),
    ] {
        for ancestor in base.ancestors() {
            if ancestor.join("implementation-package").is_dir() && ancestor.join("models").is_dir()
            {
                return Ok(ancestor.to_owned());
            }
        }
    }
    bail!("无法找到 AI Tutor 工作区")
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mut acceptance = Acceptance::new(root(&args)?)?;
    let workflow = args.iter().any(|a| a == "--workflow");
    let restore_learner = if workflow {
        s(&acceptance.state().await?["session"], "learner_id").to_owned()
    } else {
        String::new()
    };
    let mut result = if workflow {
        acceptance.workflow().await
    } else if args.iter().any(|a| a == "--resume") {
        acceptance.resume().await
    } else {
        acceptance.run().await
    };
    if workflow && !restore_learner.is_empty() {
        if let Err(error) = acceptance.select(&restore_learner, true).await {
            if result.is_ok() {
                result = Err(error.context("验收结束后恢复原档案失败"));
            }
        }
    }
    if workflow {
        let path = acceptance.root.join(format!(
            "runtime/product-workflow-live-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "status":if result.is_ok(){"PASS"}else{"FAIL"},
                "verification_layer":"real Rust service + real Kimi; all learner data synthetic and simulator-only",
                "created_at":chrono::Utc::now().to_rfc3339(),
                "checks":acceptance.checks,"turns":acceptance.turns,
                "hardware_tested":false,"voice_tested":false
            }))?,
        )?;
    } else if result.is_err() {
        let _ = acceptance.report(" — 中断记录");
    }
    result
}
