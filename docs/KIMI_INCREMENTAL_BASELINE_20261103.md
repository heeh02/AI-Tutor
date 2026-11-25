# Kimi 2.3.1 增量输出与时序基线

日期：2026-11-03；实测补充至2026-11-04。此轮增加诊断和协议研究；默认教学输出继续等待完整 JSON 与 TutorReply 校验，不提前显示、播报或写入记忆。

## 精确版本证据

本机只读执行 `kimi --version`、`kimi --help` 和 `kimi acp --help`：版本为 **2.3.1**，支持 `--output-format stream-json` 与 `acp` 子命令。本次没有发送模型请求、启动 ACP 会话、读取凭证明文或操作设备。

官方发行标签是 `@moonshot-ai/kimi-code@2.3.1`，对应提交 `684bae9`。以下实现结论基于该提交，避免把滚动更新的官网文档直接视为本机行为。[官方发行列表](https://github.com/MoonshotAI/kimi-code/releases)

| 接口 | 已核对的官方实现 | 本机尚待验证 |
|---|---|---|
| `-p --output-format stream-json` | `PromptJsonWriter.writeAssistantDelta` 累加字符串；`flushAssistant`／`finish` 才写完整 assistant JSONL。无工具正常单轮通常不是 token 级输出。 | 首 meta、首完整 assistant、EOF 的实际时间差和事件数 |
| `kimi acp` | `assistantDeltaToSessionUpdate` 将 `assistant.delta` 映射为 `agent_message_chunk`，具备真正增量事件通路。 | 2.3.1 Windows 实际分块时序、取消表现与独立无工具策略验证 |

对应源码：[JSON writer 第155–156、227–244行](https://github.com/MoonshotAI/kimi-code/blob/684bae9/apps/kimi-code/src/cli/prompt-render.ts#L155)、[ACP delta 映射第33–42行](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/acp-server/src/events-map.ts#L33)。官网同时说明 [JSONL消息语义](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command) 和 [ACP session/prompt](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-acp.html)。

**首 stdout 字节不等于首教学文字。** 版本元数据可以早于模型输出；一条 assistant JSONL 即使跨多个管道 read，也不能称为多个模型 delta。不能凭 `stream-json` 名称承诺更早开始播报。

## 已实现的指标

`kimi.rs` 仍并发读取 stdout/stderr 并等待子进程，保留 stdout 256KiB、stderr 128KiB 上限、90秒 deadline、取消、Windows Job Object、隔离 home 和完整 schema 校验。新增观察器只记录数字，不保留第二份输出或对外投递内容；逐字节只扫描一次，UTF-8 只在完整行或 EOF 后解析。

`status.metrics.last_stream_timing` 及核验/诊断 detail 的 `stream_timing` 包含：

| 字段 | 含义 |
|---|---|
| `first_byte_ms` | 第一次成功读到 stdout 字节 |
| `first_line_ms` | 第一条完整非空 JSONL 行；末行无换行时在 EOF 才记录 |
| `first_assistant_ms` / `last_assistant_ms` | 首／末条可解析、role 为 assistant 的完整 JSONL 记录；不代表 schema 通过或可播报 |
| `stdout_eof_ms` | stdout 关闭 |
| `io_complete_ms` | 管道读取及进程等待完成，或该阶段因失败、取消、超时退出 |
| `validated_reply_ms` | 完整外层协议、教学 JSON 与 TutorReply 校验通过 |
| `completed_ms` | 成功回复并清理本轮隔离目录完成 |
| `stdout_read_chunks` / `stdout_bytes` | 操作系统管道读次数／已接收字节数，不是 token 数 |
| `complete_nonempty_lines` / `assistant_messages` / `malformed_lines` | JSONL 记录数量与诊断计数 |

时钟为 Rust `Instant`，原点为通过队列及配置审查后、创建本轮隔离目录之前；不包含排队或前置配置审查耗时。既有 `last_duration_ms` 仍保持原来截至 IO 结束的含义。失败时没有出现的事件保持 null，不伪造为 0；未通过完整校验的观测不会标记为 `validated_reply_ms`。

新增四项纯内存测试：单字节 UTF-8 分片和元数据早到、精确输出边界/超限/空流、早到 assistant 后迟到工具事件仍整体拒绝、无换行 EOF 和非法行。原 Windows 真实匿名管道/进程树合同测试继续复用同一底层有界读取器。

## 可直接实测的路径

新增自动发现二进制 `kimi-timing-probe`，不修改 Cargo 配置。构建和远程调用由根代理统一执行：

```text
kimi-timing-probe --real-synthetic --root "C:\Users\RBMS\Desktop\AI TUTOR" --language zh
kimi-timing-probe --real-synthetic --root "C:\Users\RBMS\Desktop\AI TUTOR" --language en
```

每次仅发一轮固定合成教学请求，沿用已核验的生产 provider，输出版本/二进制摘要/agent摘要/耗时/字节和字符数，不输出教学原文，不写学习者数据库，不声明已展示。没有 `--real-synthetic` 时直接退出且不调用模型。已有无工具核验无效时拒绝请求，不自动重跑付费核验。

先比较 `first_line_ms`、`first_assistant_ms`、`stdout_eof_ms` 与 `assistant_messages`，再判断当前 JSONL 是否仍整条输出。至少分别记录冷启动与重复独立进程的样本，保留模型/网络波动；少量样本只能作为基线，不能宣称 p95 达标。即使首 assistant 早于 EOF，默认交付仍以完整校验为准。

## ACP 下一阶段可验证路线与隔离差异

2.3.1 的 [ACP入口第60–68行](https://github.com/MoonshotAI/kimi-code/blob/684bae9/apps/kimi-code/src/cli/sub/acp.ts#L60) 只向 server 传入 home、版本和登录入口信息，未转发 `--agent-file`／`--skills-dir`；[session/new 第242–252行](https://github.com/MoonshotAI/kimi-code/blob/684bae9/packages/acp-server/src/server.ts#L242) 创建会话时传入 cwd、附加目录和 MCP 信息，不接收教学 agent 参数。因此把现有命令尾部换成 `acp`，即使解析器接受全局参数，也不能视作现有空工具策略已经生效。

可验证候选是每次独立 home 和空 work，在自有 work 的 `.kimi-code/agents/agent.md` 放置经校验的默认主 agent 覆盖文件，明确 `name: agent`、`override: true`、`tools: []`、`subagents: []`，正文使用当前教学策略且不引用继承模板。官方2.3.1 [agent发现及覆盖规则](https://github.com/MoonshotAI/kimi-code/blob/684bae9/docs/en/customization/agents.md#L56) 支持此机制，但本机 ACP 尚未验证，不能直接上线。`SYSTEM.md` 只替换提示词，不替换工具列表，不能用于替代空工具策略。全局 `tools.enabled=[]` 同样不禁用工具。[2.3.1全局工具语义](https://github.com/MoonshotAI/kimi-code/blob/684bae9/docs/en/configuration/config-files.md#L420)

验证顺序应为：先无模型 initialize/session 创建与有效 profile 检查，再独立合成 canary，确认工具和子代理均不可用后才发普通模型诊断。客户端不提供 fs、terminal、MCP 能力，对权限请求明确拒绝，并对工具事件直接终止；这些是补充防线，不能替代有效的空工具配置。所有新增目录、通知和 stderr 仍须有界、归属本轮并清理，仍由 Job Object 负责进程树取消。

通过上述验证后再测 `initialize → session/new → session/prompt`，从 `session/update.agent_message_chunk` 统计首 delta、次数和间隔，同时比较完整合法 TutorReply 时间。每轮独立会话，禁止 load/resume 历史复用；取消应同时发 `session/cancel` 并保留本机强制终止兜底。任何文本增量仍为未校验候选，不作为语音播放或学习记忆。需要早播时，须另行设计已闭合且获准字段的交付合同，不在本轮更改生产行为。

## 当前验证状态

根代理已完成workspace回归（309通过、0失败、1忽略，`runtime/goal-baselines/20261103/workspace-fourth.log`）及原生release构建。首次真实stream-json中文诊断见 `runtime/goal-baselines/20261103/kimi-stream-zh-first.log`：首字节/首行374.41ms，首完整assistant8853.47ms，EOF8884.76ms，完整校验8894.01ms，清理完成8896.96ms；3条非空协议记录中仅1条assistant，工具事件0、stderr0、无投递。它确认早到的stdout元数据不能算教学首字。

独立ACP静态配置验证、一次真实合成canary和一次取消诊断也已执行：真实轮98个文字chunk，首文字6941.18ms，prompt完成7074.16ms，空工具与完整策略检查均通过。取消轮保留预期中止状态而非完整成功。详见 `docs/KIMI_ACP_PROBE_20261104.md` 和对应原始日志。

两条真实请求的prompt与计时原点不同，只有各一条，不能把时间差解释为ACP带来的确定加速。真实ACP文字主要集中于最后约109ms，首文字前仍有约6.94秒等待，低延迟对话目标尚未达到。诊断验证不等于生产已迁移ACP，也不构成真实学习者或语音交互验证。
