# 真实生产分段播报诊断

本片增加独立 Windows ignored 库内诊断。2026-11-04 已由根代理完成首轮真实模型和默认输出设备运行，中英两份子测试及外层协调器均正常退出，通过范围和实际数据见文末。旧 `real_synthetic_asr_teaching_tts_transport_chain` 及其历史证据保持原语义：等待完整 final 后逐段合成到内存 sink，不能证明生产分段调度或真实输出。

新测试名称为 `server::segment_playback_diagnostic::real_synthetic_production_segment_playback`。它沿用固定成人合成中英 WAV、独立自有 ASR 实例和真实 Kimi provider。ASR 返回的文本进入隔离 simulator 档案，经过真实 `Workstation` 提交、分段持久化、显示确认、分段预约及 `AudioService` 原生输出 owner。仅显示 ACK 是诊断模拟；不会替换 `SegmentSpeaker`、生成假 Started/Completed、创建生产注入 WAV 的 API 或启用麦克风。

每种语言恰好两个显式教学请求，失败不自动重试：

1. `stop_then_late_ack`：第一段确认显示后等待真实 Qwen `first_callback_submission` 回执；先核对 Store 仍为 started、音频状态 speaking、相同 playback ID/epoch，再绑定请求和代次关闭。随后不发额外 Stop、不靠下一次 submit，在两秒内单独观察同一次播放进入 idle 且具有 `PcmOutput.stop()` 后写入的 `pcm_playback`，保存消费者观察延迟。重新读取关闭后的 Store 快照，再确认其余文字，验证迟到 ACK 不启动后续音频。若真实回复只有一段，或首段已经自然结束，记录 `coverage_not_exercised`，该轮及整体不得通过；仍完成原定第二轮，不重试或虚构晚段覆盖。关闭保留的 `gate_closed` 状态不冒充音频 owner 终态，观察到 native owner 退场也不等于测得扬声器物理停止时间。
2. `restart_complete`：再显式提交同一固定转写，按序确认显示，让所有片段由真实输出 owner 完成。重放上轮 close 前也必须核对当前片段仍为 started 且 speaking、playback ID/epoch 匹配，检查新音频代次未被撤销且全部片段获得匹配的真实 Completed 回执。若没有实际覆盖播放中重放，则报告覆盖不足并使整体失败。

在同一个 Store 锁内读取全文 final 状态与真实开始回执。如果回执存在而 `final_validated=false`，`overlap_observed=true` 证明生产回调提交发生在 final 之前，也意味着其 PCM 已经到达；并不提供精确网络首 PCM 时刻，更不等于扬声器物理首音。如果采样未观察到这一关系就保留 false，不人为延迟 final 或重试直到得到理想结果。时间字段均为诊断消费者的观察时刻。

每轮报告保留 `submit`、`observations`、`display_acks`、`final_snapshot` 和全部 `segments`；开始及终态回执已解为 JSON。完成轮只接受 `qwen_pcm`、`first_callback_submission`、正回调帧数、匹配 playback/epoch、`predicted_output_drained=true`、`speaker_output_measured=false`。生产规则可能在尚未提交 PCM 时退回 SAPI；发生时诊断失败，不把 SAPI 标成 Qwen 成功。

显示确认和 final 提交会按生产规则应用候选。因此保存 `fixture_state_before/after`（快状态、真实 turns、有效记忆）并以 `candidate_application_completed` 标记 final 应用阶段确已完成；该字段不声称一定有非空候选或实际新增记忆。所有变化只在新建的合成 simulator 数据库。

运行由 `scripts/validation/voice_model_chain_smoke.py --production-playback` 的协调器控制。它持有外层自有 Qwen 服务，新诊断只用其私有 token 安装客户端到真实输出 owner。环境约定沿用 `AI_TUTOR_REAL_VOICE_CHAIN_SYNTHETIC=1`、`AI_TUTOR_VOICE_CHAIN_LANGUAGE=zh|en`、`AI_TUTOR_VOICE_CHAIN_TTS_ENGINE=qwen`、唯一 `AI_TUTOR_VOICE_CHAIN_RUN`、私有 `AI_TUTOR_VOICE_CHAIN_TTS_TOKEN`，另需 `AI_TUTOR_REAL_SEGMENT_PLAYBACK=1`。编译应在协调器持有模型前完成，直接运行已生成 test exe 的上述 exact ignored 测试。每轮上限 180 秒，无自动重试。

新建报告和方法文件只写独立输出目录；固定 WAV、服务脚本、模型、诊断/生产路径源码及测试可执行文件均记录哈希。凭据不落盘、不打印。所有结果路径均先关闭 Workstation 播报和 provider，再等待自有 ASR 的完整清理证明；TTS 服务由外层协调器 finally 停止。原工作区安装文件、策略、旧 ASR/TTS 描述符哈希核对不变。

明确未验证：真实浏览器显示时刻、麦克风采集、声学首音/音量/听感、完整 voice_loop 权限与重开、同一 main 托管 ASR/TTS 联合生命周期、相机和 PTZ。实际短暂默认输出播放需由根代理安排，不改变系统音量。

实现只新增测试模块及注册，并将旧诊断七个函数和一个常量改为 `pub(crate)` 的测试专用共享入口；无生产调度、音频、Store 或 provider 逻辑变更。备份位于 `runtime/goal-backups/20261104/backend_review/production-segment-playback`。

## 首次实际结果与独立复核（2026-11-04）

运行前统一 `workspace35` 为 736 passed、0 failed、7 ignored。根代理随后运行已编译测试程序：外层 `voice-production-playback-first` 正常 exit 0、`success=true`，共 210.094 秒。Qwen 服务从协调器开始计时到通过鉴权和模型 ID 就绪检查为 113.329 秒；就绪不是预热，后续固定中文、英文预热分别为 3671.176、536.010 毫秒，均有非空 PCM。未授权请求为 HTTP 401，本次随机凭证请求为 HTTP 200。

这次实测发生在本片后续活动归属代码修改之前。实际测试程序已另存 `checkpoint15-measured-binaries/ai_tutor-6e6c8bdc539ac8db.exe`，SHA256为 `32d7b62f0bc9e02cb524996b2ab9d681c852ed340d0108d261173da66ec7c3f0`，与方法记录一致；不能把后续工作区源码当作同一次真实模型运行的精确版本。运行前根侧已更新教学证据规则，外层保护哈希证明运行期间未再次修改，不代表本片从未修改提示词。

固定原始证据：

| 文件（均位于 `runtime/goal-baselines/20261104/`） | SHA-256 |
| --- | --- |
| `voice-production-playback-first/report.json` | `5ba422c08b53f5313cadc2a7d8dd867c9008c80c061bb8e77f768c44557d6843` |
| `voice-playback-b34db2d848814da0a49cc062f8c5e6bf-zh/report.json` | `fac0a3c80635c59f5ecbf60283065d0b741f50c3b370182d66cbcbfc76a91dd7` |
| `voice-playback-70e0dbbe2cae45bda8f848106337f97e-en/report.json` | `5fa1d0d186067319b1a68ef3a66fa73a6b51739320d48f2d2f6d0d998b777149` |

独立只读复核按原始 `observations`、`display_acks`、每段 Started/终态回执重新推导，而非只信任 `passed`：外层引用的两子报告哈希、各方法文件哈希、独立 `round-0/1.json` 与报告内嵌轮次均一致；观察时间单调；每语种恰好两次实际教学请求、两次完成、零错误与零重试。四轮覆盖不足列表均为空。两个 simulator 会话在两轮后分别只有两个 user turn；本次有效记忆数量均为 0，不能把 `candidate_application_completed=true` 写成新增记忆。

ASR 在每语种开始时只执行一次实际固定 WAV 识别，其非空终稿用于该语种两次明确提交。下表首 partial 从实时音频泵起点计时，包含固定 WAV 的前导静音，未经过浏览器字幕轮询；尾部从发送剩余 PCM/结束输入到收到 final 计时。未使用 Whisper 回退。

| 语言 | 输入时长（ms） | 首 partial 观察（ms） | final 尾部（ms） | ASR worker（ms） |
| --- | ---: | ---: | ---: | ---: |
| 中文 | 2754.989 | 782.615 | 29.448 | 2796 |
| 英文 | 2644.989 | 772.885 | 41.763 | 2690 |

以下四轮时间均从该次生产 `dispatch` 提交开始计时，单位 ms。ACK 是模拟显示请求完成的观察时间；首回调与 final 是读取 Store 时首次观察到对应事实的时间，不能解释为浏览器呈现、精确网络首包或声学首音。每格是单次结果，不构成延迟分布或前后性能比较。

| 语言 / 轮次 | 首显示 ACK | 首回调观察 | final 观察 | 本轮结束 |
| --- | ---: | ---: | ---: | ---: |
| 中文 / 关闭后迟到 ACK | 12832.731 | 13148.023 | 14764.229 | 14764.893 |
| 中文 / 恢复完成 | 6857.255 | 7149.819 | 9633.354 | 15487.830 |
| 英文 / 关闭后迟到 ACK | 14995.284 | 15364.963 | 19948.313 | 19950.509 |
| 英文 / 恢复完成 | 7818.595 | 8201.831 | 9118.197 | 21571.809 |

四轮实际都返回两段，首 ACK 到首回调的消费者观察差依次为 315.292、292.563、369.680、383.237 ms，含调度、TTS、缓冲及观察等待，不能独立归因于模型。对应同锁“Started 已存在而 final 尚未验证”的观察数为 3、3、4、1，因此四轮 `overlap_observed=true` 均有直接证据；首回调与首次 final 观察差分别为 1616.205、2483.536、4583.350、916.365 ms，不是精确管线重叠时长。

逐项覆盖与边界：

- **真实停止：** 中英首轮关闭前均是相同 playback ID / epoch 3 的 `speaking` 输出，Store 首段为 `started`。绑定关闭请求耗时分别 2.226、1.838 ms；从开始关闭到独立观察同 owner `idle` 分别 30.583、45.961 ms，其间没有下一次提交或额外 Stop。停止后 PCM 统计分别为 2400、2880 回调帧，`predicted_output_drained=false`，支持确实在未自然排空时停止。首段数据库终态来源是 `gate_closed`，真实音频终态回执为 null，未把它冒充 owner Interrupted 回执。
- **迟到显示确认：** 两首轮均确有一个关闭后的新段 ACK（sequence 1），观察时间分别为 13182.126、17384.283 ms，晚于同 owner 退场。该段在全部观察和最终记录中均为 `not_started`，没有 Started/终态回执；文字 final 仍正常完成。
- **旧关闭不影响新轮：** 两恢复轮重放前后均为相同新 playback ID、epoch 7 的 `speaking` 输出；关闭回执仍属于首轮 request / generation 2 / audio epoch 3。新轮随后两段全部完成，最终 `speech_complete=true`、gate 仍 open。
- **真实完成回执：** 两恢复轮共四段均为 `qwen_pcm`，Started/Completed 的 playback ID 和 epoch 匹配，终态来源 `owner_receipt`；中文两段回调帧数为 184320、203520，英文为 345600、284160，全部 `predicted_output_drained=true`、`speaker_output_measured=false`。最后观察的输出状态均 idle。没有将 SAPI 回退算作 Qwen 成功。
- **所有权与清理：** 两自有 ASR 实例均 `forced=false`，launcher/interpreter 退出及两个 Job 为空得到确认；两 provider 清理确认、请求进程均已回收。外层同一 Qwen instance 给出 `owner_stop`、服务与 WSL bridge 均 exit 0、`forced_group_stop=false`，没有只杀 bridge 后声称 Linux 清理完成。外层和两子报告保护文件 before/after 哈希均相同。此处依据运行回执，未将 `Workstation::close()` 本身扩展成所有音频线程已单独 join 的证明。

本次证明固定合成输入下的实际教学调度、分段输出、绑定停声与旧请求隔离。仍未验证麦克风采集/VAD、浏览器实际显示、扬声器声学首音/停止/余响或听感、用户语音打断、voice_loop 自动重开、同一 main 托管 ASR/TTS 联合生命周期、摄像头并发、30 分钟持续负载、儿童声音或真实教室噪声。第二轮响应较快仅是本次顺序结果，未做 ABBA 或受控优化比较。
