# 录音上下文绑定与草稿保护后端

本片把一次录音绑定到其实际学习者、数据空间、档案 epoch、会话 generation 和教学语言。保留原 AudioService 的录音 generation 取消机制；不启动新识别服务、不改变语音模型生命周期、不自动发送转写，也不打开常驻麦克风。

## 接口

- `POST /api/audio/record/start`：`{session_id, expected_generation?, device_id?}`。在控制锁内先验证 expected_generation 和录音/转写空闲，再由服务端取消旧教学轮次一次。返回原音频字段及实际 `{session_id, session_generation, language, recording_generation}`。
- `POST /api/audio/record/stop`：新客户端发送上述四字段。停止前验证完整上下文；识别期间释放工作台和数据库锁；识别结束后重新检查上下文及录音编号，过期结果返回错误，不返回旧转写。有效结果带同四字段和原 text/provider/metrics。
- `POST /api/audio/record/discard`：新客户端发送同四字段。只清理已登记且音频编号仍匹配的捕获。允许教学轮次已变化后的迟到开始回执清理自己的旧捕获，但不能丢弃新录音，不返回文字。
- `POST /api/sessions/{sid}/turns`：新增可选 expected_generation，在取消、递增轮次及修改输入节流时间之前验证。auto_speak/client_submission_id 的已有语义保留。

旧手动客户端可省略 expected_generation，stop/discard 仍可只发 session_id 与 recording_generation；服务器必须已有真实 start 绑定，不会根据 stop 请求臆造上下文。只要提供 session_generation 或 language 中任一字段，就必须同时提供二者并严格匹配；null、负数及字符串代次不被接受。

新界面应移除录音前额外 sessions/cancel，保存开始回执，并按草稿 revision 决定是否采用最终转写。迟到回执只发精确 discard，不取消当前教学轮次。此文的后端校验不等于已完成浏览器草稿体验验收。

## 实现与验证范围

生产逻辑集中在 `src/recording.rs`。Workstation 保存一个当前录音上下文槽；不存在额外通用任务队列。旧槽不参与“是否允许开始”的判断，音频已失效后可由新开始替换。旧 stop/final 不会清除新槽。关闭时原有“先同步失效运动、播报和录音，再等待教学锁”的顺序保留。

`audio/recording_test.rs` 只在测试构建中存在，以假 capture/speech 通道确认真实接口调度顺序；不创建输入流、不播放声音、不加载识别模型、不连接模型服务。8 项新增回归：

1. `recording_start_binds_actual_context_and_bumps_only_once`
2. `stale_start_and_text_submit_do_not_cancel_or_change_input_debounce`
3. `recording_partial_or_mismatched_context_never_consumes_capture`
4. `legacy_recording_stop_uses_registered_context_without_inventing_it`
5. `old_recording_controls_and_final_cannot_consume_or_clear_new_binding`
6. `final_rechecks_session_language_epoch_and_owner_after_unlocked_asr`
7. `invalidated_binding_does_not_block_next_recording_start`
8. `exact_discard_can_clean_its_capture_after_teaching_generation_changed`

根代理已完成该片统一验证：workspace22 总计 597 通过、4 忽略，native12 与 WASM 构建成功，release-smoke-eighth 通过，并保存第八检查点。这里的录音回归仍是假通道；真实麦克风、真实儿童语音、浏览器操作与端到端草稿验收尚未进行，不能据此宣称这些已通过。

原文件备份：`runtime/goal-backups/20261104/backend_review/recording-binding/`。
