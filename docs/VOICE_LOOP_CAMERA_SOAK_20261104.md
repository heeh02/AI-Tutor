# 同 Workstation 相机与连续语音诊断

本切片实现显式、默认忽略的 `server::voice_loop_soak_diagnostic::real_synthetic_voice_loop_camera_soak`。首轮失败保留；第二轮在新政策、诊断和冻结源码下完成 30 分钟联合运行，详见文末，不能倒推首轮失败原因。根代理统一编译、选择相机、持有 Qwen 服务并控制运行；普通程序没有新增合成麦克风或诊断 HTTP 接口。

固定中文输入沿已验证的 `round` 执行：20 ms 节奏固定成人合成 WAV → 真实 capture owner / Sonora → 自有 Zipformer → 原始最终转写 → 真实 Kimi → 模拟显示 ACK → 真实 Qwen / CPAL 完成回执 → 生产 cooldown / rearm。每轮立即继续下一轮，不直接赋循环状态、不伪造音频完成、不更正转写、不失败重试。共享逐轮函数增加消费者观察时间点，并以实际 SQL 计数检查所有用户 turn，避免 `session.turns` 仅有 24 条显示页而使长运行失效。

相机是同一个 Workstation 的 `CameraService`。必须提供精确 ID 和 SHA 固定的原生 worker 可执行文件；`open(Some(id))` 仍验证当前枚举成员，没有默认索引或自动选择。相机权重、ORT DLL 和 providers DLL 按已有固定 SHA 复制到新 fixture。只每秒保存元数据白名单：帧身份、帧龄、尺寸、FPS、处理阶段时间，以及父进程确实接受的处理帧计数。没有保存 JPEG、脸框、tracks、native pose 或原始错误详情；不能据此声称浏览器呈现或真人跟随通过。

计时从 ASR/TTS 准备完成、相机已有新鲜处理帧、生产 voice loop 已 armed 后开始。固定 1,800 秒内立即连续工作；到期不再开启新轮，在途整轮统一最多再收尾 180 秒，覆盖录音、最终转写、教学、播放和 rearm 全过程。教学请求硬预算 96 次；若先用完预算而不足 1,800 秒，诊断失败并保留证据，不空转补时。任一原文不完整、模型或播放失败、相机失效/epoch 变化/重启、心跳撤销，都停止本轮，不自动重试。相机新鲜度检查采用已有 2 秒界限，不增设未经实测的 FPS 性能通过阈值。

六个 5 分钟窗口分别记录覆盖时长、开始/完成轮次、ASR finals、教学提交、实际接收处理帧增量和阶段时间。跨窗口轮次按观察区间裁切；完成次数只记实际完成的窗口。`sampled_activity_counts` 是每秒的状态观察数，不是精确占空比；`phase_elapsed_ms` 是 capture/finalize/submit_wait/teaching_playback/rearm 的消费者观察墙钟区间，不是纯模型计算时间。每个完整窗口都必须有真实完成轮次和处理帧增长。RSS/CPU/GPU 趋势由外层 runner 单独采样，内层不重复收集。

观察覆盖另固定最大间隔 3,000 ms，适用于相邻元数据观察，以及最后一次 active 观察到 operation 结束的尾部。超过即失败，后续恢复不能抹掉中间缺口；停止信号竞态后产生的样本不能补足 active 尾部。报告分别保留最大 gap、tail gap 和覆盖判定。这是 1 Hz 观察覆盖界限，允许调度抖动，不是 200 ms 响应性能达标线。同步写入 `progress.json` 后再检查期限，不能因文件 I/O 跨过期限而启动新轮。

后台记忆整理首版不运行，报告固定 `memory_background="not_exercised"`。正常生产候选提交仍作用于隔离 simulator DB，不能宣称所有候选都停留在 shadow。固定成人合成语料不证明儿童、自发语音、长思考停顿或声学轮换；物理麦克风和 PTZ 始终关闭，系统音量不改。

清理先发生产 shutdown 的输入/播报/动作失效，再等待观察任务与模型收尾。相机仅测试构建增加精确清理证明：保留真实 `Child::wait` 成功次数、仍持有的 Job 空进程证明和 supervisor 退出标记；不会根据 UI 的 `closed` 状态或 PID 推断已回收。`cleanup_confirmed` 要求已启动 child 数与确认 wait 数一致、已归属 Job 数与空 Job 数一致，并且 supervisor 已退出。自有 ASR 沿既有双句柄/Job 回执；Qwen 服务由外层 runner finally 停止。旧 18867 实验实例和生产描述文件不接管、不改写。

运行合同：

- `AI_TUTOR_REAL_VOICE_LOOP_SOAK=1`
- `AI_TUTOR_VOICE_CHAIN_RUN`：新的小写字母/数字/连字符运行名称，最多 80 字符。
- `AI_TUTOR_VOICE_CHAIN_TTS_ENGINE=qwen`；`AI_TUTOR_VOICE_CHAIN_TTS_TOKEN` 仅私有环境传递。
- `AI_TUTOR_VOICE_CHAIN_LANGUAGE` 如提供，必须为 `zh`。
- `AI_TUTOR_SOAK_CAMERA_ID`：根代理明确核定的完整 ID，不记录原文。
- `AI_TUTOR_SOAK_CAMERA_WORKER`：绝对路径；`AI_TUTOR_SOAK_CAMERA_WORKER_SHA256`：精确字节 SHA。

产物位于新 `runtime/goal-baselines/20261104/<run>/`：`method.json` 固定输入、资产和代码哈希；`round-001.json` 等保存逐轮真实结果；`camera-samples.jsonl` 每秒 flush；`progress.json` 每轮开始/结束刷新；`report.json` 汇总时长、预算、窗口、所有权回执和保护文件前后哈希。`active_duration_ms` 包含最后一轮可能的收尾时间，六个统计窗口只覆盖前 1,800 秒。不能把准备阶段或清理耗时凑入窗口。

新增验证包括元数据隐私投影、跨窗口统计、超过显示分页的真实用户 turn 检查、清理证明必要条件、精确 idle child / Job 回收，以及观察覆盖边界/中间长 gap/尾 gap，共 6 项常规回归；另 1 项忽略的真实 soak。根代理 workspace41 首编译暴露 `Path` 被 Axum 同名类型遮蔽，已显式导入标准路径类型；workspace42 通过后补观察覆盖与期限复核，workspace43 完整结果为 **786 通过、0 失败、9 忽略**，其中 UI 128 项，无 warning。此前失败日志、checkpoint17 四轮证据均保留，不改写。

## 第二轮实测：限定负载下完成 30 分钟

产物为 `voice-loop-camera-soak-second/` 与 `voice-loop-soak-04a694888c944ba494cc7447755b61a8-zh/`，都在 `runtime/goal-baselines/20261104/` 下。使用 checkpoint20 的 295 项测前/测后相同源码和已归档二进制；测试 exe SHA `a7a617210a20c2be54f50e2c1a19c6de2ea33a7fba369e1e8c6c334256dfa749`、camera worker SHA `5b3367a5b81a33fd9b103f2f8efe483d09ce9ec8587435733de1c5bfdb3a3166`、教学 policy SHA `ec7d6dd187d3a2817fd870afc15de39abb9d66a13f2dc74d60000c4b2ceb9758`。新增诊断 revision 2，仍使用原 1,800 秒/96 请求/180 秒在途收尾合同，没有放宽通过指标。每轮 provider 诊断仅绑定该轮精确 request/generation，另保存清理前后快照，不覆盖已写轮末状态。

**实际 active 1,809,236.4308 ms（30 分 9.236 秒），49/49 轮完整，120 段原生播放完成回执，子进程及外层退出 0。** 六个 5 分钟窗口各覆盖 300,000 ms，完成数为 `[9,7,8,8,8,8]`，合计 48；第 49 轮在期限前启动、在允许的在途收尾期间完成，不能重复计入六窗。相机接收处理帧观察增量分别为 `[7392,7671,7668,7584,7475,7581]`。无自动重试、无闲置补时，49 次真实 Zipformer final 都保持固定原文，49 次 Kimi ACP 请求均完成且无错误/取消/重试。49 次输入源打开、关闭和 producer join 均确认。

根代理使用 `audit_soak20_root.py` 从原始 round、camera JSONL、资源 JSONL 和只读模拟数据库复算，结果为 `voice-loop-camera-soak-second/root-final-review.json`。此前 UI 代理已逐批检查前 40 轮，但未形成完整最终审阅文件；后台代理的最终定量审阅遇到用量限制未完成。因此最终汇总明确是**方法知情的根代理复核，不称为完整独立审计或盲评**。根代理另读完全部 49 轮/120 段文本，没有发现无条件抓车、碰轮、拉线或冒充远程车控的建议；各段停止建议有保持距离及可达控制的条件，后期“已经停下了吗”是问句。逐轮状态仍是 goal_setting、模块未知且需确认、未填完成结果/实验记录。最终数据库有 98 条 turn、49 个 provider run，memories、education_memory_candidates、memory_jobs 均为 0。原始候选数组未保留，不能把落库边界正确说成所有原候选均正确。

| 消费者观察指标（49 样本） | P50 | 最近秩 P95 | 最大值 |
|---|---:|---:|---:|
| 整轮至 rearm | 37.441 s | 41.362 s | 42.369 s |
| 端点关闭至 ASR final | 33.08 ms | 36.67 ms | 37.85 ms |
| 提交至首 callback 回执 | 11.151 s | 14.139 s | 15.024 s |
| 提交至 text final | 13.396 s | 17.127 s | 18.866 s |
| 提交至完整播报完成 | 32.143 s | 36.090 s | 37.060 s |
| 播报完成至 rearm | 531.32 ms | 533.49 ms | 534.08 ms |

49 轮的首 callback 回执都先于 text final，证明本次分段链路确有重叠；回执不是扬声器实际首音。教学首句仍明显未达整体 2.5/5 秒目标，不能用 ASR 尾延迟代替整体延迟。固定输入重复、文本长度和远程模型采样不同，不将与首轮的时间差解释为严格因果对照。

相机 720p，1,811 条元数据观察，epoch 变化 0，最大观察 gap 1,013.5904 ms、active 尾 gap 230.4358 ms；没有 active 结束后的样本补足覆盖。初次接收计数 2、末次 active 45,640，期间增量 45,638；清理回执 45,646 的额外 6 帧不计入 active。末次报告处理 FPS 25.223，启动样本 FPS 4.510，不能声称每一时刻均达 24–30 FPS。无浏览器预览负载及 UI 呈现计时，不证明目标跟随、遮挡或采集到显示延迟。

全子进程生命周期的 359 次 Windows 树采样 RSS 最大值为 895,922,176 B（约 854.42 MiB），不含 WSL Qwen 主机内存；60 次 NVIDIA 聚合采样显存最大值 7,773 MiB / 12,227 MiB（约 63.57%）。无采样错误，均为离散采样最大值，不是独占归因或连续真实峰值。约 active 22–23 分钟根代理的一次文件搜索范围意外扩大后终止，产生额外文件系统/CPU 读负载；该区间完整保留，详见 observer-notes-root.json，不标为隔离空闲主机基准。

ASR 正常非强制停止，解释器/launcher 及两层 Job 清理确认；相机仅一个 child，真实 wait 和 Job 空各一次，supervisor 退出。Kimi 清理确认。外层 Qwen owner stop 的 service/bridge 均退出 0、非强制组停止且 Linux 清理确认。所有保护文件前后 SHA 相同，已安装 exe 未替换。

本次仍只覆盖固定成人中文合成输入、实际相机、Zipformer、Kimi、Qwen/CPAL；显示 ACK 和心跳模拟，无物理麦克风、声学测量、浏览器、PTZ 或后台记忆。Qwen 由外层诊断持有，不是主程序统一托管的全部服务生命周期。后续同主程序托管与 shadow 记忆联合负载见 `MAIN_MANAGED_VOICE_MEMORY_INTEGRATION_PLAN_20261104.md`，尚未实施。首轮失败和全部旧结果保留，第二轮成功不能证明首轮第 13 次失败的原因已被修复。

## 首轮实测：失败保留，没有重试

外层产物：`runtime/goal-baselines/20261104/voice-loop-camera-soak-first/`。子产物：`voice-loop-soak-e2dacb8fe8e346fdac6007432ff94dfc-zh/`。测前源码与精确二进制归档于 `checkpoint18-measured-sources/`、`checkpoint18-measured-binaries/`；测试 exe SHA 为 `9145e4287c85e1ef98b6a678e7919c11893c6f095c39c00b78c43261e4f50458`，原生 camera worker SHA 为 `eabc0f9cbd655e81e576a8d5c679a747c7477dd78095b7c22fbabe3f325964c1`，policy SHA 为 `3d285fdf797183c9c0e718e4405007777c2a8c7f7953c6efab715547a847035b`。

active 实际 **323,588.3053 ms（约 5 分 23.6 秒）**。12 轮教学和音频完成并 rearm，共 22 个真实原生播放完成回执；11 轮观察到首 callback 回执先于文本 final。第 13 次采集、端点和 ASR 均正常，原文仍为“先让小车停下来”，但教学失败，未出现任何片段。13 次输入源均已停止并 join。子进程退出 101，外层退出 1；没有自动重试，预算不是退出原因。这不能作为 30 分钟稳定性通过。

第 13 轮的持久记录为 `provider_runs.status=failed`、约 778 ms，stream `provider_or_delivery_failed`。ACP session setup 578.6461 ms 后，仍为 0 thought chunk、0 message chunk、0 segment，final 未验证，contract failure code 为空，整个 provider 尝试 764.4592 ms。可定位到写入 `session/prompt` 或等待首响应阶段；保留产物没有具体原始 ACP 错误，不能断言限流、凭据、上下文或模型 JSONL 错误。它不是 24 条显示分页断言：全量 SQL 检查位于成功播放/rearm 之后，第 13 轮根本没有走到该处；最终数据库有 13 个不同 request，各恰有一条用户 turn。第 13 条 assistant 内容为空，未伪造显示或完成。

下表从前 12 个成功 round 的观察时间点重新计算。首 callback、text final 和播放完成均从 `submit_begin` 起算；这些是消费者观察延迟，不能称为扬声器实际首音或纯模型计算耗时。N=12 时最近秩 p95 等于样本最大值，不能外推总体性能界限。

| 指标 | 中位数 | 样本最小—最大 |
|---|---:|---:|
| 整轮至 rearm | 26.207 s | 22.579—30.743 s |
| 合成输入源实际关闭观察 | 4.703 s | 4.689—4.715 s |
| 观察到端点关闭至 ASR final | 31.60 ms | 27.42—36.45 ms |
| 提交至首 callback 回执观察 | 9.699 s | 7.145—13.274 s |
| 提交至 text final 观察 | 11.544 s | 8.425—15.343 s |
| 提交至完整播报完成观察 | 20.920 s | 17.259—25.452 s |
| 播报完成后的 rearm 等待观察 | 529.66 ms | 505.67—531.89 ms |

相机留下 325 条元数据观察，epoch 未变；最大相邻观察间隔 1,011.4264 ms，active 尾 gap 576.5348 ms，均在固定 3,000 ms 覆盖界限内。首次计数 2、末次观察计数 8,188，关闭回执计数 8,200：末次观察和关闭之间的处理结果不能混算为同一采样时点。第一个 5 分钟窗口覆盖 300,000 ms，实际完成 11 轮、接收处理帧观察增量 7,586；第二窗口只覆盖 23,588.3053 ms，完成 1 轮、帧增量 600；后四窗没有覆盖。观察连续性局部成立，不代表整段 30 分钟通过，也不证明浏览器呈现或人物跟随。

资源数据覆盖整个子进程生命周期，包括准备和清理。66 次约 5 秒 Windows 自有进程树采样中，RSS **采样最大值**为 863,166,464 B（823.18 MiB），不包含 WSL Qwen 的主机内存。11 次约 30 秒 NVIDIA 聚合采样中，显存使用**采样最大值**为 7,770 MiB，总量 12,227 MiB，观察比例约 63.55%。没有采样错误；这些离散观察不能证明连续时间的真实峰值，也不能全部归因于本模型，更不是长期 12 GB 容量保证。

清理方面：ASR 为非强制退出，launcher 和实际解释器退出、两个 Job 空均有确认；相机只有一个已启动 child，真实 wait 成功 1 次、Job 空 1 次、supervisor 已退出；Kimi provider 已回收。外层 Qwen 由 owner stop 正常结束，非强制组停止，service/bridge 均退出 0，Linux 清理确认。保护文件前后 SHA 一致。清理成功没有改变本次负载失败的结论。

独立语义审阅另存 `voice-loop-camera-soak-first/independent-semantic-review-backend-complete.json`，保留早期 partial 文件。前 12 轮和最终只读数据库的有效任务状态均保持 goal_setting、reported_result 为空、modules 未知且仍需确认；没有实验完成或掌握声明。持久 memories、education_memory_candidates、memory_jobs 表均为 0 行。正常生产 apply 确实运行过，因此这只能证明已落库边界没有所查污染，不能证明每个未保留的原始候选都正确。全部完成回复反复建议“用手拦住小车”，而尺寸、速度、硬件仍未确认，已单列为需复核的教学表达问题；不将链路完成等同教学质量通过。第 13 轮无教学输出，不按空回复计为语义成功。
