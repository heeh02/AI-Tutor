# 分句播报 UI：意图与关闭边界（2026-11-04）

本片落实已与后端冻结的 UI 合同。声音合成、逐段排队、真实开始/完成收据和音频 owner 均由 Rust 后端负责；浏览器只提交本次选择、显示确认和精确绑定的关闭请求。未创建或修改生产启用配置，未启动浏览器、模型、音频设备或其他设备。

## 合同

`/api/state.tutor_delivery.segment_speech` 为 `disabled|after_display`，缺省按 disabled 显示；segments 模式的原 `automatic_speech` 仍为 false，不能进入旧整篇 `Delivered → Read` 链。whole_reply 保留原流程。

`POST /api/sessions/{sid}/turns` 携带 `text,auto_speak,client_submission_id`。`auto_speak` 在发送时取值，服务端仅在配置 after_display 且该值为 true 时开启本流。旧客户端未传时服务端默认 false。每次新的 Send 使用独立 nonce；本 UI 没有自动重发 turns，取回失败输入后再次 Send 是新请求。nonce 仅关联意图，不作为授权或服务器幂等键，也不通过相同文本猜测归属。

submit 回执保留 `request_id,generation,delivery_mode`，增加 `speech_gate,client_submission_id`。当前 stream head/snapshot 回显 nonce，并独立带：

```json
{
  "speech_gate":"open",
  "speech_close_reason":null,
  "speech_completed_through":null,
  "speech_complete":false
}
```

文字的 `phase,final_validated,shown_through` 独立。UI 不根据文字完成推断音频完成，也不把配置 open 显示成已经开始播放。完成段数只读取后端实际收据投影。

`POST /api/sessions/{sid}/streams/{rid}/speech/close` 提交 `{generation,reason}`。UI reason 为 `user|auto_speak_off|hidden|history`，返回 `{ok,request_id,generation,speech_gate,speech_close_reason,speech_completed_through,speech_complete}`，可附加 revision 等字段。关闭原流、幂等旧代次关闭、重复关闭不误停新的手动播放，均由后端绑定验证。没有 reopen 接口。

手动朗读 completed 的 stream turn 仍使用一次原 `/api/audio/playback`；后端先原子关闭自动分句，再显式整篇播放。UI 不把它拆成两次有竞争窗口的请求。

## 模块与行为

- `speech_intent.rs` 是纯状态：当前会话、最多一个未绑定提交意图与当前流的关闭标记。同会话只接受更高 generation 或同一个 request，避免提交回执之后较早发出的 state 旧 head 擦掉关闭意图。没有正文或音频队列。
- App 在取消勾选、明确打开历史、页面隐藏或“关闭本条自动朗读”时记录不可逆的本流关闭意图。勾选文案为“下次发送自动朗读”，勾回和返回页面不恢复旧流。
- submit 响应丢失时，state head 中精确匹配的 nonce 仍能绑定并关闭。新 Send 替换未绑定的本地提交意图；旧 nonce、旧代次和旧会话的回执不能套用到新请求。已有精确绑定的 HTTP close 不会改成新请求的路径。
- close 全程最多一个 HTTP 请求在途。失败保留标记，依现有状态轮询重试；界面明确提示“关闭尚未确认”。暂时缺少 head 不清除已有标记；服务器 closed 也不会被随后旧 open 状态重开。
- `TutorStream` 只新增独立 ACK 许可代次。在关闭尚未确认时，保留文字并继续有界快照轮询，暂不发送新的显示确认。关闭确认后恢复文字 ACK；已发出的合法 ACK 不伪撤回。旧 rAF/预发送回调即使经历关闭再恢复也不能复活。发送前被撤销属于本地控制，不误报网络错误。
- 本片不把普通 scroll 事件视为停声，因为内容变化、布局与程序滚动也可能触发它。显式“查看更早消息”会关闭声音；原有向上阅读的文字 ACK 门限保持。

隐藏事件的网络关闭是尽力发送。浏览器冻结、断网、关页或 HTTP 请求已经在途时，不能承诺零延迟静音；本地标记阻止重新开启意图，服务端负责取消已排队/正在播放的段。模型或 final 失败后的声音关闭由后端负责，实际已播收据仍保留。

## 验证状态

已写 11 项纯逻辑回归（10 项 SpeechIntent、1 项 ACK 许可）：丢失提交响应、精确 nonce、不相干 head、不重开、关闭失败与文字独立、旧会话/旧关闭、缺省 disabled、暂缺 head、服务端 closed、POST 新 head 后旧 state、rAF 许可撤销。它们尚待根代理统一 Cargo/WASM 验证。已运行 rustfmt 检查和 fixture JSON 解析，不宣称编译或浏览器通过。

合成双语人工脚本：`rust/tutor-ui/fixtures/segment-speech-20261104.json`，12 项均 NOT_RUN。必须使用隔离 simulator 数据；没有合成真实开始/完成的浏览器回执。实际发声、设备延迟与自然度仍需独立测量。

上一片比较 UI 已由根代理在 checkpoint-fourth 验证（整体 484 tests、UI 75、WASM/release 及独立 HTTP/Kimi）；这些结果不覆盖本片新增代码。

修改前备份：`runtime/goal-backups/20261104/ui_review/segment-speech/`。
