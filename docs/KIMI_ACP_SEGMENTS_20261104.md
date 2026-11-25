# 可选 ACP 教学片段接口（2026-11-04）

本轮增加可选接口和本地协议测试，不切换教师界面的默认调用。`KimiProvider::generate()` 和 `consolidate()` 仍使用原有 stream-json、整份 TutorReply 验证后返回。没有新增服务器路由、数据库写入、UI、TTS 调用或远程模型实测。

## 接口与调度

`KimiProvider::generate_segments(request, generation, sender) -> Result<ValidatedTutorReply>` 定义在 `rust/ai-tutor/src/kimi_acp_stream.rs`，复用同一个 provider 的 `register_call(..., false)` 和 gate。它与普通前台教学请求一样先取消后台记忆工作，再排队取得唯一模型执行许可；许可直到 ACP 自有子进程和目录清理之后才释放。不会新建第二个 provider 绕开调度。

调用方提供大于 0 的本地交付 generation 和容量 1–2 的 Tokio bounded sender；推荐用公开常量 `SEGMENT_CHANNEL_CAPACITY = 2`。调用方必须并发消费接收端，不能先等待返回值再读队列。通道关闭、provider 的现有 `cancel` / `cancel_owner`、90 秒期限都会中断工作，包括队列满时的 send 等待。不要通过直接 abort 执行任务替代 provider 取消接口：受控取消才能等待进程回收并报告清理结果。

最终成功值只通过读取方法暴露原 TutorReply；构造字段不对外开放。调用方只有在成功返回、会话与 generation 仍有效时，才可走现有整份回复的服务器验证与候选应用事务。本轮尚未接入该调用方。

## 模型输出契约

输出为 UTF-8 JSONL，segment 必须以换行符结束。final 推荐换行；仅在 ACP 明确 end_turn 且末次实际 profile 审计通过之后，也允许 EOF 结束一个完整的 final。第一类记录为 `segment`：

```json
{"schema_version":"1.0","type":"segment","request_id":"本次请求","sequence":0,"text":"先观察测距模块显示的数值。","used_evidence_ids":[]}
```

最多 12 段，编号从 0 连续递增；每段 1–320 个 Unicode 字符，全文最多 1600 字符。每段都必须是非空白文字，来源列表至多 20 项且无重复，全部属于当前请求的 `context.evidence[].event_id`。模型不能填写本地 generation。对外的 `TutorSegment` 由解析器附上 `{request_id, generation}`，不含思考文字或未完成 JSON。

最后一条记录为 `final`：

```json
{"schema_version":"1.0","type":"final","request_id":"本次请求","segment_count":1,"reply":{"schema_version":"1.0","request_id":"本次请求","reply_text":"先观察测距模块显示的数值。","teaching_move":"ask_observation","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}}
```

`reply` 继续使用现有完整 TutorReply 校验器；`reply_text` 必须逐字等于全部片段的拼接，包括空白和标点，不做 trim 或重新排版。final 的来源必须覆盖所有片段使用的来源。缺失 final、重复 final、final 后额外有效文本、记录乱序、未知字段、未结束的 segment 或截断 JSON 均失败。任一错误会使解析器永久失效，不能继续补出一份成功回复。普通 `finish()` 仍要求换行；只有显式 `finish_clean_end()` 可以将 EOF 当 final 分隔符，且复用同一个记录校验函数，不释放末尾 segment。

“逐句”是模型的输出组织要求，程序可以验证记录、字符数、来源和最终一致性，不能证明一句自然语言在教学内容上必然正确。分段校验不会取代原有教学约束。任务与记忆候选只出现在 final；不得从较早片段推断或应用候选。

文本解析器保存跨 chunk 的 UTF-8 字节，完整换行出现前没有片段交付。单行最多 32 KiB，全文 JSONL 最多 64 KiB；底层 ACP 另保留单行 64 KiB、stdout 总量 256 KiB、stderr 128 KiB、有效 profile 证据 512 KiB 的硬上限。上限触发会停止，不截断后假装成功。

## 有效配置与进程边界

`kimi_acp_common.rs` 从已核验诊断中抽出隔离 home、workspace agent override、最小环境、bounded RPC reader、模型元数据、thinking 回执、实际 `wire.jsonl` profile 审计。原 `kimi_acp_probe` 改为调用同一组基础函数，没有复制整套诊断程序到生产适配器。

每次新 ACP 会话均要求官方 2.3.1、protocolVersion 1，clientCapabilities 与 MCP server 列表为空，实际配置中的模型与 session/new 回执一致。发模型请求前必须证明当前隔离会话的 `profile.bind` 是教学策略、工具列表为空、subagents 为空。`tools.reset_active_tools`、非空工具列表、策略/agent 切换、历史分支回退会失败。

可选适配器明确设置 `thinking=low`，要求 session/new 支持 low、set_config_option 回执 currentValue=low、有效证据出现新的 config.update 且最终值为 low；不把配置文件写入当生效证据，也不声称关闭了 always-thinking 模型的思考。每段入队前和 prompt 完成后都会重新核验当前 profile/low 的证据。agent_thought_chunk 只计数并丢弃正文，工具/计划事件直接失败；反向权限请求回复 cancelled，文件或终端请求回复能力禁用并终止。

沿用 Windows Job Object 与 OwnedRun。失败先发送有界 cancel 通知，再关闭 stdin、终止自有进程树、等待回收、完成 stderr drain、移除自有目录和 credentials junction。没有远端 cancel ack 时不声称收到。清理失败不返回成功 TutorReply。

## 实际交付记录

消费者持有 `DeliveryLedger`，所有 receipt 都绑定 request_id、generation、segment sequence。`accept(TutorSegment)` 只表示已校验，不能视为显示或播报。显示回执必须按序且不能重复；播报开始要求已显示且前一段的播报已经结束；完成/中断必须先有开始，重复或乱序完成均拒绝。旧请求、旧 generation、未接纳片段的回执不能改变记录。

`finish_validated(&ValidatedTutorReply)` 只接受该绑定、相同段数的最终验证结果，它不自动标记显示或播完。`invalidate()` 保留已经确认显示、播完的前缀，把正在播报的段记为中断，后续段仍保持未播；旧回执一律拒绝。该中断是本次交付会话失效，不是扬声器物理停止的测量，更不证明学习者听清或理解。消费者还必须实际停止 TTS，并继续使用既有播放所有权规则。

中途失败时，一些片段可能已经显示或播出。这些事实不可撤销，不能改写成“本次没有交付”；已播内容不应因重试而自动再播。未完成 final 的候选没有提交资格。后续 UI/Store 接线需要持久化实际交付状态及失败/取消事实，并避免把仅入队内容记成完整 assistant 消息；本轮没有提前改写 Store。

## 观测与验证边界

`status().metrics.last_acp_segments` 仅保存耗时、块数、入队段数和清理结果，不保存正文或 thought。first_delta / first_validated_segment / first_segment_enqueued / final_validated 以 prompt 写入前为起点；total_duration 包含握手、配置和清理。入队时间不是浏览器显示、扬声器首声或端到端对话延迟。

新增 14 项不访问模型的测试：8 项 JSONL/ledger 测试覆盖逐字节中文 emoji、未结束行、来源与请求绑定、重复未知字段、final 精确拼接与完整 schema、结束后的输出、各项上限、旧/乱序/重复回执和失败前缀；6 项假 RPC/发送测试覆盖 thought 丢弃、旧 session、工具与未知事件、响应 ID、反向权限拒绝、输入上限、容量 2 的取消和提示格式。原 ACP 诊断的有效配置与有界读测试继续保留。

根代理首次真实中文探针 `runtime/goal-baselines/20261104/kimi-segments-real-zh-first.log` 保持失败记录：296 个 message chunk，丢弃 43 个 thought chunk，2 个已校验但未显示/播报的 segment，首段验证 6146.43 ms（含握手的 consumer 接收为 6741.35 ms），最终未通过，清理通过。确切触发分支是 end_turn 后尾部存在未换行的非空白数据；旧日志未保存尾部，不能判断它是否完整 final。不会将这次记录回写为成功。

据此增加干净 end_turn 的最终 EOF 边界处理，以及 4 项本地回归：完整无末尾换行 final 正例；截断/尾部 segment/缺 final/额外对象/无效 UTF-8 反例；与换行路径相同的正文、来源、schema、段数验证；诊断字段不包含正文。安全失败码包括 `incomplete_json`、`invalid_json`、`invalid_utf8`、`record_schema_invalid`、`tail_not_final`、`missing_final`、`final_text_mismatch`、`final_evidence_mismatch` 等。`contract_diagnostics` 只保存字节数、段数、末尾形状和白名单记录类型，`final_delimiter` 在成功时区分 `newline` 与 `clean_end_turn_eof`。这些修复尚待根代理统一测试和复测。

根代理已运行 `lib-acp-first.log`：226 项通过、0 失败、2 ignored，包含上述新增契约/假 RPC 测试。代理侧只执行 rustfmt，没有运行 Cargo。共享抽取后的静态 probe 与可选真实 JSONL 输出是否稳定遵守格式，由根代理统一验证并追加结果。此前 ACP 诊断证明官方协议能给出多个 message chunk，不能代替本次 JSONL 模型输出和端到端分段交付实测。本接口目前不支持宣称首句延迟目标已通过。

## 显式合成探针

新增自动发现的 bin `kimi-segments-probe`，只在同时给出 `--real-synthetic` 和 `--language zh|en` 时继续，仍要求 `probe().available=true`。它使用刚实现的同一 provider 可选方法、容量 2 通道、临时请求编号、固定中/英文教育问题、空 evidence；没有读取学习者 DB、显示、播报或设备操作。所有接收的 segment 只进入 ledger 的 validated 状态，不制造 shown/played 回执，不输出文字或完整回复。

根代理统一构建后可在项目根目录执行（每条真实模式为一次远程请求）：

```powershell
target/release/kimi-segments-probe.exe --help
target/release/kimi-segments-probe.exe --real-synthetic --language zh
target/release/kimi-segments-probe.exe --real-synthetic --language en
target/release/kimi-segments-probe.exe --real-synthetic --language zh --cancel-after-ms 300
```

报告分别保留 provider 的 prompt 起点首段/最终验证耗时，以及包含排队和握手的 consumer 首段接收耗时。取消计时从 `generate_segments` 调用前开始，`cancel_to_return_ms` 包括 provider 受控取消与清理，不能当作纯操作系统 kill 耗时。cleanup_verified 要求进程回收、目录删除、stderr drain 和 provider running=0 同时成立。预期取消仍输出 success=false / exit 1，另记录 expected_aborted 与清理证据，且明确没有远端 cancel ack。取消后的未消费队列项只统计丢弃，不伪记显示或播报。新增 bin 的 1 项本地参数测试证明没有两项显式参数时不会进入远程调用函数；该 bin 尚待根代理编译与执行。

源码依据与既有实测见 `KIMI_ACP_PROBE_20261104.md` 和 `KIMI_INCREMENTAL_BASELINE_20261103.md`；固定官方代码是 [kimi-cli v2.3.1](https://github.com/MoonshotAI/kimi-cli/tree/2.3.1)，已核验 commit `684bae9`。本轮没有再次请求远程模型。
