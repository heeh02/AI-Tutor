# Kimi 教学首段：移除重复配置往返

2026-11-04，第十四检查点。此子任务仅修改 `kimi_acp_stream.rs` 和其合同测试；当前版本、模型、low 档位、无工具 agent、教学 policy、输出 schema、request/context、片段与完整回复校验均保持原样。独立的context改动见同检查点检索文档，本节固定probe不经过该上下文构建路径。编译与真实对照已由根侧统一执行，没有运行音频或视觉设备。

## 已有证据

来源是 `voice-model-chain-first/report.json` 指向的中文、英文各一轮真实模型链。计时原点不同，不能直接把所有差值当作网络或模型时间：

| 指标 | 中文 | 英文 |
| --- | ---: | ---: |
| consumer 调用到首段观察 | 13,470.44ms | 11,542.42ms |
| prompt 发送前到首正文事件 | 10,637.44ms | 10,977.68ms |
| prompt 发送前到首段验证 | 12,890.26ms | 11,017.30ms |
| consumer 与 provider 首段计时之差 | 580.18ms | 525.12ms |
| 已丢弃的 thought chunk 总数 | 161 | 247 |

最后一项是全轮事件数，不能据它推算思考 token 数或单独耗时。首正文前已有约 10.6–11 秒等待；代码没有在收到可用片段后固定睡眠，容量 2 的投递通道允许取消。中文首正文到闭合首段另有约 2.25 秒，英文约 40ms；只有闭合且通过协议、来源、profile 校验的句子才能交付，不能用未校验前缀或占位句掩盖这部分等待。consumer/provider 差值包括 gate 等待、本地隔离准备、进程/握手以及消费者调度，不是单项测量。

系统 agent 是短的角色与无工具边界，完整教学 policy 只在用户请求中出现一次。未发现可直接删除的重复完整教学策略。请求精简、移除路由字段或 schema 注释本片均不实施，以保留真实前后对照的同一 prompt。

## 实际改动

`safe_config()` 已请求 `thinking.effort=low`。旧 ACP 流程无论 `session/new` 的真实选项和有效配置日志是否已确认为 low，都会再发送一次 `session/set_config_option(low)`，随后等待同值的新日志记录。

现流程先完整验证初始 profile：实际 agent、空工具/委派、精确 system prompt、当前 thinking 必须和返回选项一致。如果两份证据均为 low，则省去同值配置 RPC 及它后面重复的证明；若实际档位不同，仍发送配置请求，要求回执为 low，并核对日志中的新增 config.update 后才继续。每个片段和 final 仍重新验证当前持久日志，不能用启动时的证明替代后续检查。

这是移除确定冗余操作，不能声称解决正文前约 11 秒的模型等待。官方固定版本的 `setSessionConfigOption` 会调用 thinking setter 并重新返回当前配置选项，因此现有低档位的确认和“再次设置同一值”是可区分的操作。[Kimi 2.3.1 对应提交的 ACP 实现](https://github.com/MoonshotAI/kimi-code/blob/684bae9ea6a437f3bae98bba1dcb797a917d500b/packages/acp-server/src/server.ts)

`stream_prompt` 函数与备份逐字符比较一致，函数源码 SHA-256 为 `05804bd66413e4ca10eaf598533f1824182fda7f2fe8c76f9eb66a082a442273`（读取 UTF-8 后统一行尾再摘要）。政策、schema 和生成请求本片没有改动。

## 新数字指标

| 字段 | 原点与含义 |
| --- | --- |
| `initial_thinking` | `session/new` 返回且已由有效日志核验的档位 |
| `thinking_config_rpc_count` | 本轮发起配置 RPC 数；已经确认为 low 时为 0，否则为 1 |
| `profile_verifications` / `profile_verification_ms` | 实际成功的持久证据核验次数及其累计时长，包含各段和 final；不是模型推理时间 |
| `queue_wait_ms` | 注册请求后，仅等待现有 provider gate 的时长 |
| `preparation_ms` | 取得 gate 后到启动进程前的隔离目录、配置和同一 prompt 准备 |
| `process_spawn_ms` | 进程启动、Job 归属、管道与 stderr reader 准备 |
| `session_setup_ms` | workflow 开始到 prompt 发送前：initialize、session/new、档位确认与必要配置、初始 decoder |
| `prompt_chars` / `prompt_utf8_bytes` | 实际送出的同一 prompt 字符/UTF-8 字节数，不估算 token |
| `first_thought_event_ms` | prompt 发送前起点到首次观察到 thought 事件，仅时间；内容仍直接丢弃 |

原 `first_delta_ms`、`first_validated_segment_ms`、`first_segment_enqueued_ms`、`final_validated_ms` 原点保持在 prompt 写入之前；`total_duration_ms` 继续包含 workflow 与清理。失败前未观测到的事件不填造数值。旧 release 没有新增分阶段字段，不反推或回填旧报告。

## 合同与验证

新增四项使用临时目录证据与内存 duplex RPC 的回归，不启动进程、不调用模型、不改生产时钟：

- `already_low_reuses_verified_profile_without_sending_configuration_rpc`：真实低档位和持久证明一致时，没有任何配置字节写出。
- `low_option_claim_still_requires_matching_durable_no_tools_profile`：回执自称 low 而实际日志为 high，或工具列表非空时拒绝，不发送配置或假记有效档位。
- `non_low_session_changes_effort_and_verifies_the_new_durable_record`：非 low 时实际读取发出的 RPC，写入新的正确日志并回应，才能继续。
- `low_configuration_receipt_without_durable_change_is_rejected`：回执为 low 但日志没有相应更新时拒绝。

根侧可用 `cargo test -p ai-tutor --lib kimi::acp_stream::tests -- --nocapture` 统一验证。旧版与新版都使用已有 `kimi-segments-probe --real-synthetic --language zh|en` 的固定输入，各语种各一次、不重试；模型执行由根侧独占安排。可确定归因的是少一次同值 RPC/重复证明；单次时延差仍混合网络、模型采样、进程和 OS 波动，不能据此给出 p95 或宣称模型等待改善。

## 本次真实结果

`workspace-thirty-fourth.log` 全部731项通过、0失败、6忽略（含UI122），新增四项合同均通过。原生release构建132.85秒成功。固定probe源码SHA在前后method中一致；生成请求ID每轮不同，不能称完整请求逐字相同。先旧中英、后新中英，各一次、无重试；输入是解释测距模块并提出未进行实验，和第十三片的“停下小车”输入不同，不跨输入推断加速幅度。

| 指标 | 中文旧→新 | 英文旧→新 |
| --- | ---: | ---: |
| consumer到首个有效段 | 8556.57→6612.65ms | 7990.58→7047.61ms |
| prompt发送前到首段验证 | 8034.12→6105.35ms | 7482.28→6543.79ms |
| 两个时钟原点的首段差值 | 522.46→507.30ms | 508.30→503.82ms |
| prompt发送前到完整回复校验 | 11271.07→6841.52ms | 9951.88→9819.12ms |

两次新版实际都报告 `initial_thinking=low`、`thinking_config_rpc_count=0`，各完成2段及final、4次profile核验。准备/进程启动分别约2.59/2.81ms与3.02/3.65ms；会话设置501.84/497.12ms，累计profile核验7.23/9.30ms。首thought事件1927.33/1445.51ms、首正文4811.90/6511.64ms；这些是观测事件时间，不能拆成精确模型思考/网络/排队耗时。

因此确定的收益是省去冗余RPC；本地差值仅减少约15.15/4.47ms，且仍含调度波动。总首段减少1.94/0.94秒主要发生在prompt提交后的时间段，不能归因于这项代码优化。新首段依然约6.6/7.0秒，高于目标，主瓶颈尚未解决。本片不修改指标、插入占位句或省去片段校验来掩盖等待。

四轮均正常完成、子进程回收、隔离目录删除、stderr排空；没有显示/播放/教学投递或打开用户数据库。完整度/格式校验通过不等于教育质量人工验收。证据在 `runtime/goal-baselines/20261104/kimi-lowconfig-before/` 与 `kimi-lowconfig-after/`；前者保留旧probe可执行文件，方法和原始结果不覆写。
