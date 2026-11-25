# P1 可选文字流式客户端 · 2026-11-04

## 范围与模块

本轮实现 Rust/Yew 文字显示，不增加分句音频队列。默认 whole_reply 保持；服务端显式配置 `runtime/tutor-delivery.json` 的 `protocol_version:1, mode:segments` 才启用。开发过程中未创建或修改生产启用配置，未启动模型、设备或浏览器。

- `tutor_stream_state.rs`：绑定会话、request、generation、turn；解析完整有界快照；连续序号、不可改写文本、单调 revision；缓冲与显示前缀分开。
- `tutor_stream.rs`：单请求在途的 150ms GET，逐段单请求在途的显示确认，页面/历史/请求变化时撤销显示代次，拥有计时器、rAF 和可见性监听。复用现有同源认证 API 与小型浏览器资源所有者，没有新 WebSocket 或 JS 业务逻辑。
- App/view：固定 turn_id 原气泡嵌入组件；流式轮次永久绕过旧整篇 delivery/autoRead；显式停止或替换请求立即阻止旧请求继续确认；同一 turn 的新文字沿用上翻不跳底部的策略。

## 冻结接口

`GET /api/sessions/{sid}/streams/{rid}` 返回完整快照：

```json
{"protocol_version":1,"session_id":"s","request_id":"r","generation":3,
 "turn_id":"t","revision":2,"phase":"generating","final_validated":false,
 "shown_through":0,"segments":[{"sequence":0,"text":"先观察。"},{"sequence":1,"text":"Then compare."}]}
```

序号从 0 开始。最多 12 段、每段 320 字符、总计 1600 字符。每次 segments、phase 或 shown_through 改变都递增 revision。同 revision 内容一致。完整快照及去重使重试不依赖消息传输次序；过期 revision 无效，段缺口或已存在文本改写拒绝。

`POST /api/sessions/{sid}/streams/{rid}/display` 使用 `{"generation":3,"sequence":0}`，幂等返回完整快照。phase 是 `generating/final_pending/completed/failed/cancelled/interrupted`。

`state.tutor_delivery` 提供 mode、protocol_version、automatic_speech；`session.tutor_stream` 提供同 head、不含 segments。后台立即建立固定 assistant turn，占位也必须标 `delivery_mode:"segments"`；持久 turn 保留该字段及 stream_phase，终态 head 保留到 generation 改变。

## 显示与失败语义

仅把“已确认前缀 + 下一段”放入 DOM，其余段留在最多 12 段的内存快照。Yew rendered 之后、当前页面可见且非历史的一次 requestAnimationFrame 机会才确认下一段。还需当前跟随实时窗底部，并且该分句的实际矩形同时与聊天滚动区域、浏览器视口相交；确认发出前再次核对。该名称是显示确认，不是显示器实际呈现计时，也不是人已读证明，相交也不代表整段已被阅读。页面隐藏/历史/上翻/切会话/停止会使已排队 rAF 及迟到响应失效；请求已合法发出后再隐藏，后台可能完成原确认，但前端不会据其开始音频或显示后续新段。

上翻跨越跟随门限即重绘，不能仅依赖新回复徽标变化。上翻期间暂停组件轮询；主 state 发现同一请求 head 推进时只显示返回入口，不强拉底部。同 turn 的新分句 DOM 提交通过 `StreamProgress → content_changed → App.rendered` 请求跟随底部。内容增高不自行改写跟随意图；rAF 若暂未看见当前分句，仅推迟确认。真实浏览器中滚动、重排和绘制机会的时序仍需现场验证。

最终完整校验且全部显示确认后，后台同 turn_id 升级已完成，不新插入全文、不整篇自动朗读。final 晚于所有确认或最终事务暂失败时，允许只重复最后一个已有确认来完成幂等收尾。

失败响应仅含后台已确认前缀；客户端保留最多一个已经 DOM+rAF 提交但回执未返的分句，标注未完成，不引入失败后未显示的尾部。`failed` 且仍是当前后台代次时，允许重试这一条已有显示确认；`cancelled/interrupted` 不再生成确认。服务器持久记录以确认前缀为准，未确认文字不能成为完整教学。

完成的流式轮次可主动朗读；生成中或未完成轮次不可朗读。整个流式模式不自动朗读，原自动朗读偏好保留供默认模式使用。停止播放不会使之后的分句重新启动播放。

## 验证状态

新增 12 项 Rust 逻辑回归：前 10 项覆盖完整快照去重/单段显示窗口、段缺口/改写/错误归属、失败交叉确认、final 同气泡完成及晚到重试、未提交 DOM 的失败与取消无回执、协议容量/revision、显示代次、同 turn 增长、历史未完成标记。视口修复另增两项：分句须同时与滚动区域和屏幕相交；滚动门限改变即重绘，同 turn 增高仍维持已选择的跟随行为。

root 已完成文字流式 UI 的 WASM release 构建。首次原生 UI 测试在编译 fixture 时失败：重复数组 `&[&part; 6]` 未将 `&String` 转为 `&str`；修正后 root 第二次 UI 原生测试通过，保留 `ui-tutor-stream-first.log` 与 `ui-tutor-stream-second.log`。这些通过结果早于本次视口修复；当前新增视口代码和两项测试仍等待 root 重跑，rustfmt 检查通过。`rust/tutor-ui/fixtures/tutor-stream-20261104.json` 是合成中英任务及协议输入，所有浏览器任务均为 NOT_RUN，不代表真实 DOM/rAF 已验收。

待真实浏览器确认：显示机会后的 POST 时序、后台标签切换、历史返回、新会话迟到请求、网络响应丢失后的幂等确认、固定气泡升级、窄屏换行、无自动音频。无需真人、摄像头、麦克风或云台即可完成这一轮界面验证。

修改前备份：`runtime/goal-backups/20261104/ui_review/tutor-stream/`。

视口修复备份：`runtime/goal-backups/20261104/ui_review/tutor-stream-viewport/`。仅 root 给 web-sys 增加 `DomRect` feature，没有依赖版本升级。
