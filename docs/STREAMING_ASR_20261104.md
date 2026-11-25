# Rust 录音与本地流式 ASR（2026-11-04）

`audio.rs` 继续独占 WASAPI 设备、录音代次和最长 20 秒的内存音频；`audio/streaming_asr.rs` 只实现当前 Zipformer 本地服务客户端。Python 端不采音、不播放、不保存录音。未配置服务时沿用 whisper.cpp；服务已发布 `runtime/speech-service.json` 后，新录音自动尝试流式识别。

## 数据路径

WASAPI 回调仍写入完整的有界单声道 f32 录音缓冲。设备 owner 每 20 ms 检查取消、设备错误及新增采样，按连续采样位置增量转换为 16 kHz PCM16LE，每 100 ms 发送一块。转换保留跨块位置，尾部在停止时补齐；不是对每块分别重采样。网络工作在独立 Tokio 任务，owner 不等待 HTTP。

发送队列最多 8 块（每块 3200 字节），每个会话顺序处理请求。队列已满、服务中断、返回序号/代次错误、响应超限或收尾失败时，停止该流式会话并显示回退原因。完整 20 秒录音仍在 Rust 内存内，停止后使用 Whisper 转写；不会跳过音频块继续声称流式结果完整。若 Whisper 自身不可用，原有错误路径向界面报错。

停止命令先在设备 owner 内释放麦克风，再等待流式收尾或离线转写。取消通过录音 generation 使旧任务失效，owner 20 ms 检查，流式请求同样每 20 ms 检查。旧网络回复不能更新新录音字幕；远端取消绑定独立 UUID 和 generation。cancel HTTP 最长等待 300 ms；这只是本地等待上限，Python 正在执行原生推理时不能据此声称远端已及时确认取消。

## 本地推理协议

描述符只有 `protocol_version:1`、`base_url`、`token`。Rust 仅接受指定 `http://127.0.0.1:非零端口`，禁代理与重定向，无 URL 账号/路径/查询。每请求带 Bearer 凭证，不把凭证或识别正文写入运行日志。

- `POST /v1/asr/start`：Rust 随机 `session_id`、`recording_generation`、`sample_rate:16000`、`channels:1`、`encoding:"pcm_s16le"`、`language:"auto"`、`max_duration_ms:20000`、`protocol_version:1`。
- `POST /v1/asr/{session_id}/chunk?sequence=N`：`X-Recording-Generation`，原始 PCM16LE，序号从 0 连续递增。常规 3200 B，尾块可短于 3200 B。
- `POST /v1/asr/{session_id}/final`：JSON generation 和下一 sequence；服务须已接受所有之前的块。
- `POST /v1/asr/{session_id}/cancel`：JSON generation；旧 UUID 不影响新会话。

start/chunk/final 响应均检查 protocol_version、session_id、generation、revision、text 和 final；chunk/final 还检查 sequence。revision 仅在文字变化时递增。响应最多 64 KiB、转写最多 8000 字符。普通请求 2 秒，final 5 秒，Rust 收尾等待总限 7 秒。所有界面交互仍经过主程序原有本机认证。

## 字幕与确认

新增 `GET /api/audio/status` 仅返回音频状态，不读取学习者数据库。`asr_partial` 为 null 或 `{recording_generation,revision,text}`；界面仅接纳当前录音代次。`asr_streaming_state` 表示 disabled/starting/streaming/finalizing/fallback/idle，`asr_fallback_reason` 保留明确原因，`asr_provider` 表示当前实际路径。

partial 仅留内存作临时字幕。新录音、取消、设备错误和最终收尾会清除它；不写输入框、不保存、不向模型投递。最终 stop 返回仍为可编辑文字并带 `requires_confirmation:true`，同时给出 provider/fallback_reason。`asr_ms` 对流式路径表示停止后等待结果的时间，不等于整段音频时长或端到端可听延迟。

## 验证

新增 6 个不触设备测试：44.1 kHz 增量重采样与整段转换逐样本一致；有界队列满后保留完整原录音；真实 loopback 假服务的 partial/final/序号和完整 PCM；服务 502 明确回退；旧取消不能清新字幕或新服务会话；协议身份/修订号/超限与本地地址限制。

另有默认忽略的 `real_local_service_fixed_synthetic_wav_pipeline`：仅读取既有 `sapi-corpus` 合成成人语音，内存转为 44.1 kHz，再以 20 ms 节奏经过本次生产 pump/HTTP/Zipformer 路径；输出 first partial、final 等待与合成文本。根代理单独执行，避免常规测试访问推理服务。它不打开麦克风/扬声器，不测试浏览器字幕渲染，也不代表儿童、口音、噪声或自然对话效果。

本子任务仅完成源码检查与 rustfmt；Cargo、合成服务集成和最终结果由根代理统一验证后记录。服务生命周期启动脚本与依赖由根代理负责。
