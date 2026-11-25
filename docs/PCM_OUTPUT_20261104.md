# 本地流式语音输出接线

状态：Rust 接线和无设备回归已完成，等待 root 统一编译/测试。没有创建 `runtime/tts-service.json`，默认仍走 Windows SAPI。本轮未启动模型、音频设备或录制真实数据。先前模型 HTTP 测量不等于本机扬声器首音和停止响应实测。

## 模块和接口

`audio/streaming_tts.rs` 负责本地 HTTP 和 PCM16LE 分包，公开 `synthesize` / `QUEUE_CAPACITY` 合同保持；`audio/pcm_output.rs` 负责 CPAL 输出设备和可纯测试的 renderer；`audio/speech_output.rs` 是唯一 speech owner，串行管理 CPAL / SAPI、停止、fallback 和收据。原 SAPI COM 代码从 `audio.rs` 移到 owner 模块，COM 对象和 CPAL Stream 均由该线程创建/释放；网络任务不持有设备。

新增窄接口：

```text
speak_with_receipts(text, language, speed, expected_epoch,
                    oneshot::Sender<SpeechStarted>) -> Result<SpeechOutcome>
```

`SpeechStarted` 含 playback_id、epoch、engine_kind 和 milestone。CPAL 的 milestone 是 `first_callback_submission`，只在回调实际写入过来源 PCM 后发送；SAPI 的 `sapi_speak_accepted` 表示异步 Speak 调用被引擎接受。收据只发送一次，发送方不会等待消费者。原 `speak` / `speak_with_speed` 保留 bool wrapper。

`SpeechOutcome.state` 是 Completed 或 Interrupted，保留 playback_id、epoch、引擎、callback_submitted_frames、predicted_output_drained 和 reason。`speaker_output_measured` 始终为 false。初版 server 提前标记 speech_started 的记录不能被追溯当作真实首音；后续服务端收据接线见文末。

## 缓冲、速度和停止

输入固定 24 kHz、mono、PCM16LE，每包至多 480 样本（20 ms）。集成 transport 队列 6 包（120 ms），输出环形缓冲至多 480 ms、预缓冲 120 ms。HTTP 单原始 chunk 仍限 256 KiB，但改为每 960 B 解码/发送，不再先展开整个 chunk 的数秒 PCM。除有界原始 Bytes 外，仅一个解码包/不足一包的尾数等待，缓冲上限应分层描述，不能称整条链路总共只缓存 480 ms。

回调 `try_lock`，不会等待 producer 锁；缺数或锁冲突时填静音，记录 underrun / lock miss，不丢未来输入或假造结果。短句正常 EOF 可越过预缓冲门槛；中途断粮等待重新预缓冲。输入音频自带静音保持，不做盲目裁剪。

按输出设备格式转换 F32/I16/U16，把单声道复制到设备声道；线性重采样仅用于匹配至少 24 kHz 的设备采样率，保持原语音时长。slow / fast 直接沿用 SAPI 速率，正常速度才尝试本地 PCM；超过单段 320 字符也回到现有 SAPI 路径。

停止先增加原子 epoch，回调每个输出 frame 检查 epoch / closing / halt；owner 每约 20 ms 泵检查并释放旧输出。Stop 命令携带新 epoch，迟到的旧 Stop 不会清除该 epoch 或更新 epoch 的新 Speak。即使命令队列满，epoch 仍使旧回调停止提交；队列满的控制调用返回错误，而不伪造 owner 已确认的停止。已交给驱动的少量尾音无法据纯代码保证立刻消失，需要真实设备测量。

## 结束与 fallback

HTTP 完成、数据入队、回调提交和预测输出排空是四个不同阶段。正常 PCM 完成要求 HTTP 正常 EOF、packet 与 ring 耗尽，并等待最后提交 frame 的 CPAL 预测播放时间。CPAL 文档明确这是 predicted delivery；其 WASAPI 实现也有估算说明，不是 DAC 或扬声器测量回执。[CPAL 0.16 OutputStreamTimestamp](https://docs.rs/cpal/0.16.0/cpal/struct.OutputStreamTimestamp.html)

输出错误先 halt 并 drop/join stream，再读取已提交帧数，避免“看到 0 后 callback 恰好开始”导致重复播报。只有提交为 0、epoch 仍有效且服务未关闭，才允许 SAPI fallback。任何提交过的 PCM（包括静音）都关闭整段 fallback；中途失败返回 Interrupted。取消不进入 fallback，网络任务及 packet receiver 随旧播放销毁。

本地协议仅接受协商的 audio/pcm 或 octet-stream，并拒绝矛盾的 rate / channels 参数。audio/L16 已移除，因为其规范使用网络字节序，不能按 PCM16LE 解码。[RFC 2586](https://www.rfc-editor.org/rfc/rfc2586)

## 验证范围

新增 10 项无设备测试：4 项 renderer（预缓冲/EOF/预测排空、采样率与声道、取消与静音计数、断粮重缓冲）；4 项 owner 合同（旧 Stop 重排、命令队列的 Speak-before-Stop 重排、部分提交/取消禁 fallback、一次开始回执）；2 项 HTTP/decoder（媒体参数合同、最大原始 chunk 增量解码）。原 client 的分包、满队列取消、速度保护测试保留。

root 提供的 chunk8 预热 12 个合成样例与较早 chunk25 分开记录：chunk8 首 PCM 约 27.6–50.4 ms，后续 640 ms 包约在 135–157 ms 可用；按网络时序模拟，120/240 ms 预缓冲都无下溢。这只支持把 120 ms 作为首批待测值，不证明设备无下溢。声信号启发式起点约 420–680 ms 也不是实听指标。接真实设备后还需测回调提交、首个有效声信号、停止尾音、callback lock miss 和 underrun；当前没有达到扬声器首音/停止时限的证据。
# 根代理补充验证（2026-11-04 02:11）

PCM 尾部取消修补后 `workspace-ninth.log`：457 通过、0 失败、2 忽略；所有原生 bin release 构建成功。`tts-stream-probe` 经真实 Qwen HTTP 服务验证中英 PCM 完整接收和 100ms 取消（取消到 Rust 返回约15.6ms）。探针只消费数据、不调用音频 owner；CPAL 实际设备、首音和停止延迟尚未测。服务端实际播放收据仍在后续接线，不能将接口首 PCM 或单元测试当作可听验收。

02:31更新：服务端收据接线已完成，8项定向模拟测试通过，并纳入 `workspace-eleventh.log` 的484通过/0失败/2忽略。关闭流程首次完整回归发现教学锁等待先于运动取消，根代理已恢复先同步取消运动/音频，再等待收尾；保留 `workspace-tenth.log` 失败记录。新原生release/WASM成功，仍未播放音频设备，逐段自动播报是下一片。

## 服务端收据接线（随后新增，待统一验证）

`speech_delivery.rs` 从 server 主文件抽出播放业务。请求只返回 queued 和 playback_attempt_id / playback_epoch，不再提前写 speech_started。每次接受播放均 reserve 独立音频 epoch，防止旧任务迟到入队换掉新播放；单个 queued reservation 拒绝同一回复重复入队。Started 绑定真实 playback_id，并在 control 短锁内核对 namespace、learner、session、session generation、learner epoch、audio epoch 和 attempt；终态再次核对同一绑定及 playback_id。所有模型、网络、音频等待均不持数据库锁。

停止、切档、设置语速和关闭会先使旧代次失效，释放 reservation 并中止旧等待任务。停止和关闭等待该任务释放前台占用；未收到 Started 时保持原显示状态。已开始才记 interrupted。新播放和旧完成回执交错也不能覆盖同 turn 的新状态。Started 和 Outcome 同一 poll ready 时先取已发送的 Started，避免极速完成漏掉开始事件。

`Store::acknowledge_speech` 仅更新已显示 assistant turn 的播报状态，要求当前会话代次有效；不改 last_delivered_hint、last_delivered_turn_id 或 state_revision。重播旧回复不会改变当前教学问题或使记忆任务无谓失效。原 acknowledge 行为保留，避免影响其他调用。

本批新增 8 项 fake receipt / 临时数据库测试，替换原先把“未开始即取消”视为已打断的 1 项 server 测试，净增 7 项。覆盖排队未开始、同 poll 开始/完成、未开始取消、同 turn 旧回执、owner/epoch/generation 隔离、引擎里程碑与精确 playback_id、重播不回退教学状态、取消释放后台调度前台占用。尚未运行本批 cargo 或设备测试；不将这些合同测试视为实听测量。生产默认仍为 SAPI，未创建 TTS descriptor，未接分句自动播报。

## 外部语音服务准备基础与取消提交边界

分句队列前一片已由 root 验证：全 workspace 517 passed，native 与 WASM 构建完成。随后本地准备基础已纳入 317 项定向测试通过，但该结果发生在本节末尾的安装取消修补之前，不能算作修补验证。`local_speech.rs` 已导出但未挂 main。没有创建生产 `runtime/local-speech-policy.json`。

策略文件最多 4 KiB，缺失即 disabled。只有 `{protocol_version:1, mode:"external", warmup:"synthetic_bilingual"}` 才允许显式调用 `LocalSpeech::prepare_external_tts()`。它读取已有 `runtime/tts-service.json`，以当前 bearer token 检查 health 和配置的模型 ID，随后仅合成固定中英各一句。PCM 通过两包有界队列消费并丢弃，不播放、不保存波形、不使用学习者文本。readiness 与双语 warmup 分开记录，health 成功不能当作预热完成。

前台 `synthesize` 仍为首响应/相邻 chunk 各 15 秒、总 90 秒；新增 `warm_synthetic` 单独允许冷服务等待，`prepare_external_tts` 对 readiness 加两种语言合计施加 120 秒总截止，并以 10 ms 周期处理取消。背景接口没有放宽前台限制。这个区别针对 root 实测 Cosy 首次首 PCM 约 38–71 秒，而不是假设本地服务总能在 15 秒内完成冷启动。

返回的 PreparedTts 包含经验证的客户端和独立预热记录；配置 SHA256、逻辑模型名与固定语料版本随记录保留。当前外部 vLLM 没有可信进程实例回执，因此明确记录 external_instance_verified=false、model_revision_verified=false；warmup 是已完成的合成观察，不能承诺下次延迟或推断服务未重启。prepare 成功后停在 warm_ready_pending_install。

`AudioService::configure_tts(client, expected_epoch)` 仅向 speech owner 发送 ConfigureTts。owner 只有输出空闲且代次匹配才安装/移除客户端；忙碌、旧代次、关闭中以及已经放弃回执的请求不变更当前客户端。此命令不创建 Speak、不补播文本，不影响既有部分 PCM 提交后禁止整段 SAPI fallback 的规则。`new_with_deferred_tts` 供以后主程序显式选择等待暖机安装，旧 `new` 保留 descriptor 启动兼容行为；本切片未切换生产启动路径。Cosy 的 engine/provider 标签保留。

本节新增 10 项无设备测试：6 项策略/模拟 HTTP（缺省不联系服务、显式配置边界、两语言预热但不自动安装、ready/部分成功不能冒认完成、总截止、冷响应取消与并发拒绝），1 项独立头/块超时合同，3 项纯通道安装（实际 API 不发 Speak、忙/旧代次/关闭不改 client、放弃回执不迟到修改）。测试使用临时配置和 loopback fake server；没有启动模型进程或音频 owner。

root 同期新增 `scripts/speech-lab/managed_tts.py` 和 `tts_gpu_lease.sh`。第一轮所有权测试 7 项通过；更新证据 `runtime/goal-baselines/20261104/managed-tts-ownership-second.log` 为 14 项通过，仅使用 sleep 子进程，无真实 GPU。lab 与 managed 启动路径现在共同使用 Linux 每 UID flock，并检查 Qwen/Cosy 两个固定候选端口；这不是跨 WSL 发行版或所有 GPU 应用的互斥。首行 stdin 私有传新 token，stdout 仅生命周期元数据，starting 的 ready/warm 都为 false；EOF/绑定 stop 只清理自有进程组。SIGKILL 后还必须确认该进程组没有非 zombie 活进程，清理失败记录 cleanup_failed，不能报 stopped。该 launcher 尚未由 Rust main 调用，当前 Cosy 实验服务仍是原实例、未被接管。服务进程启动与预热/音频安装保持三个独立事实。

root 已通过独立 `tts-stream-probe --prepare-bilingual` 实测现有 Cosy 服务，证据为 `runtime/goal-baselines/20261104/tts-cosy-prepare-first.log`：中英合计准备约 3.04 秒，中文首 PCM 约 517.7 ms、英文约 594.5 ms；`tts-cosy-prepare-cancel-first.log` 在 900 ms 发起取消，约 0.20 ms 后返回。这些探针只使用唯一临时目录中的策略与 descriptor，已清理私有文件，没有安装客户端或调用音频设备，不能据此宣称实际扬声器延迟达标。前台 15 秒 I/O 限制与后台 120 秒准备截止保持独立。

安装取消修补新增独立准备 token，由 ConfigureTts 一起送至 owner。`cancel_preparation` 与 owner 的“校验 token → 替换客户端 → 标记提交”共用一个短锁；锁不跨异步等待、音频输出或状态通知，先释放提交锁再更新状态。取消先返回时，后续 owner 必须以 preparation_cancelled 拒绝旧客户端；owner 已提交时，取消保留该次安装，记录 active 与 cancellation_after_install。`install_prepared` 等待原有 2 秒有界确认，让实际安装回执优先，不由 10 ms 取消轮询冒认已撤销，也不引入回滚。HTTP 准备阶段仍按原 10 ms 周期取消。

本次增加两个纯通道确定性测试：`cancellation_returning_before_owner_commit_rejects_the_queued_prepared_client` 先暂停 owner、取消返回后再放行，确认旧请求不覆盖原客户端；`owner_commit_before_cancellation_is_kept_even_before_the_installer_polls_its_receipt` 先提交再取消、最后才读取回执，确认状态与实际安装一致。测试复用 fake HTTP 准备结果，不启动真实 speech owner 或设备。修补已完成 rustfmt，等待 root 新一轮统一测试；前述 317 项和真实 prepare-only 探针不覆盖这一新增提交边界。

## 主程序实际挂接（下一切片，等待统一验证）

上述安装取消修补随后已纳入 root 完整 workspace：539 通过、0 失败、3 忽略，全部原生 release 成功；UI 86 项与 WASM 构建无 warning。独立 managed-Cosy 实测也完成：服务就绪约 89.719 秒，双语准备约 7.245 秒（中文冷首 PCM 约 4.828 秒、英文约 639 ms），owner_stop 后确认自有组强制清理、bridge exit 0，总计约 102.203 秒，私有凭据已删除。这仍是 prepare-only，不能当作后续主程序安装或扬声器验证。

本切片把生命周期接到 `main.rs` / `server.rs`，业务放在 `local_speech_runtime.rs`，自有 WSL bridge 放在 `local_speech/managed.rs`。main 取得项目独占锁、绑定 listener 并写成功 server descriptor 后才启动一个后台任务。工作台统一用 deferred TTS 起步，系统 SAPI 保持可用；缺失或 disabled 策略不会通过旧 `tts-service.json` 旁路加载客户端。策略错误记录降级状态，教学服务继续启动。

`runtime/local-speech-policy.json` 的 external 模式仍读取既有 descriptor；managed 模式要求下列显式字段，没有生产文件自动生成：

```json
{"protocol_version":1,"mode":"managed","warmup":"synthetic_bilingual","managed":{"engine":"cosy","distribution":"Ubuntu","references_file":"runtime/tts-references.json"}}
```

Qwen 配置使用 engine=qwen 并省略 references_file。Cosy reference 文件直接使用 `{zh:{wav_path,sha256,text},en:{wav_path,sha256,text}}`，相对 WAV 路径相对该文件目录；继续验证原 WAV 哈希和精确转写，不依赖实验日期目录。managed 每次生成新 token 和 instance，仅在内存构造客户端；不写生产 descriptor 或复用实验密钥。生产固定执行已安装的 `bin/local-speech/managed_tts.py --root <规范化WSL项目根目录>`，不回退源码脚本、拼接 shell 或自动下载依赖。

managed 启动/readiness 有 240 秒截止，先核对本实例 starting 元数据，再通过新 token 验证 HTTP health/model；metadata 不代表 ready。固定双语 warmup 仍共享原 120 秒截止。准备完成后只保留一个待安装客户端；control 短锁内检查手动播放 reservation 与整条自动流。文字 final 或片段队列暂空不能安装；必须 gate 已关闭，或 speech_complete=true 且 worker 退场。捕获 audio epoch 后释放 control，再由音频 owner 条件提交；期间新自动流或手动播放会先更新 epoch，使迟到配置失效。busy/stale 只重试空闲安装，不重新运行模型准备。

`/api/state` 新增 local_speech，区分 ready、warmup_completed、active 和 phase，并提供 busy、retry_available、shutting_down；busy 表示生命周期任务仍占用，包括已安装状态，独立于正在出声。已安装仅表示客户端生效，实际逐段引擎、开始/终态继续以音频回执为准。鉴权后的 `POST /api/audio/local-speech/retry {}` 是明确重试入口；`POST /api/audio/local-speech/stop {}` 停止本地语音生命周期，期间显示 stopping，清理未确认时返回错误而非成功。失败不循环重启，模式和引擎变更需先停止旧任务再显式 retry。普通 playback/stop 仍只停声音，不卸载模型。

停止先失效 audio epoch、取消准备并关闭自动语音流，随后清理 owned model group；external 模式从不停止外部进程。owned bridge 只接受匹配 instance/engine/固定 endpoint 的有界元数据，stderr 持续消费但不保存。组清理最多等 20 秒，超时只结束本次 Windows bridge 并记录 linux_cleanup_unconfirmed，不能冒认 Linux 组已退出。全局 shutdown 的 HTTP 回执仍先确认已接受，owned 清理位于 app.close；CLI 文案同步为 shutdown accepted。实体运动与音频同步失效仍早于 control 锁等待。

本片首批新增 8 项无设备测试：managed 策略边界及 descriptor 无 opt-in 不启用；两项整流安装门禁；默认 runtime 无任务与内存 Qwen 配置不落凭据；两项绑定 managed 元数据/cleanup_failed 合同。随后新增 2 项：实际纯通道卸载遇 stale_epoch 时保留旧客户端并重核代次，成功回执到达后才确认移除；重试门禁排除运行中和清理未确认状态。卸载最多重核 3 次，不运行额外模型调用。首批已随 root workspace-sixteenth 通过，后两项与状态修补待新一轮定向验证。既有 fake HTTP 暖机及取消提交测试继续适用；本实现代理未运行 Cargo、WSL、模型或音频设备，也未创建生产策略。

随后 root 的 local-runtime-second 定向测试与 native-release-eighth 均成功。末轮补充将模式字段固定投影到所有准备状态，并区分运行失败与资源清理：wrapper 在服务自己退出后可用 exit 1 表示运行失败，但匹配的 stopped 回执仍证明自有组清理完成，此时 normal_owner_stop=false、linux_cleanup_confirmed=true，允许显式重试。新增 `service_failure_retains_confirmed_group_cleanup_without_claiming_normal_stop`，本片累计 11 项新增；这项及模式投影待下一次验证。cleanup_failed 或缺少可信终态回执仍不能冒认清理完成。主程序真实生命周期探针由 root 后续执行，不以独立 managed prepare-only 的成功替代。

正常 preflight 失败也须分离：wrapper 仅在没有创建子进程，或 stop_owned 已确认组清理完成时，为 failed 明确附带 linux_cleanup_confirmed=true、process_started 布尔值。Rust 同时要求绑定 instance 与上述显式字段；旧 failed、缺字段或 false 都不能成为清理证明。已证明清理的端口/租约占用失败进入 degraded，可手动重试；cleanup_failed 继续关闭重试。Python 对真实临时租约/端口占用及已启动 sleep 子进程异常的 18 项测试已通过，没有真实 GPU；Rust 新增失败证据与重试门禁 3 项回归，待 root 验证。

WSL 路径转换边界另修复 Windows canonicalize 的扩展盘符路径：使用 `Path::components()` / `Prefix::VerbatimDisk` 将其还原为普通绝对盘符路径后，才作为独立参数传给 wslpath，保留空格与 Unicode。main 及项目内部仍保留原规范路径，不通过字符串宽松去前缀。普通和扩展 UNC 路径在 managed 模式明确拒绝，避免被解释成错误的 Linux 路径；外部模式不受此桥接限制。新增 2 项 Windows 路径测试，覆盖普通/扩展盘符、空格、Unicode、UNC 与设备前缀；本切片累计新增 16 项测试。此次未启动 WSL、模型或设备，未改生产策略。

## Root主程序验收

上述全部实现及立即撤销采集的关闭修补已通过`workspace-twentieth.log`：566通过、0失败、4忽略；原生全部bin release成功，UI91项和WASM构建通过。

`scripts/validation/local_voice_lifecycle_smoke.py`以独立目录、固定模型文件硬链接和7个实际部署资产运行真实主程序。`main-managed-cosy-first/report.json`成功：新缓存约136.2秒到warming，预热67.081秒，203.292秒安装Cosy客户端；中文冷首PCM64.115秒、英文615毫秒。只消费固定合成PCM，不请求采音或播音。

已安装时的重复retry被拒绝；显式停用5.193秒，先确认客户端移除，再由wrapper确认自有进程组已空。需强制组终止，不能称模型优雅退出；这一时间也不是按键到静音。随后新nonce/token实例在starting约1秒时取消，TERM清理完成、无客户端安装。主程序正常exit0、server凭据descriptor和fixture策略删除，GPU回到1752MiB。冻结软件包保存在`runtime/goal-releases/20261104-seventh`，尚未发布到当前安装。外部模式和实际扬声器/麦克风仍须各自的端到端验收，不能由本次准备成功代替。

另行实际执行`local_voice_preflight_smoke.py`，证据为`main-managed-preflight-first/report.json`：自有Ubuntu占位监听使主程序约735毫秒正确进入degraded，failed明确process_started=false且清理已确认，允许显式重试；监听的accepted_connections为0，并一直存活到测试主动释放。释放后新实例重试，1秒后仍处starting即Stop，约78毫秒完成自有TERM清理。未进入ready/warming、未安装客户端或请求设备；主程序exit0，凭据和fixture策略删除。该启动中取消时间不是实际停声时延。

