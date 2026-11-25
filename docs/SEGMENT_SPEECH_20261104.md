# 分句音频接线（默认关闭）

本批已于02:56纳入 root 第五软件检查点：完整workspace 517 passed、0 failed、2 ignored，UI86项；原生所有bin release和WASM构建成功。真实release+Kimi的关闭后继续文字投递合同通过，音频保持关闭；fake speaker验证顺序和错误路径，未作扬声器实听。详见 `GOAL_PROGRESS_20261104.md` 与 `runtime/goal-checkpoint-20261104-fifth.json`。

## 开关与接口

沿用 `runtime/tutor-delivery.json`，新增可选 `segment_speech`，值为 `disabled`（默认）或 `after_display`。只有 `mode=segments` 允许后者。本轮没有创建或修改运行配置，默认整句投递与 SAPI 路径保持。`tutor_delivery.automatic_speech` 在两种 segments 配置下一律 false，防止旧 UI 把分句又整篇自动朗读一遍；新增 `tutor_delivery.segment_speech` 表示分句能力。

`POST /api/sessions/{sid}/turns` 新增可选 `auto_speak:boolean`（缺省 false）、`client_submission_id`（非空 ASCII 关联 nonce，最长 96，服务端接受字母数字及 `-_.:`）。只有配置开启且本次 true 才创建 open 语音流。nonce 只关联界面提交意图，不是授权或通用幂等键。回执回显 nonce、request_id、generation、delivery_mode、speech_gate；旧客户端不发送这些字段时不会启用分句音频。

head/snapshot 增加 `speech_gate: disabled|open|closed`、`speech_close_reason`、`speech_completed_through`、`speech_complete` 和关联 nonce；内部同时保留固定 `speech_gate_audio_epoch`。原文字 phase/final/shown 字段不变。

`POST /api/sessions/{sid}/streams/{rid}/speech/close` 接受 `{generation,reason}`，UI reason 为 `user|auto_speak_off|hidden|history`，返回对应流的语音摘要。关闭不可逆，不改文字投递；重复或迟到关闭只针对原请求，不停止之后的手动朗读或新流。手动朗读完整 stream turn 仍调用原 playback API，后端先关闭自动队列，再开始用户明确请求的整篇朗读。

## 顺序与回执

`speech_delivery.rs` 继续拥有全局音频生命周期，子模块 `segment_speech.rs` 只保留当前流身份、固定音频代次和一个执行任务。Store 的现有分段行与 DeliveryLedger 是投递事实的唯一账本，不复制正文队列、不预合成、不并行调用 TTS。每次领取一段已显示且前序全完成的文本，先提交 reservation，再调用本地音频，保存真实 Started 和 Outcome 后才领取下一段。

无已显示新段时执行器退出并释放前台占用；显示 ACK 或 final 到达再次唤醒。检查无工作和退休标记与显示调度使用同一 control 锁，防止漏唤醒。数据库锁不跨音频等待。显示 ACK 晚于 final 也顺序播放；整篇文字 completed 不代表音频 completed。final 应用更正导致 generation/learner epoch 改变时，返回更新后的语音 gate/revision，并停止旧语音身份。

同一流始终使用创建时的音频代次，不能因新 ACK 自行续发新代次。停止先原子失效该代次，然后持久关闭；即使数据库暂时无法保存关闭，旧流也不能恢复声音。绑定关闭使用 expected-epoch CAS，旧请求不会废除之后手动播放的新代次。全局停止及 shutdown 在等待教学锁之前取消物理音频；shutdown 保持先取消云台/音频、再等待教学锁的既有顺序。

模型/完整文本校验失败、音频失败、开始或终态回执保存失败均关闭本流后续语音。部分播放不自动整篇重放，未开始不伪造 Started；gate 关闭和进程重启产生的状态与真实 owner 终态分开记录。CPAL `first_callback_submission` 与 SAPI `sapi_speak_accepted` 保持原义，`speaker_output_measured` 仍为 false。

## 本批后端测试

新增 8 项 fake speaker 通道/临时 SQLite 测试：final 先于显示 ACK 仍逐段顺序、文本完成不等于音频完成；首 ACK 前关闭及迟到重试不伤后续 manual epoch；教学锁忙时仍先取消音频；关闭事务失败后晚 ACK 不续命；开始回执写入失败停止后续；终态写入失败不推进；provider 失败关闭已显示尾段；learner epoch 改变后晚 ACK 不发音。另新增 1 项配置测试，验证默认关闭及 segments 不进入旧整篇自动链。

Store/ledger 和 UI 分别由对应代理实现、汇总验证。浏览器隐藏关闭请求属于尽力网络通知，页面冻结或断网不等于已确认停声；不据此宣称硬实时静音。本切片仍需统一测试、原生/UI构建与之后的实际设备验证。
