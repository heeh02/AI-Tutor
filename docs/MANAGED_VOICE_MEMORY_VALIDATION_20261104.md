# 同一工作台托管语音与后台记忆诊断

本片实现一个 Windows 默认忽略的集成诊断 `server::voice_memory_managed_diagnostic::real_managed_voice_camera_memory`。外层 Python runner 只准备隔离资产、启动精确测试 exe、采样资源并记录退出；ASR 与 Qwen 的启动、预热、客户端安装和停止都调用同一个 Workstation 的既有生产生命周期。它不覆盖发布版 main 的 listener/descriptor/启动锁全流程，也不替代已有 30 分钟长测。

固定两轮中文成人合成 WAV 经生产 capture/VAD、真实 Zipformer、Kimi 分段、模拟显示 ACK、Qwen/native CPAL 与 cooldown/rearm。使用已明确选定的真实相机及 worker SHA；不运行云台运动或物理麦克风，不把 callback 计作声学首音。后台来源来自另一组明确的模拟学习者，保持档案隔离。

两个固定后台 job：A 是“以后科学解释先例子、后简短定义”的中文明确偏好；B 是英文已接受但未实施的纸桥对比计划，含一/二折、各一张 A4、10 cm 间隙和相同硬币。每份活动先结束，再由真实 scheduler 发现。来源都明确 synthetic_fixture=true；不改语音原文事件以凑后台调用。日 reservation 上限 4、每活动 2、每 job 最多 2 attempt，max_pending=2、lease=120 秒。

固定顺序：首次 armed 后保持真实空闲，A 的后台自有进程启动；第一轮生产 Start 抢占它，前台完成后等待同一 A job 用新 lease 恢复至 shadow。第二轮真实 ASR 终稿后，在 before-submit 协调点才引入 B；等原有十秒空闲条件及 B 启动后，提交原 ASR 文本，观察抢占并等待同一 B 恢复。没有改生产时钟、取消 idle 门限、直接设置循环阶段或伪造播放完成。若后台提前自然完成导致抢占未覆盖，则保留未覆盖，不能加任务或补抽第三次。

started_at_ms 来自 provider 自有子进程成功启动后的回执，**不能证明远端已接收请求或开始生成**。本片将 remote_generation_inflight_preemption_proven 固定为 false；恢复所需的真实模型候选和进程抢占分别验证，远端生成中抢占仍待独立证据。两份任务空候选时实际状态为 completed，不能当作实质记忆覆盖通过。

后台候选必须保持 shadow，原始来源版本、引用与类别结构均检查，语义仍需独立审阅。前台两轮照常执行生产 candidate apply；报告保留全库、语音学习者和后台学习者的最终 memory 数、正文及来源。前台原始候选没有全部保存，不能据最终状态宣称每个提议均正确，也不能把 background_memories=0说成所有候选都未应用。

收尾先失效输入/播放，分别等待相机 Child/Job、ASR 两层 Job、Qwen Linux 组/bridge/客户端、provider 及 memory scheduler JoinHandle。关键阶段写小型 progress.json，终态报告独占创建；固定失败代码保留可解释原因，未知依赖异常不复制原始凭据、stderr 或思考。外层超时先请求协作停止；若只能终止精确测试 child，Linux 清理保持未确认，不通过进程退出推断干净回收。

独立静态审阅发现并修复一处初始 progress 调用被放到无 out 参数的函数内的问题。统一回归 836/0/10 及初版 Python 3 项通过，源代码及精确二进制绑定在 checkpoint22-measured-*。

## 准备失败与修复范围

首次 `managed-voice-memory-first` 在 0.414 秒内退出 1，停在服务准备之前。旧 runner 把当前脚本与较早 main-managed-qwen-first 的旧 BUNDLE 哈希比较，第一个 managed_tts.py 就不符；当前 managed_tts.py 和 tts_gpu_lease.sh 实际均与成功的 checkpoint20 soak 及 checkpoint22 构建归档一致。目录只创建了空 bundle 文件夹，未复制脚本、生成 setup、启动 child、调用模型或打开设备。原报告只记录 ProbeFailure 类型，具体原因由原代码执行顺序、空目录及逐文件哈希独立复核，原报告未回写。

修复只涉及 Python runner 及其测试：显式 `--build-manifest` 将实际 test/camera exe 和 51 项 Rust、嵌入 fixture、政策/schema、服务脚本绑定到本次测量构建；旧主程序报告只用于模型硬链接来源。ProbeFailure 的固定本地代码可留存，其他依赖异常仍仅留类型。新增回归拒绝未测的 worker 或脚本；现在 Python 4 项通过。两个新文件单独冻结在 `checkpoint22-runner-update-sources/`，其余 305 项来源逐一未变，没有重新编译或篡改原 307 项测前/测后归档。这是服务启动前准备器修复，不是失败模型回复的补抽。

## 修复后的首次真实联合运行

`managed-voice-memory-second` 正常退出 0，外层 272.144 秒。服务 ready/warm/active 的观察在 141.758 秒；active 123.520 秒含真实协调、前台和恢复等待，内层总计 268.326 秒。该运行不是 30 分钟连续推理，也不能与 checkpoint20 拼接成一次长测。

两次输入都沿原始合成 WAV 完成真实 Zipformer 终稿、教学和 rearm，共 6 段实际 CPAL 完成回执。提交至首 callback 观察分别为 20.230/14.870 秒；不是声学首音，仍未达实时目标。provider 记录 2 个 ACP 前台请求、4 次后台进程启动，其中 2 次取消、2 次后台成功结果，另 2 次前台完成；errors/retries 均为 0。没有把两个很早取消的进程计为已确认远端模型生成。

两份 job 均保留原 job ID、来源 snapshot/version、epoch 和 state_revision：首次 attempt 为 preempted，观察到 lease 释放且无候选的 pending 间隙；第二次有不同非空 lease，得到 response_received 和 shadow_ready。独立只读数据库核对为 2 job、4 reservation、4 start、2 shadow 候选、0 持久 memories。A 的中文持续偏好和 B 的英文未实施实验计划各一条，独立语义审阅 2/2 接受；实验条件、未执行/无结果都保留。没有应用后台候选。

前台六段均保留远离设备及可安全触达控制的条件，没有宣称已经停车。最终语音快状态仍是 goal_setting、模块未知/需确认、无结果/实验完成记录；第二轮形成停下目标和待确认车轮/电源步骤，不能把待确认写成学习者已经确认。全库及语音学习者 memories 均为 0。原始前台候选数组未保存，最终状态检查不能替代对每个未留存提议的审阅。

两个时序限制必须保留：初始 armed 真实等待 10.011 秒；B 的 before-submit 协调仅 434.899 ms，原生产门限是距上次 Submit/比较保存的 last_input≥10秒并排除活跃前台/音频，而不是 ASR final 后重新静默十秒。录音 Start/final 不重置该时钟，B 当时距前次 Send 已约 61 秒。其次，scheduler 不等 voice-loop rearm，A 的第二 attempt 已在 rearm 前短暂窗口启动；12.760/10.202 秒只是 round 返回后观察到 shadow 的等待，不是完整第二次模型计算耗时。连续自动语音可能在这个短窗启动后又抢占后台、消耗预算，这是下一片需改进的真实空闲准入问题，不放宽当前判定或预算。

相机留下 125 条 active 元数据、同一 epoch，720p，末次处理 FPS 26.022；启动样本较低，不宣称每一时刻均达目标。最大观察 gap 1,012.566 ms、尾 gap 512.745 ms，覆盖通过。首次/末次 active 接收计数 1/3,212，清理回执 3,226 的额外帧不算入 active；无浏览器呈现或人物跟随验收。

全 child 生命周期 54 次 Windows 树采样 RSS 最大 925,069,312 B（882.21 MiB），不含 WSL Qwen 主机内存；9 次 GPU 聚合采样最大 8,084/12,227 MiB（66.12%）。无采样错误，均为离散采样最大值，不是独占归因或真实峰值。ASR 非强制停止且两 Job/解释器回收确认；Qwen owner stop 的 service/bridge 退出 0、非强制组停止、Linux 清理及客户端停用确认；camera child wait/Job 空、采音 producer 和 scheduler JoinHandle 均确认。provider 整体 close/running=0 和两次前台的逐请求回收有记录，取消的两次后台调用没有单独保留逐次 reap 回执，不能将整体关闭扩大为逐次回执都齐全。保护文件哈希不变。

完整独立方法知情审阅为运行目录中的 `independent-review.json`。它接受本方法声明的有限运行范围，不是盲评、完整 main 启动、声学打断、真实麦克风、浏览器、远端生成中取消或 PTZ 现场验收。
