# 实验性半双工轮换 UI 接线

统一验证补充：第十一检查点 `workspace-twenty-ninth.log` 共696通过、0失败、5忽略，UI为119通过（本片10项）；`wasm-voice-loop-first.log`构建成功。新主程序HTTP smoke的15个轮换拒绝场景均保持原状态。后端当前heartbeat撤销尚未使用的重开许可时，UI也退回等待，旧回执不能复活。真实麦克风窄打开/丢弃/重开另有通过记录，未覆盖本UI的真实两轮完整对话；浏览器验收仍NOT_RUN。下方“未运行”描述保留为子片交接时状态，以本段和Goal检查点为最终结果。

本片实现 `tutor-ui/src/voice_loop.rs` 的领域状态及 App 真实接线，未启用生产策略，默认仍为手动录音、检查与发送。只有 `state.voice_loop.available=true`、现有 `segments + after_display` 配置、当前活动、已明确选择的稳定麦克风、空草稿且无待核对结果时，才能明确选择“开始 / 继续”。缺字段的旧后端不可开始。

## 实际路径

- 独立麦克风选择保存 `device_key + input_topology_revision`，不从默认设备、名称或旧枚举 index 推断许可。UI 还核对当前音频拓扑版本。
- enable 的完整 echo 绑定当前会话、语言、设备、客户端意图及服务端 epoch。每次明确继续使用新意图；迟到 enable 只能为已暂停意图补充精确 stop 标识。
- 1 秒间隔、单请求在途 heartbeat；请求限时 1.5 秒，服务端 lease 为 3 秒。许可过期控制由后端执行；这些是配置边界，尚非设备实测保证。
- Start 使用 `voice_endpoint_v1` 和稳定设备绑定，不先发全局播放停止。绑定服务端完整录音票据与 loop echo。自身 Start / Submit 的预期代次变化分别只允许 +1 / +2，旧 heartbeat 不回退新状态。
- 复用 `LiveTranscript` 的 200 毫秒、单请求在途状态获取。仅当前录音、方法版本匹配、`input_closed=true` 的 `speech_end` 触发完整票据 Stop。临时字幕停更不是终点。
- 最终非空文字仍通过原 `RecordingDraft` 的开始时空输入及 revision 未变门限。只有 Fill 且 loop 仍有效才单次自动 Send，固定 `auto_speak=true`、本轮录音 token 和 loop 标识。已有文字或任何编辑不会被覆盖。
- 自动 Send 成功回执必须核对 nonce、会话、request、generation、分句模式及完整 loop echo；不能核对则保留“发送结果待核对”卡，禁止自动重发。后来的 state/head 不代替该发送成功回执。
- 下一次录音仅由成功发送后、匹配 heartbeat 的 `armed + rearm_ready` 驱动。UI 的 Done、单段音频完成或文本 final 均不允许重开。后端负责整条回复的显示/播放完成、输出 owner 退出和连续 500 毫秒尾间隔。

## 暂停与终点边界

编辑、取消自动朗读、隐藏页面、查看历史/上翻、切档/教学设置、停止、连接异常及归属变化都会撤销本页意图；返回页面、勾回自动朗读或迟到成功回执均不恢复。主“停止”和 Esc 保持停止生成/播报功能；实验卡另有明确暂停入口。

`no_speech` / `device_error` 暂停并精确清理，不提交文字。`duration_limit` 先完成完整票据 Stop，并撤销自动填入资格，把可返回的文字保留到未采用转写卡；等待这一终稿时暂续约，避免提前 stop lease 把正在转写的完整音频废弃。后端同样拒绝把此终稿当自动发送许可，返回后 UI 暂停。22 秒 UI 看门狗只负责无终点回执时的停止，不能产生语句终点或自动发送。

这是实验功能而非课堂验收结论：冻结 VAD 的 4 秒思考停顿样例会截断后续语音，实际扬声器尾音和儿童/真实噪声场景未通过验证。半双工不支持用说话打断正在播报的回复；需按停止或 Esc。

## 验证状态

本片新增 10 个纯状态回归：完整循环须服务端 rearm、迟到 enable 只关闭、编辑暂停不可复活、设备/意图归属、Start 代次与语言、旧终点/heartbeat、duration limit 禁发送、发送未知后禁恢复、Start 完整 echo，以及新 heartbeat 撤销尚未使用的重开许可。已有草稿与未知发送保护继续使用原模块。

独立 HTTP smoke 脚本另新增 15 个反例：默认未启用、错误 enable、无现存/无效 epoch 的 heartbeat 与 stop、无许可或过期 Start/turns、缺少配对录音字段。每一例都核对会话代次/状态/原文、录音代次、拓扑、模型请求计数及 loop 投影不变，不枚举或打开设备。脚本仅完成语法检查，运行交给根代理。

已执行 rustfmt；Cargo、WASM、端到端请求与设备验收由根代理统一执行。本文件写入时未宣称上述测试已运行，也未启动麦克风、模型或浏览器。

备份：`runtime/goal-backups/20261104/ui_review/voice-loop-v1/`。相关冻结设计见 `HALF_DUPLEX_VOICE_LOOP_CONTRACT_20261104.md`；声学实际失败边界见 `VOICE_ENDPOINT_20261104.md`。
