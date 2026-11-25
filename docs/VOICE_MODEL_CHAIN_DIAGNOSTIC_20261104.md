# 固定合成语音的真实模型链诊断

## 第十三检查点实际运行

`voice-model-chain-first/report.json` 已完成中英两条固定成人SAPI音频的真实链路，外层exit0、总176.626秒。Qwen graph新fixture首次ready观测113.828秒；中文预热首次PCM为2829.55ms，英文47.52ms，单独保留而不混入已预热结果。每语言各一次实际教学ACP请求、无重试，两个ASR实例和自有TTS均正常退出，未强制清理；原安装、已有服务descriptor和策略哈希一致。

| 本次单样本观测 | 中文 | 英文 |
|---|---:|---:|
| 首次非空字幕被观察到 | 832.35ms | 834.21ms |
| 音频送完至ASR终稿 | 26.70ms | 26.32ms |
| 教学请求至首个有效段被观察到 | 13470.44ms | 11542.42ms |
| 教学请求至完整final | 15778.26ms | 17345.85ms |
| 全部段数 | 2 | 3 |
| 各段首PCM接收范围 | 24.44–36.58ms | 25.80–59.76ms |

中文终稿“先让小车停下来”、英文“FIRST STOP THE CAR”保留固定源意思，差异仅标点/大小写；两条均未请求回退，也未运行Whisper。中文两段共253440个24kHz样本（10.56秒），英文三段382080个样本（15.92秒）；接收样本数与各段transport计数一致，非零样本存在。原PCM未落盘、未送扬声器，不能由以上首包时间推出实际首音或听感达标。每语言只有一次，不能汇总成群体P95；字幕仍未达700ms参考目标，教学模型的首段等待仍明显超出整体实时目标。

本次诊断刻意在完整教学final之后才依次合成，不测生产逐段重叠播报的端到端延迟。根代理与独立代理核对报告hash、逐段拼接和所有权回执均一致。开放内容核对观察到中英文策略不同：中文将指令作为目标并询问模块连接；英文明确不能代停、建议实际Stop/电源后反馈。两者未声称已停车或已完成实验，未应用目标候选或写入慢记忆。但这不证明中英教学质量一致，更不是儿童/真实房间的质量验收。

中文报告SHA `5a3f26c3af958cd6718af7d222a9e28e062bd929d79e1b009ef23012240c85b4`，英文 `a17e326008cd1be52e798e03c5141e25ffd3964e4997c6ea6b4b5dcc2338bc5b`。外层报告给出其唯一目录及原始子报告路径；两份均保留 `voice_loop_permission_or_rearm_tested=false`、`speaker_output=false`。完整设备轮换、实际扬声器起止、尾音、自然度、长停顿、儿童/混语与联合连续教学负载仍待独立验证。

状态：2026-11-04，第十三检查点已实际执行并完成上表链路；实现初期 workspace32 的编译记录不再是运行状态。该 transport 诊断不打开麦克风、扬声器、摄像头或云台。随后新增的真实生产分句播放是独立模式，见 `PRODUCTION_SEGMENT_PLAYBACK_DIAGNOSTIC_20261104.md`，不回写本次 transport 方法和结果。

## 路径与边界

`audio/voice_model_chain_diagnostic.rs` 是仅 Windows 测试构建可见的 ignored 库内诊断。输入只允许既有成人 SAPI 合成语料的 `zh_short` / `en_short`，校验固定 SHA 和原 manifest 来源声明，不能传入任意 WAV。先把 16kHz 单声道 PCM 变为 44.1kHz，按 20ms 实时时序送入现有 `streaming_asr::begin/pump/finish`。识别服务是本次 fixture 自有的 Python 进程、动态 loopback 端口及私有 instance/token；仍使用已验证的 Windows Job、实际解释器归属、ready、静音预热和退出协议，不访问旧 18867 服务。

实际非空 ASR final 写入新的 simulator SQLite，建立合成来源事件和用户 turn，经生产 `build_context` 交给真实 `KimiProvider::generate_segments`。必须已有原生无工具 canary 核验成功，不启用测试 transport；每次执行恰好一个教学请求，不自动重试。模型片段继续通过生产 `SegmentDecoder`、完整回复校验和 Store 流身份校验。没有显示 ACK，没有应用模型建议或慢记忆，也不把合成用户发言当真实儿童事实。

完整教学 final 校验之后，再按顺序把各段交给真实 `TtsClient`。有界 PCM sink 只统计数量、首包时间、非零样本及 SHA，随后清零，既不保存原始 PCM，也不创建任何 `SpeechStarted/Completed`。这是**网络合成数据接收**，不是音频输出 owner、扬声器首音、物理播放结束或声学质量测量。它也未调用真实 Workstation 的录音/Send/heartbeat/重开许可；完整接口状态闭环另由 `voice_loop_integration_tests` 验证，两类证据不能合并宣称完整现场自动轮换通过。

## 自有服务 runner

`scripts/validation/voice_model_chain_smoke.py` 只调用预先编译的 Rust 库测试 `.exe`，持有 GPU 服务期间不编译 Cargo。固定使用已有成功 graph Qwen fixture 的模型硬链接身份与固定 YAML SHA，重新验证下载 manifest 的模型 SHA，再把当前七项 launcher/bundle 资产复制到全新的输出 fixture。模型不下载、不复制大权重，旧 fixture 不写；新运行缓存属于本次新 fixture。方法文件记录实际测试二进制、Rust 诊断另存的方法、runner、所有权解析 helper、bundle、固定配置、模型 manifest 和环境指针 SHA。

runner 通过私有 stdin 给自有 `managed_tts.py` 新 token，经认证 `/v1/models` 和无凭证 401 验证 ready。只做中英各一条固定预热，然后中文、英文各运行一次真实模型链。token 通过 wrapper stdin 和测试子进程环境传入；测试进程的后代除非主动清空环境，也可能继承该新凭据，不能描述为只在单个测试进程中可见。不写 key、descriptor 或 policy，不打印 token。已有 stdout/stderr 解析、脱敏、ready 和 `stop_wrapper` 被直接复用；任何失败都停止后续语言、不重试，并在 `finally` 回收自己创建的 TTS 进程组。没有通过端口查找或杀掉外部进程。

预算保持明确：ready 总预算 240 秒、每条预热 120 秒；前台 Rust TTS 原有 15 秒响应/块等待与单次总期限不变。单语言诊断外围限制 420 秒，自有 TTS 停止 20 秒。外围强制结束测试进程后，缺失的 ASR 终态收据不会被推测为已清理或成功。报告必须实际包含解释器退出、两个 Job 清空、所有权归属、provider 清理和根目录受保护文件哈希不变。共享 Linux flock 仍只覆盖已配置同一发行版/用户下的两个候选，不代表所有 GPU 应用互斥。

## 根侧复现

先在项目 `rust` 目录编译测试，明确选取 Cargo 返回的库测试可执行文件；编译结束后再执行 runner，不要用文件通配猜选旧二进制。

```powershell
cargo test -p ai-tutor --lib --no-run
python scripts/validation/voice_model_chain_smoke.py --real-local-synthetic --root "C:\Users\RBMS\Desktop\AI TUTOR" --test-executable "<本次Cargo返回的ai_tutor库测试.exe绝对路径>" --output "C:\Users\RBMS\Desktop\AI TUTOR\runtime\goal-baselines\20261104\voice-model-chain-first"
```

第二行从项目根目录执行。输出目录必须尚不存在。根侧统一安排 CPU/GPU/模型时间窗；脚本没有自动模型重试。独立库测试的完整名称为 `audio::streaming_asr::voice_model_chain_diagnostic::real_synthetic_asr_teaching_tts_transport_chain`。runner 传入 `AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC=1`、`AI_TUTOR_VOICE_CHAIN_LANGUAGE=zh|en`、`AI_TUTOR_VOICE_CHAIN_TTS_ENGINE=qwen`、唯一 `AI_TUTOR_VOICE_CHAIN_RUN`，以及不记录值的私有 `AI_TUTOR_VOICE_CHAIN_TTS_TOKEN`。库测试本身也支持显式 Cosy lab transport，但本轮 runner 固定 Qwen，不能把未运行的 Cosy 分支视为验证。

外层 `report.json` 保留所有权事件、实际清理结果和两份唯一单语言报告路径；单语言目录另有 `method.json`、`asr.json`、`teaching.json`、每段 TTS 数据统计和隔离 SQLite。文本均来自固定合成来源和其真实模型回复，允许留存人工核对。退出码和结构性 `passed` 只表示链路和清理检查，不表示 ASR 字错率、教学有效性、自然度、儿童/口音/噪声表现通过。需单独人工检查 transcript、回复内容及遗漏。

实现期间完成 Python AST 和 7 项报告合同纯数据检查，记录为 `voice-model-chain-runner-static-first.log`。口径修订后新增“只请求 fallback”的拒绝项，共 8 项通过，记录为 `voice-model-chain-runner-static-second.log`：诊断从未调用 Whisper，故 `whisper_fallback_used` 固定为 false，失败时仅 `fallback_requested` 反映后备请求。首段时间使用 `first_validated_segment_observed_ms`，包括 final 完成后的 channel drain 分支，表示消费者观察时刻。另以随机 loopback 端口上的线程假 HTTP 验证 `Connection:close` 响应仍按剩余预算读取并正常结束，记录为 `voice-model-chain-runner-http-close-first.log`。未运行任何模型、外部服务进程或设备。Rust 验证和真实报告由根侧另行记录。
