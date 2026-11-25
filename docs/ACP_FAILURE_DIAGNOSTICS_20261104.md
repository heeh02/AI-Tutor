# ACP 流式失败诊断：后续切片

本次改动位于 checkpoint18 已测源码归档之后。不能把新增字段或测试倒算进该 checkpoint 的 786 个通过测试，也不能用新字段补推旧实测的根因。

## 触发证据与原有信息缺口

`voice-loop-soak-e2dacb8fe8e346fdac6007432ff94dfc-zh/round-013.json` 的第 13 轮 ASR 已有终稿，教学流随后失败，没有教学段或播放回执。该次 provider metrics 的 `session_setup_ms=578.6461`、`total_duration_ms=764.4592`、`message_chunks=0`、`failure_code=null`。原实测保留：12 轮完整，未达到 1800 秒目标。

旧实现的 `failure_code` 只识别 `tutor_segments::ContractError`。ACP EOF、JSON-RPC 错误、读写错误、取消、超时、清理失败并不属于这个类型。Wire 还把“响应 ID 不匹配”和“远端 error 响应”合成同一条安全错误，未保留 RPC 数值代码。stderr 有界读取后仅取长度，而长度未写入 metrics；临时 owned 目录正常清理。主程序会把安全错误文本暂存到最多 8 条 `state.errors`，但本次 soak 保存的投影只有通用失败状态与 provider metrics，无法恢复具体原因。

`session_setup_ms` 在发送 prompt 之前赋值。因此旧数据只能说明会话准备结束、没有观察到内容，不能证明 prompt 已成功写完、远端已接收，也不能区分传输中断、远端 RPC 拒绝或其他协议问题。此次未读取无关私有 Kimi 日志，未重跑失败请求。

## 新字段与边界

`metrics.last_acp_segments` 仍只保留最近完成的一次调用；新增 `request_id` 和 `generation`，消费者须匹配实际请求，不能把“最近一次”当作当前请求。准备、排队和进程启动前的错误也会生成本次绑定的记录，避免失败后仍呈现旧成功。此处不新增历史日志队列。

| 字段 | 含义 |
| --- | --- |
| `phase` | 最后执行阶段：prepare、queue、spawn、initialize、session_new、profile、prompt_write、prompt_response、segment_validation、final_validation、session_close、cleanup。是固定枚举，不来自模型文本。 |
| `failure_phase` | 首要失败的阶段，清理过程不会覆盖原操作失败的阶段。 |
| `prompt_write_attempted` | 已进入本地 `session/prompt` 写入步骤。 |
| `prompt_write_completed` | 本地管道 `write_all` 完成；不是远端接收、接受或开始推理的回执。 |
| `operation_success` / `operation_failure_code` | 会话流程结果。保留原教学 JSONL 契约代码；其他错误使用 ACP 白名单代码，未知错误为 `acp_operation_failed`。 |
| `remote_rpc_code` | 仅匹配请求 ID 的远端 error 响应中的整数代码；没有有效整数则 null。不保留 message/data。 |
| `cleanup_failure_code` | 独立清理失败码，不覆盖操作失败；可与操作失败同时存在。 |
| `failure_code` | 兼容字段：优先操作失败码，否则清理失败码；实际失败不再因不是 ContractError 而空缺。 |
| `stderr_bytes` | 有界 stderr reader 成功返回时的字节数，不保留正文；失败或未完成则 null。 |
| `process_reaped` / `owned_directory_removed` / `stderr_drained` | 明确执行并观察到的清理结果。早期退出尚未获得这些证明时保持 null，不把 Drop 或未启动推断为清理成功。 |

白名单区分读、写、写入超时、EOF、JSON 编码、容量上限、协议封装、响应 ID、远端 RPC、禁用能力、profile 核验、准备、启动、进程归属、取消、总截止、接收方关闭、stderr 和清理。分类来自类型及明确失败分支，不匹配中文错误字符串。任意错误链上的外层 context、远端 message/data、提示、路径及 thought 都不序列化进新增诊断字段。

`state.errors` 可在聊天的“查看服务详情（原文）”中显示，因此类型的 Display 使用固定、简短的服务状态提示，不把内部 `acp_*` 代码作为课堂错误文案。数值代码仍只通过独立诊断字段输出，测试按类型/代码验证。

生产流程保持原容量 2 的分句通道、90 秒截止、取消顺序、进程回收和目录清理策略。仅将既有等待分支提取为模块内函数，使本地测试可以执行同一取消/截止分支；没有新增重试、权限能力、生产 API 或通用任务框架。原 Store 错误状态仍使用其既有通用失败映射，未在本片修改。

## 验证状态

已新增 7 个本地测试函数，覆盖真实 Wire 的 EOF/JSON/错误 ID/RPC 数值投影、嵌套 context 不泄漏、实际完整 workflow 的响应前失败/契约失败/成功、第三次写入失败不误称 prompt 写完、生产等待分支的取消/截止/stderr 错误、操作与清理同时失败、早期拒绝覆盖旧请求 metrics。fixture 使用临时目录、本地内存管道和生产 decoder/profile/workflow，不启动远端模型或设备。

其中清理失败测试向生产投影函数注入已观测布尔结果，只验证主错误与清理错误共存及优先级，不证明操作系统 Child/Job/目录真实清理故障已实测。`last_acp_segments` 按完成顺序写入：较旧调用晚收尾可以覆盖较新调用的早拒记录，不能把它当作最大 generation；request/generation 绑定用于明确归属。

本代理已执行 `rustfmt`，未执行 Cargo、模型、设备或浏览器。根代理已完成统一 workspace44：807 passed、0 failed、9 ignored，UI 128，exit 0，无 warning；`workspace-forty-fourth.log` 中本模块 17 个测试（含新增 7 项）均通过，退出记录见 `workspace-forty-fourth-exit.json`。本代理只读核对了该日志和退出记录。新诊断字段尚无真实模型故障复测；原 soak 报告与原方法保持冻结，下一次实测须使用新的独立输出目录及新二进制哈希。

## 两次真实成功路径及独立字段审阅

根代理随后各执行一次中文、英文合成诊断，两者 exit 0。原始完整 JSON 为 `runtime/goal-baselines/20261104/acp-failure-metrics-{zh,en}-first.log`，对应 `-exit.json` 保存外层计时。使用归档的 `checkpoint19-core-measured-binaries/kimi-segments-probe.exe`，SHA256 `2f30e5cc2fd1b354a87a811186dd5ccf76dca026cf9e1770cc99d34d8dea2842`；归档 manifest SHA256 `42ef8ecc9a0cbcda98a2564cd1f013dfec3ec69ba95680f0b72b7af085ef6c42`。报告内的 `binary_sha256` 指 Kimi CLI，不是探针程序，二者不混用。

architecture_audit 独立读取原日志、退出记录与归档哈希，并生成 `runtime/goal-baselines/20261104/acp-failure-metrics-success-review-first.json`（SHA256 `d4343399ea79c4b2ddb939137cc1709b6ef111196af4af653c6bc0bf1a6975b3`）。该审阅没有调用模型或修改原报告，字段一致性检查均通过：

- 两次 request_id 各自与 ledger 完全一致且相互不同；generation 均为 1，与调用和 ledger 相同。每次恰有连续的 0、1 两段，全部 validated，final_validated=true，未 invalidated。
- `prompt_write_attempted/completed` 均为 true；`operation_success/success` 均为 true，所有操作、远端 RPC 与清理失败码均为 null；最终 phase=cleanup。进程回收、目录删除、stderr drain 均为 true，stderr_bytes=0。
- 每段 displayed=false、speech=not_started；顶层 delivered/displayed/played、设备与用户数据库访问均为 false。ledger 的 speech_gate=open 不是显示或播放回执。
- 中文 final 由换行结束；英文 final 通过既有 `clean_end_turn_eof` 契约完成。英文在 finish 前保留的 545 字节尾部是一个完整 final JSON，不能因为 `newline_final_validated=false` 就称最终回复未验证。
- 实际 thinking 均为经审计的 low，未再发送档位修改 RPC，profile 核验各 4 次；仍分别出现 140、93 个 thought 事件，正文丢弃，不能宣称关闭思考。

| 语言 | 首段消费者接收，含排队/握手 | final 验证，自 prompt 写入前起算 | 探针内部总耗时 | 外层进程耗时 |
| --- | ---: | ---: | ---: | ---: |
| 中文 | 11.907 s | 12.493 s | 13.031 s | 13.566 s |
| 英文 | 8.175 s | 10.118 s | 10.663 s | 11.122 s |

这些字段记录的是两个成功样本，计时原点不同；不是扬声器首音、稳定分布或时延保证。`prompt_write_completed` 仍仅证明本地管道写入。此轮没有实际取消、远端 error 响应、EOF 故障或清理故障，不能凭 null 错误码声称这些失败路径已真实复现，更不能由两次成功反推原 soak 第 13 轮的根因。正文未保存，因此也不构成教学语义、物理操作措辞或 30 分钟稳定性验收；相应原失败证据保持不变。
