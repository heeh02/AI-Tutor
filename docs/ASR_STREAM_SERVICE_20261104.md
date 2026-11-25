# 本地流式识别服务与实际链路基线

状态：开发验证中，未发布为默认语音配置。Rust 主程序拥有录音、采样率转换、取消代次和完整音频回退；Python 只持有当前识别器状态，不访问麦克风、扬声器或学习数据库。

## 托管协议固定 WAV 实测（第十检查点）

`runtime/goal-baselines/20261104/managed-asr-wav-first/{method,report,summary}.json` 记录一次完整库内集成诊断。新建 `OwnedAsrService` 经私有握手加载固定真实 Zipformer，使用 managed `AsrClient` 和实际 `CaptureStream.pump`；原12段成人 SAPI 合成音频先由16k转44.1k，再按20ms实时节奏经过流式重采样送到16k服务。不是设备采音，也不是主程序 HTTP 录音端点测试；真实主程序生命周期另见第九检查点。

| 12段固定样本、nearest-rank | P50 | P95 | 冻结参考目标 |
| --- | ---: | ---: | --- |
| 首次非空字幕被观察到 | 822.2ms | 842.1ms | ≤700ms，未达 |
| 输入结束到最终结果 | 25.9ms | 40.9ms | ≤500ms，达到本次离线链路目标 |

字幕计时包含原音频前导静音，观察分辨率为20ms；收尾计时不含界面或扬声器。每语言6段包含该语言主导的混语样本；中文/英文字幕P95分别842.1/836.0ms，收尾P95为27.4/40.9ms。样本量小，不推断真人、儿童、口音、噪声或联合负载下的总体P95。

12段均产生非空终稿、未触发回退、完整fallback PCM逐项保持不变，但**这不是准确率通过**：英文 `sensor` 被识别为 `CENSOR`；中文主导混语样本只返回“我想比较”，英文主导混语样本缺失“电机”。SAPI跨语发音本身也是混杂因素，仍需独立真人语料。保留全部原文、字幕修订和终稿，不能因非空或收尾快就启用自动发送。

额外一轮先确认流仍健康，再于1秒处撤销录音代次：旧worker退出与cancel请求收尾约1.17ms，新owner哨兵未被迟到状态改写；随后**同一个服务的新识别流**成功完成首个样本。这不证明模型进程重启、新旧真实capture重叠或按键到物理静音时限。服务准备到warm_ready约1.189秒（起点为本次spawn之前，非冷启动比较），最终launcher/解释器退出且两个Job清空，forced=false；原安装、外部descriptor和生产策略hash不变。

证据：`managed-asr-wav-first.log` 为修订后的ignored诊断实际1 passed，执行77.17秒；未运行Whisper回退推理。诊断在运行前冻结方法/音频/程序/模型hash；保留失败样本的阶段、已见字幕和最后状态，未运行行明确not_run，成功spawn后的错误路径仍等待所属服务清理。生产默认配置未变。

## 模块和协议

- `scripts/speech-service/recognizer.py`：固定 bilingual Zipformer int8 模型和 sherpa-onnx 2.0.1，CPU 两线程，启动时加载并用静音预热。
- `scripts/speech-service/asr_service.py`：单活跃录音，鉴权的 127.0.0.1 HTTP 服务；启动就绪后发布 `runtime/speech-service.json`。日志不打印凭证、音频或识别文本。
- Rust `audio/streaming_asr.rs`：独立推理客户端和分块队列；原 WASAPI owner 继续拥有采音。客户端不会把推理地址解析到其他主机，也不会跟随重定向。
- UI `live_transcript.rs`：独立读取当前录音字幕，未发送临时文字不改写编辑框。最终结果继续使用原来的编辑、确认发送流程。

协议包含 start、连续编号 PCM16LE chunk、final、cancel。每次请求绑定 Rust 生成的 UUID 和 recording_generation。输入为 16kHz 单声道，每块最多 100ms，单次最多 20 秒。服务侧闲置 30 秒释放；final 结束即释放活动会话。旧会话的 cancel 不能清掉新会话，序号不连续不能假装完整成功。部分文本变化时才增加 revision。服务端只保留一个活动解码器，无历史录音落盘。

服务使用单线程推理；Rust 本地取消与麦克风释放不依赖远端推理退出。遇到模型调用卡住时，远端释放仍需该调用返回，不能把客户端取消检查间隔写成远端确认时限。音频队列溢出或服务出错后保留完整的有界 RAM 音频，显式转向原 Whisper 转写。

开发启动方式（在项目根目录）：

```powershell
runtime/speech-lab/.venv/Scripts/python.exe scripts/speech-service/asr_service.py
```

原开发入口沿用 speech-lab 的固定依赖环境。托管接线见下一节，默认仍关闭；实际主程序托管模型生命周期在第九检查点验证，第十检查点补充上述managed协议固定WAV链路。真实设备采音验收仍待完成。

## 显式托管生命周期接线（2026-11-04 后续切片）

新增托管入口 `managed_asr.py`，安装到 `bin/local-asr` 后复用同目录的 `asr_service.py`、`recognizer.py`。原手动服务入口保持兼容，现有 18867 实验实例与 `runtime/speech-service.json` 未被接管或修改。安装器只发布这三个程序文件，不复制模型、环境、配置或凭证。

主程序在项目独占锁、HTTP listener 和主 descriptor 建立之后后台准备 ASR。缺少 `runtime/local-asr-policy.json` 或 mode=disabled 时不启动子进程，也不跟随公共识别 descriptor。启用格式为 `{"protocol_version":1,"mode":"managed","python_executable":"已安装环境中 python.exe 的绝对路径"}`；已存在策略必须包含版本 1，缺少或未知版本拒绝。此处仅记录合同，没有创建生产策略。模型固定为现有 Zipformer 目录，CPU 两线程，不下载依赖、不打开设备。

Rust `local_asr.rs` 与 `local_asr/owned.rs` 保存自有 Child、Windows Job 及实际解释器的进程句柄。实际解释器 Job 归属确认后才通过 stdin 发送实例编号和临时凭证，Python 此前不得加载模型或绑定 HTTP。服务使用动态 `127.0.0.1:0`，从不写 `speech-service.json`。实际解释器 PID 只从自有私密管道取得，用一次 OpenProcess 获得归属查询、Job 分配与退出等待所需句柄；终止只作用于自有 Child/Job，不按 PID 查找或终止进程。stdout 只发有界生命周期元数据，凭证不进入 argv、日志或状态。

Windows venv redirector 可能在 Rust 将 launcher 加入 Job 前已创建实际解释器，既有子代不能假定自动补入。此解释器还可能已属于 CPython 自己的 Job：先对同一个进程句柄调用 IsProcessInJob；已属主 Job 时复用，否则加入新建的空 Job 并再次验证，然后才放行加载。后者避免向已含 launcher 的非空 Job 强行并入不兼容的 Job 层级。stop/Drop 回收两个精确 owned Job，cleanup_confirmed 同时要求 launcher/实际解释器退出及相关 Job 无活动进程；收据另有 launcher_job_empty、interpreter_job_empty、interpreter_job_owned。依据为 [AssignProcessToJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject)、[IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob) 和 [CPython 3.10.11 launcher 的 run_child](https://raw.githubusercontent.com/python/cpython/v3.10.11/PC/launcher.c)。

状态 `state.local_asr` 独立呈现 ready、warmup_completed、active：HTTP 模型已加载、静音预热已完成、客户端已安装给后续录音。active 不表示麦克风开启。阶段为 disabled/configuration_error/starting/loading/warming/active/stopping/stopped/degraded/cleanup_failed。`POST /api/audio/local-asr/retry` 是显式重试；stop 只停用流式增强，保留正在录制的完整 PCM 走既有 Whisper 回退。清理未确认时禁止重试。加载总预算 60 秒，预热 30 秒；owned 停止约 8.25 秒上限，runtime 收尾等待 10 秒，不通过加大截止掩盖失败。

安装采用同一短 lifecycle control 锁中的 epoch 校验、client 槽替换和 active 标记；取消同锁失效并按实例清槽。锁序是 lifecycle control → ASR client slot，采音只读取 slot 快照，不反向获取 lifecycle/教学锁。每段录音使用不可变连接快照，旧清理不能卸载新实例。后台不在录音中发送 health 探测；仅真实连接失败、服务超时、5xx 或实例/响应协议错误标记服务不可用。用户取消、本地队列溢出、正常 4xx 冲突只让本轮回退。应用关闭仍首先同步丢弃采音，再与 TTS 并行收尾自有模型，避免预算顺序叠加。

已运行 `test_service` + `test_managed_asr`：13 项通过（4 旧协议测试、9 新托管测试），证据 `runtime/goal-baselines/20261104/managed-asr-python-second.log`。使用假引擎及自有假子进程，覆盖握手之前无加载/端口、ready 与 warm、旧实例/凭证、EOF、预热中取消、加载失败和外部 descriptor 保留。没有加载 sherpa/NumPy 模型或使用设备。

新增 13 项 Rust 回归包含缺策略禁旁路、版本与绝对解释器路径、回环/实例限制、有界回执、取消先于安装、录音快照与旧清理、仅停止增强不失效录音、清理失败禁重试、Stop 请求取消后由 worker 归还空闲，以及 managed 409/503/实例错误/用户取消区分。根代理已运行 ASR 定向 10+3 项通过、workspace 第 23 轮 615 通过/0 失败/4 忽略，native 第 13 轮及 WASM-ASR 构建通过。此证据发生在实际解释器 Job 归属修补之前；该修补另新增 2 个不加载模型的 Windows 子进程回归，尚待根侧重新测试。尚无本片真实 owned ASR 模型生命周期或麦克风验收。

独立诊断 `scripts/validation/local_asr_lifecycle_smoke.py` 已提供显式 `--real-local-synthetic` 入口，必须指定待测 exe、Python 环境、UI 和一个不存在的输出目录。它只在新 fixture 内安装三个服务源码与四个固定模型硬链接，隔离 provider HOME/PATH，TTS 关闭；仅调用主程序的 state、local-asr retry/stop 与 shutdown。报告分开保存 ready/warm/active、重复重试拒绝、两个不同实例的退出与 Job 清空证据、客户端卸载、加载中取消、主程序正常关闭及 descriptor 删除。生产 ASR/TTS descriptor 与策略仅取前后 hash，不读取凭证、不联系 18867 实验实例。输出与失败记录用独占新文件，清理仅针对本次写入且内容仍匹配的策略/自有主 descriptor，保留模型硬链接和 fixture 目录。该脚本当前只完成静态检查，实际调用由根代理安排；静音预热生命周期通过也不能替代新版 managed nonce 协议的真实 WAV 转写或设备验收。

### Windows stdin 与 NumPy 导入阻塞（实际失败后修补）

根代理已实际运行 `main-managed-asr-first/second/third` 独立 fixture，均在原 60 秒加载期限失败，没有放大期限。第二轮插桩显示 NumPy 导入等待约 59.935 秒，到 Stop 使 stdin reader 返回时才继续；随后 sherpa 导入约 10.5ms、识别器构造约 897.8ms。第三轮五秒 Python 堆栈显示主线程停在 `_multiarray_umath` 的 extension `create_module`，owner reader 在 `managed_asr.py` 的阻塞 `readline`，HTTP 线程正常等待 select。第二轮已保存真实停止收据：解释器与 launcher 已退出、两 Job 清空、cleanup_confirmed=true，forced=true；第一轮未收 ASR 终态收据，不能由主程序 exit0 推定该证据。原失败报告不回填或覆盖。

这些本机堆栈和 Stop 后导入恢复的时序，与 [NumPy #24290](https://github.com/numpy/numpy/issues/24290) 的 Windows stdin 管道问题及 [OpenBLAS #5109](https://github.com/OpenMathLib/OpenBLAS/issues/5109) 的 DLL 初始化/CRT 文件描述符锁机制一致；尚未取得本机原生堆栈，底层锁机制仍是有依据的推断。

窄修保留现启动顺序和立即监听 Stop/EOF：`managed_asr.py` 单一 `OwnerLines` consumer 在 Windows 真实管道上先 PeekNamedPipe，只对已可读字节执行有界 os.read，空时等 10ms。设置 O_BINARY 避免 CRT 对末尾 CR 的翻译额外等待；同一 fd 不再混用 TextIO.readline。最多积累 MAX_LINE+1 字节，完整行、半行、EOF、旧实例与坏协议仍按原规则处理；StringIO/假源保持兼容。微软说明同步管道的 PeekNamedPipe 本身也可能受并发阻塞 I/O 影响，因此关键是消除本 reader 长时间持有的阻塞读，而不是宣称 PeekNamedPipe 在所有情形绝不会阻塞。[官方说明](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe)

修补后的固定 venv 测试 15 项通过（原 13 + Windows 真实管道 2），证据 `runtime/goal-baselines/20261104/managed-asr-stdin-venv-first.log`。新增测试在 bootstrap 后让管道保持打开，确认 reader 正等待，再由另一个线程执行 os.fstat(0)，一秒内完成；覆盖 Stop/EOF、拆分帧、连续旧/新实例命令、超长及未终止帧。测试只使用假子进程和 stdlib，没有 NumPy、真实模型或设备。修补后的真实主程序准备与转写仍需另跑，不能用这 15 项替代。

第四轮真实主程序已通过，证据 `runtime/goal-baselines/20261104/main-managed-asr-fourth/report.json`：同一 60 秒上限内 NumPy 导入约 71ms、识别器构造约 916ms，主程序从启动到 active 1436.828ms；Stop 132.588ms，两次不同实例（正常停用与新实例 loading 中取消）均有解释器退出、两 Job 清空及 cleanup_confirmed=true。没有开启设备，未发送用户音频，仍仅证明准备/安装/生命周期；原 18867 descriptor 前后 hash 相同。warming 太短未被轮询捕捉，但 active 前的私有 warming/warm_ready 顺序校验已完成；不将观察不到该阶段写成单独预热延迟。

第四轮两次收据 forced=true 暴露停止判定的时间边界：launcher 退出后，旧实现立即检查解释器/Job，瞬时未清空就强制回收，尚未用完五秒自然退出窗口。后续窄修将 launcher wait、解释器退出与两 Job 清空共用同一五秒 deadline，期间短轮询；到期仍未完成才 force，保持原约 8.25 秒总预算。新增 `stop_waits_remaining_grace_after_launcher_exits_without_forcing` 和 `stop_still_forces_stuck_interpreter_after_shared_grace_deadline` 两个无模型自进程回归，前者用真实 launcher 退出句柄作屏障再让解释器延迟自然退出，后者验证卡住解释器仍被精确回收。本修补目前已格式化，测试和第五轮无插桩主程序验收待根代理运行；前三次失败、第四次成功均保留原报告。

## 第九检查点实际验收

停止宽限期修补后，ASR定向14项Rust通过；`workspace-twenty-fifth.log`为619通过、0失败、4忽略，含109项UI测试。`native-release-fifteenth.log`构建全部原生bin成功；前端沿用已通过的`wasm-local-asr-first.log`产物。服务管道修补的固定venv 15项测试与安装资产fixture均通过。

`runtime/goal-baselines/20261104/main-managed-asr-fifth/report.json`使用最终主程序SHA256 `14529e9a6a5353d55fcb310e7cf15320869148c2d564d613e0c2a988dd1a7976`及未经插桩的三个服务源码，实际通过加载、预热、安装、重复Retry拒绝、Stop、新实例Retry及loading阶段取消。诊断开始至active为1689.858ms，正常停用为154.917ms；这包含测试目录准备及此前读取模型文件，不是冷缓存或总体P95基准。warming在本轮被轮询捕获，active时AudioService确认可用于后续录音。

两次停止收据均为解释器退出、launcher/解释器Job空、cleanup_confirmed=true、forced=false；之后没有已安装ASR客户端。主程序正常exit0并删除自身descriptor，测试启用策略移除，生产descriptor/策略前后哈希一致。全程没有设备、用户音频或教学模型请求；仅使用识别器自有的静音预热。此结果证明实际托管生命周期，不能替代新版managed nonce协议的固定WAV转写、真实采音、字幕准确率或按键至静音指标。前三次失败和第四次插桩成功保持原样。

## 实际测量

12 条已有作者编写教育句子的 SAPI 合成 WAV，以真实时间节奏、100ms 分块，经实际 HTTP 服务运行。无麦克风、扬声器或浏览器操作。证据：`runtime/goal-baselines/20261104/asr-http-first/report.json`。

| 测量 | 结果 |
| --- | --- |
| 加载与预热 | 1891ms，本次一次启动 |
| 第一个非空临时识别结果 | 523.5–839.5ms，12 条小样本 |
| 发送 final 到收到最终文本 | 25.2–43.6ms |
| 单个 chunk 请求最大耗时 | 每句约 29–33ms |

首次部分识别包含输入音频等待和 HTTP 往返，不包含采音、Rust 泵送和界面刷新时间。final 时间不等于用户松开按钮到可编辑终稿的端到端时延。本轮后台还有依赖/模型下载，不作为干净联合负载基准。700ms 字幕目标尚未通过；需要继续评估分块、模型右上下文、界面呈现及其他 ASR 候选。

合成成人声音不能证明儿童、口音、噪声或自然交谈准确率；原语料中的中英混读本身受 SAPI 发音影响。未将模型转写当作已确认学习事实。

## 已验证与待验证

- 服务侧 4 项测试通过：真实 loopback HTTP 鉴权/顺序/结束、旧取消与旧结果隔离、文本修订、闲置和输入预算释放。日志 `asr-service-protocol-first.log`。
- 12 条真实模型 HTTP 测试完整返回，原始作者语料和结果仅存测试证据目录。
- 当前 Rust 生命周期/分块/回退/取消、UI 回归及 WASM 构建已通过第九检查点所列软件检查；真实模型生命周期与旧实验服务的WAV转写是分别取得的证据。
- 实际设备录音、真实声音、界面显示、系统安装与端到端停止验收待做。
