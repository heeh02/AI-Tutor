# 可选教学文字流式投递（2026-11-04）

本轮接入前两片：持久化分段显示事实、文字逐段显示。没有接入分段 TTS，没有自动播报，也没有切换默认生产模式。原 `generate()`/整份回复的默认路径继续保留。实现由 `server::tutor_delivery`、`store::tutor_delivery` 和独立 UI 分句组件分担；ACP 仍调用已有 KimiProvider 和前台优先 gate。

## 显式配置

启动时只读取项目内 `runtime/tutor-delivery.json`，文件缺失默认 `whole_reply`。只有以下明确配置启用新请求的分段路径：

```json
{"protocol_version":1,"mode":"segments"}
```

配置最多 2048 字节，未知字段、版本或模式直接报错。支持显式 `whole_reply`。本轮未创建生产配置文件；无需增加启动命令或修改 main。重启时未完成的流式投递会终止，不恢复旧模型请求或自动继续播报。

## 同源 HTTP 合同

复用现有 HttpOnly cookie、loopback/same-origin 中间件和请求体限制。`/api/state` 增加：

```json
{"tutor_delivery":{"protocol_version":1,"mode":"segments","automatic_speech":false}}
```

当前 session 增加 `tutor_stream`，为当前 session generation 和 learner epoch 的最新流式 head；生成、最终等待显示、完成、失败的 head 都保留至 generation/epoch 改变。每条流式 assistant turn 在开始时已有固定 turn_id 和空正文，永久带 `delivery_mode:"segments"`、`stream_phase`，历史分页也保留这些字段。

`GET /api/sessions/{sid}/streams/{rid}` 返回完整、有界快照，不依赖事件广播或 after 游标，不漏段：

```json
{
  "protocol_version":1,"session_id":"...","request_id":"...","generation":2,
  "turn_id":"...","revision":4,"phase":"generating","final_validated":false,
  "shown_through":0,"error_code":null,
  "segments":[{"sequence":0,"text":"第一句。"},{"sequence":1,"text":"第二句。"}]
}
```

head 与快照字段一致，但不含 segments。sequence 从 0 连续，最多 12 段/每段 320 字/合计 1600 字。`shown_through:null` 表示没有显示回执。任何 segments、phase 或显示前缀变化都会推进 revision。同一 revision 不改变正文。UI 使用 150 ms、单在途 poll，按绑定与 revision 接纳完整快照，每次只把已确认前缀及下一段放入 DOM。

`POST /api/sessions/{sid}/streams/{rid}/display` 请求体为 `{generation,sequence}`，只能确认当前绑定的连续下一段；已提交的同一段可幂等重试。回执来自当前可见视图的 DOM 提交与绘制机会，收到网络文字本身不是显示回执。服务端返回最新完整快照，不播放音频。最终候选应用暂时失败时，同一个最后 sequence 的确认可重试最终事务，不会再次应用已提交候选。

phase 固定为 `generating`、`final_pending`、`completed`、`failed`、`cancelled`、`interrupted`。失败/取消/中断 GET 只返回已确认前缀，避免新展示剩余文字。模型失败与显示 ACK 可能交叉：failed 且 session generation/learner epoch 未变时，已在途、顺序正确的显示 ACK 仍可落库；它不应用任何候选。取消、切会话或重启后的旧回执被拒绝。

## 持久化与最终事务

`tutor_streams` 保存请求绑定、阶段、revision、final_validated、shown_through；`tutor_segments` 保存已严格验证的分段正文、来源和显示状态。它们不保存 thought 或未解析 JSON。

`turns.content`、`last_delivered_hint` 只保存收到显示回执的精确前缀。单纯收到/校验模型片段，不把文字填进聊天 turn 或教学历史。显示确认的事务与最终候选事务分开：即使最后的任务或记忆应用失败，真实显示事实仍然存在。

完整 final 成功且全部段都有显示回执后，使用原 Pending 候选进入事务：把 turn 来源从已显示分段 union 升级为已验证 final.used_evidence_ids，将 stream 标记 completed，再执行原 `apply_reply`，一起提交或回滚。升级来源不能发生在部分显示阶段。先标记完成是为了允许被接受的更正候选在同一事务内推进 learner epoch/generation；保留 final 的完整来源则避免本次回复在自身更正中被误当作仅引用旧事实的历史 turn 删除。Pending 只在事务提交后移除，存储故障可通过重复最后一次确认重试。若所有回执早于 final 到达，后台 final 到达后立即尝试同一事务。

最终 JSON 截断、不一致、来源/schema 失败或 provider 失败时，保留已显示前缀并标记未完成，不应用任务或记忆候选，不把没显示的后文补进 turn，不自动重试或重播。下一轮教学上下文带 stream_phase/reply_complete；即使该 turn 被 8 条历史窗口裁剪，task.last_delivered_hint 也独立带 last_delivered_stream_phase / last_delivered_reply_complete，避免把失败前缀当成完整结论。

会话 generation 和 learner epoch 改变会终止旧流，删除未显示尾部，保留真实显示前缀。删除 turn 的触发器同时清除对应 stream 和 segments；现有忘记档案流程删除其 turns，因此新表不留下被忘记的正文。重启将未完成 final 标记终止并移除未显示尾部。

## 取消与锁顺序

不再在 `provider.cancel()` 后立即 abort 生成任务。每个服务器运行有取消 watch；取消先推进 session generation、发送 watch/provider 取消信号，并清理 Pending，运行项保留至任务自行完成。新请求仍排同一 provider gate，旧 ACP 持有 gate 到 Job、stdout/stderr 和自有目录收尾结束。

保存分段等待 control 时也监听取消，避免关闭/删除持 control 时，任务停在保存回调里而无法继续轮询 provider 取消。服务器不能持 control 等待整个生成 task；它在 provider 完成后也需要该锁检查旧 generation。关闭只在 control 内发送取消，然后释放锁，再等音频/provider 和服务器任务收尾。提交新问题在锁内拒绝关闭中状态。

此实现没有声称远端模型收到 cancel ack。既有合成 ACP 取消验证只证明本地受控返回与自有进程/目录清理。

## 文字与音频边界

传统整条 `/delivery` 拒绝分段 turn，防止跳过逐段显示协议。UI 的旧 rendered/Delivered/pending_read 自动朗读流程跳过所有分段 turn。分段路径仅 completed 的整条 turn 允许用户明确点击手动朗读；failed/final_pending 不可通过旧音频接口朗读。自动分段播报尚未接入，也没有制造 speech_started/completed 回执。

## 验证与剩余边界

新增 14 项无模型测试：Store 的顺序/重复回执、失败尾部隐藏与迟到确认、generation/epoch、重启、忘记清除、最终事务及其自身 epoch 变更、历史归属；服务端配置门限、候选存储故障后精确重试、失败前缀上下文、历史窗口裁剪、完整来源升级后自身更正保留本次回复、取消 control 锁交叉，以及真实 loopback HTTP 鉴权/origin/过期回执。普通历史页仍保留原有 5 字段合同，仅分段历史增加 delivery_mode / stream_phase。测试仅使用临时数据库、合成文字和假取消任务，没有摄像头、麦克风、扬声器或远程模型请求。代理没有运行 Cargo，根代理统一验证后补充结果。

上游可选 ACP 的真实中英文合成完整 final 已由根代理通过，各 2 段：中文第二次 consumer 首段约 10029 ms、final 约 12352 ms；英文首段约 11884 ms、final 约 12556 ms。取消 300 ms 探针本地取消到返回约 21.33 ms、清理通过，无远端 ack 证明。这些是模型适配器探针结果，不是本轮浏览器逐段显示的端到端测试，也没有通过低延迟目标的结论。

生产默认仍 whole_reply。后续启用前还需验证真实浏览器可见性/滚动/切档/历史视图及实际源程序端到端显示耗时；后续音频阶段需要 Rust 音频 owner 的独立播放回执和顺序播放队列。

### 根代理集成验证（02:11）

完整 workspace 两轮均 457 通过/0 失败/2 忽略，包含本模块新增测试；全部原生 release 和分句 UI WASM 构建成功。新增 `scripts/validation/tutor_stream_http_probe.py` 在独立 simulator 数据库运行实际发布构建和现有已核验 Kimi：中文边到边发模拟显示确认、英文完整 final 到达后才确认，每种两段，精确前缀落库、重复确认幂等及取消旧代次拒绝均通过。证据 `runtime/goal-baselines/20261104/tutor-stream-http-first/report.json`。

首段 HTTP 可见约 9.09/10.56 秒，final 约 13.20/12.77 秒；仍不满足实时教学目标。测试没有打开浏览器或音频设备；模拟显示回执只验证协议和持久化，不能记作 DOM/用户实际看到。首轮计时使用 Windows 粗粒度 monotonic，取消 HTTP 响应的 0ms 不能当作零延迟；后续脚本统一使用 perf_counter，原报告保留。
