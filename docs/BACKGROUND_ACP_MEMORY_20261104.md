# 后台结构化记忆的 ACP 传输

KimiProvider::consolidate 现接入独立的 kimi_acp_memory 模块，复用既有工具隔离、工作目录、profile/low 核验和同一个前台优先 gate。只有完整 JSON 对象、记忆响应外层合同、profile 和本地清理通过才返回；全部候选的来源/版本/租约仍由原 Store 验证。没有自动 headless 回退、部分候选提交或额外重试。

原 started_signal 仍只表示自有进程及 Job 建立，用于尝试预算。active_acp_memory 与 last_acp_memory 另绑定 request、应用 session、job、attempt 及独立 observation_id，记录首个非空 thought/message 事件的时间和协议终态。私有 job/attempt 字段在 build_prompt 时被移除，不进模型输入；thought 正文不复制到应用状态、报告或候选。CLI 自身会临时写 session journal 供既有 profile 核验，仍须由 OwnedRun 清理，不能宣称磁盘从未出现任何 CLI 临时记录。

ACP 的 session/cancel 是通知。代码只在 close/kill 前收到原 prompt 的匹配响应且 stopReason=cancelled 时记录代理协议取消回执；写成功、EOF、end_turn 或进程退出都不替代该回执。观察预算与通知写入合计 500 ms，随后分别有界收尾。名义异步预算约 1,600 ms，位于 worker 原有两秒宽限内，不把同步文件操作或 OS 调度当作硬实时保证。[官方 ACP v1 取消说明](https://agentclientprotocol.com/protocol/v1/prompt-turn#cancellation)

Kimi 2.3.1 的 message chunk 也可能是本地提示，故更强的在途输出预检要求本请求非空 thought 事件及 prompt_pending，仍不称为供应商物理推理遥测。代理 cancelled 终态也不证明远端物理计算停止。应用的即时进度以布尔标志和首事件时间为准；累计计数在正常收尾汇总，不能把异常 Drop 的未完成快照当作完整计数。清理回执必须另读 cleanup_confirmed/process_reaped/stderr_drained/owned_directory_removed，不能只看取消错误码或 provider.close 返回 Ok。

六项新离线回归已实际通过：完整结构与私有标签、thought/message 区别、匹配取消终态、错误 ID/EOF/end_turn/超时不得假报取消、重复请求/观察归属、带合成 profile 的完整工作流。首次过滤测试由于应用文件保留旧 mtime，Cargo 复用了旧 libtest，匹配零项；该次单独记为未执行，不算通过。刷新 mtime 且文件字节未变后，第二次实际编译并执行六项成功。随后 workspace48 的 848 项完整回归通过。

新方法 background_acp_method_v1.json 预声明一个全新摆锤来源、新 job、后台最多两次和前台一次，总上限三次；先观察 thought 输出，再通过正常前台 API 抢占并恢复同 job。真实 harness 已完成 prepare/run 两阶段：先冻结实际 Store、来源、方法和精确 exe，再用 manifest SHA 独占开始一次运行。前台先正常 select，再通过 POST /turns 提交；模拟 ACK 等 final 和 Pending.reply 均可见后保存完整候选，再按序提交。它不运行相机、语音或 PTZ，不替代更大联合负载及现场项目；原两个 checkpoint22 job 不补抽、不改分。

## 本片实际预检

源码归档 `checkpoint23-measured-sources` 有 312 项；对应 libtest SHA `5defea10bd0a2bbf321692ede8df89aca5ddc9cefb5fc82d6263d180d2a91083`。最终 workspace49 为 **851 通过、0 失败、11 默认忽略，UI128，无 warning**，127.479 秒；native31 全 bin release 182.050 秒成功，HTTP 回归 1.403 秒通过。准备模式单独运行并经独立核对：2 档案/2 会话、1 个固定模拟 observation，0 job/attempt/调用；dirty due 行不是模型调用。

`runtime/goal-baselines/20261104/background-acp-first/` 的实际 run 正常退出 0，外层 57.598 秒。确切一次前台、两次后台，1 取消、2 完成，错误/重试为 0。首次后台在进程启动后约 1.919 秒观察到非空 thought；harness 读到匹配 job/attempt/request/observation、prompt_response 且仍 pending 后，通过正常业务 Submit 抢占。随后收到同 prompt 的 cancelled 终态，发生在 close/kill 前，且该次 process wait、stderr 排空、owned directory 删除都确认。

Submit 开始至观察到 preempted 与匹配终态约 61.330 ms，这是包含业务提交和轮询的复合观察区间，不是单独 cancel RPC、远端停止或声学静音指标。前台完成两个有用的英语句段；完整 task/memory 候选均为空并已保存，不能由空候选证明某类非空更新正确。它没有引用后台摆锤资料或声称学习者已掌握知识；上下文无异档慢记忆，最终全库持久 memories 为 0。

第二 attempt 保留同 job、源版本/内容、epoch/state_revision，使用新 lease/request/observation。最后一次仍未见 attempt2 的观察在最终 ACK 开始后 13.760 秒，首次见到为 13.829 秒；此区间不冒充精确 claim 或 guard-release 时刻。实际第二次后台成功收尾约 10.396 秒。独立语义审阅接受唯一 shadow：保留独立计数 8/11、两个 10 秒区间、20 cm 条件，以及尚未重复、不能据此形成一般结论的限制；source_grade 仍为 simulated，support_level 为 independent，没有应用候选。

三次实际调用均有匹配的 process_reaped、stderr_drained、owned_directory_removed 回执；scheduler 真正 join/停止，provider running=0，memory worker 空闲，应用 running/pending 空。无本地 ASR/TTS 模型启动、音频播放、相机或 PTZ 请求。关闭后 ASR 控制器虽然仍 busy=true，但 mode/phase=disabled、active=false、shutting_down=true，不能写成所有 busy 状态都为 false，也没有把该锁定标记算成已启动服务泄漏。

独立审阅为 `independent-review.json`，采用已知方法的代理审核，不是人类盲评。业务 dispatch 不证明网络 listener/auth；延后 ACK 不计作实时显示；非空 thought 属于代理协议输出，cancelled 也不证明供应商物理算力停止。原始记录保持不变，后续语音/相机/ACP 长时间联合仍须单独验证。
