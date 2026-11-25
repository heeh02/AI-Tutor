# P4：有界长对话历史浏览

日期：2026-11-04。范围为独立存储查询、临时数据库测试和 Rust/Yew 历史组件。主任务已挂接 Store 模块声明、`session.history_before` 与显式 Axum Query 路由。本子任务未运行 Cargo、浏览器、设备或真实模型。修改前备份在 `runtime/goal-backups/20261104/ui_review/conversation-history/`。

## 接口和存储边界

`Store::turn_history_page(lid, sid, before, limit)` 位于 `rust/ai-tutor/src/store/history.rs`。对应 GET `/api/learners/{lid}/sessions/{sid}/history?before=<rowid>&limit=50`，返回 `{learner_id,session_id,items,next_before}`。

会话与档案通过数据库归属 JOIN 校验；limit 为1–50，before 为可省略的正整数排他上界。查询多取一条判断是否还有更早内容，返回消息按旧到新排列。`items` 仅包含 turn_id、role、content、status、created_at；没有请求内容、来源证据或内部数据库游标。只有分页用的 next_before 暴露给客户端，不显示在界面。已结束会话可读；读取不恢复活动、不确认投递、不改变教学上下文。

`/api/state` 的 session.history_before 指向当前24条消息中的最旧行，UI进入历史时固定此边界。之后的新消息插入不改变已经打开的历史页，也不会造成偏移分页常见的重复/漏项。首个历史页与当前24条分开；重新打开历史才读取新的边界。删除或更正本身会改变可用历史，不能将游标分页解释为永不变化的数据库快照。

## UI和投递语义

`ConversationHistory` 位于 `rust/tutor-ui/src/conversation_history.rs`。组件每次只持有一页最多50条文本，翻页时清除旧正文；回退栈最多64项，只保存 before 和 scroll_top。较新页通过原游标重新读取，渲染后恢复阅读位置。超过回退栈上限仍可返回当前消息，不积累数千个消息DOM。当前24条窗在历史浏览时隐藏，因此页面消息节点上限为历史50条加当前24条。

历史请求绑定档案、会话、初始边界和请求代次；换会话或关闭销毁组件，旧回执不能恢复页面。组件一次至多一个请求在途。请求失败显示重试入口，不以旧页面冒充新页。历史只有浏览功能，没有朗读、投递或发送回调。

输入区仍由 App 持有，打开/翻页/关闭历史不改变 input 和当前session；用户主动发送时仍是当前活动的草稿，不混入历史items。已结束活动继续使用原有禁用输入行为。组件支持中英文标签，消息原文保持不变。

进入历史执行已有 SpeechStop，只停止播报，不取消教学活动；保留正在进行的录音ticket。App::rendered 在历史显示期间不发送新的 shown；Delivered 自动朗读与 pending_read 都有可见状态门控。额外的显示代次阻止进入历史前的迟到shown回执在返回当前后重启自动朗读。已经实际显示的回执仍可保留显示记录，不把停止播报改写成教学未发生。

当前窗的 ChatViewport 在历史期间只记录有新消息，不请求滚到底；隐藏DOM产生的滚动事件不清掉新消息提示。返回当前恢复先前scroll_top，“有新回复·查看最新”才明确跳到底。当前窗仍是滚动的24条：长期浏览后某条消息可能已移出这24条，此时按像素恢复不承诺保留同一条内容锚点，可通过重新打开历史查阅。更正/删除记忆时清除历史组件，避免保留已撤销对话页。

## 回归和验收状态

- 两项临时SQLite集成回归：123条记录分页期间追加消息且混有另一档案，检查完整顺序/互不串档/投影字段；finished会话只读、未投递assistant保持未投递、参数界限。
- 五项历史状态回归：迟到回执换会话隔离、原游标与scroll_top恢复、50条文本及64项导航边界、同会话边界改变及上一页迟到结果、游标必须向更早推进与null终止。
- 三项显示/滚动回归：历史浏览时24条窗口继续推进但不强制滚底；进入历史前的迟到显示回执不能自动恢复播报；显式返回仅重开当前未shown且非在途回复的重试额度。
- `fixtures/conversation-history-20261104.json` 包含130条作者编写的中英合成消息、首个历史页/当前窗预期范围及10组双语操作任务。现场状态为 **NOT_RUN**，特别是DOM实际滚动、焦点、窄屏、停止可听音频与网络延迟交错仍待浏览器/设备验证。

rustfmt --check 与 fixture JSON 解析已通过。Cargo、WASM及真实HTTP路由验证由主任务统一执行；当前不预先宣称这些检查通过。

## 独立自查补充

已只读核对 server 的停止播报、Pending 与 deliver：进入历史所调用的 playback/stop 仅执行 audio.stop_speech，不走 App::cancel、不改会话代次、不移除 Pending。模型结果中的前台保留凭证仍在 Pending 内，只有 shown 与 apply_reply 的事务提交成功后才移除；因此读历史本身不会让后台学习记忆任务抢占尚未真正显示的回复。

发现并修复显示确认重试耗尽后的恢复入口：旧逻辑最多自动尝试两次，耗尽后即使用户主动返回当前也不再尝试，后台 Pending 可能一直保留。现在“返回当前消息”和“查看最新”只清除当前未shown且非在途assistant的重试计数与临时阻止标记，交给现有 rendered 路径重新确认；不直接投递、不修改成功/在途所有权、不改server Pending。成功后即使状态轮询短暂仍报generated，也不会再次开放同一回执或启动重复TTS。

本次增加3项回归后，此功能合计8项UI纯逻辑回归和2项临时DB集成回归。rustfmt检查通过；新增回归仍待主任务统一执行。
