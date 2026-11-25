# 引用失败的有界诊断

本片仅增加失败定位，不更改可引用集合、来源归属、输出 schema、提示词、引用别名或模型回复。原有两个真实失败的违规 ID 与回复未保存，无法事后恢复，也不能将本片的合成测试视为已复现其原因。

## 校验和输出边界

`tutor/citation.rs` 仍按原有字段顺序及数组上限检查本请求 `context.evidence[].event_id`。第一次失败的用户提示保持不变，原来接受的合法非 UUID 来源和整轮重复引用也保持既有语义。非数组、超数量仍走原 schema 错误；不会为了统计扫描超限引用。

引用失败可导出以下固定结构：`code`、`field`、`candidate_index`、`segment_sequence`、`reference_count`、`invalid_reference_count`、`allowed_event_count` 及最多 20 条 `invalid_references`。每条只含索引、严格的 ID 形状枚举和是否等于本请求的非事件结构身份；不含原始 ID、回复、思考、任意错误 context、匹配到的文本或路径。`count_scope=failing_field_only` 表示只统计失败字段，不宣称已经检查了后续候选。任务/记忆候选的单字段仍最多 8 个引用。

ID 形状仅识别固定前缀加 32 个小写 ASCII 十六进制字符。它只是诊断分类，不是新的接受条件。`matches_request_non_event_identity` 只比对 request/session/activity/memory 等结构身份字段；与正文恰好相同不算身份匹配。来源是否支持回答仍需独立语义审阅。

## Provider 可见性

整轮 `generate` 在实际 `invoke` 返回（其 OwnedRun 生命周期已结束）后更新 `status().metrics.last_citation_validation`：请求 ID、请求实际提供且类型合法的 `generation/session_generation`、`outcome` 和 `citation_diagnostic`。缺失代次为 null，不生成替代代次；成功及其他失败明确清除旧诊断。该字段表示最后完成的调用，消费者须核对绑定，不能视为最大代次水位。

ACP 仍使用 `last_acp_segments` 的真实 request/generation 绑定，新增 `citation_diagnostic`。`segment_evidence` 和 `final_schema_invalid` 原机器码、固定提示、失败/清理阶段均不变。ContractError 仅复制有界 CitationFailure 原因，不保留任意原始错误。清理失败不覆盖操作时的引用诊断；早失败或成功没有上次残留。片段已通过校验不等于已显示或播放。

未来独立合成探针可在失败后读取这些状态，但本片未修改任何已冻结 probe、请求、结果或原始日志；没有保存新的模型原始响应。

## 验证范围

新增 9 项无模型测试：引用模块 5 项，整轮 provider/解析 2 项，ACP 2 项；另收紧既有 ACP 早失败测试的清旧值断言。覆盖三个回复入口、错误 memory/session ID、未知合法形状 event ID、恶意字符串/对象、包裹错误、严格形状、原接受与数量边界、合法无诊断、缺失/错误代次、成功和早失败清旧值、实际 ACP workflow 的 segment 与 clean-EOF final，以及既有 scripted transport 下的生产 gate/decoder/provider wrapper。

整轮成功/引用失败使用生产 `validated_stream_reply` 处理合成外层 stream-json；整轮早失败走真实 `generate`。ACP workflow 使用内存 RPC 与真实 profile 证据读取，scripted provider 不启动进程。它们不证明远程模型、真实进程清理故障或原先两次失败的原因。

本代理仅运行 rustfmt，未运行 Cargo、模型或设备。实际测试结果由根代理统一执行后补充。

根代理已执行 checkpoint20 统一回归：`workspace-forty-fifth.log` 为 825 通过、0 失败、9 忽略，包含上述 9 项；原生全部 bin 的 release 构建通过。编译前冻结的 295 个输入文件在编译后逐项复核未变，实际可执行文件记录于 `checkpoint20-measured-binaries/manifest.json`。这些测试不恢复此前两个真实错误中未保留的违规引用。

修改前 5 个现有 Rust 文件与 checkpoint19-core-measured-sources 哈希完全一致。原件及逐文件 SHA 位于 `runtime/goal-backups/20261104/architecture_audit/citation-diagnostics/before-hashes.json`。
