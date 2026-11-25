# 当前普通教学路径的 30 分钟 Qwen 联合运行

本轮在同一 Workstation 中完成 42 轮中英教学、相机持续采集及两个后台记忆任务的抢占/恢复，独立运行和保留内容的语义审查通过限定范围。完整 P0–P5 Goal 仍为 ACTIVE。

## 冻结绑定与方法

证据目录为 `runtime/goal-baselines/20261106/qwen-soak-first`。使用固定成人 SAPI 输入，经现有捕获/VAD、CPU partial 和 Qwen3-ASR-1.7B final，接普通默认 Kimi 路径及 Qwen TTS/native CPAL。输入源、显示 ACK和 heartbeat 是诊断替身；相机走实际 release worker。

- 354 文件源清单 SHA256：`b0b122efdecd2565a090908cf8a43f934021bd7f52dc1c6c6d6c00b4499ddb46`。
- 优化 ignored-library SHA256：`cd049c9f26c5556b009053c3bbf4e171c2a747eb20a39b1ae9bf01c7fed25563`。
- release 相机 worker SHA256：`09bc4a807a140efb98725566938f8c1afb5ba127a09afe247d83191593cca08f`。
- root 软件检查为 933 通过、0 失败、17 忽略，9 项 Python 验收检查通过。独立审查复核归档日志，没有重跑。
- 准备观察上限 380 秒；要求 1800 秒 active，允许在途工作收尾至 2100 秒；整个操作上限 2540 秒、child 上限 2660 秒。额外失败清理 backstop 不算接受窗口，本次未触发。

模型使用 `qwen_pair_12gb_v1`、同一 Ubuntu 发行版和既有 owned lifecycle，未改用户策略或安装版。独立审查核对服务 bundle、小资产哈希、模型 manifest及 hardlink 绑定，没有重复计算大型权重哈希。教学没有 Highspeed override；当前普通路径的 retained metrics证明 `low` 与清理，不含每次远端 resolved model ID。

## 实际结果

| 项目 | 结果 |
| --- | --- |
| 前台轮数 | 42：中文 21、英文 21 |
| 模型就绪观察 | 225.160 秒；冷准备不计 active |
| active＋有界收尾 | 1833.553 秒 |
| 外层 | exit0，2068.049 秒 |
| 英语转换 | 第 22 轮，active 约 912.584 秒；转换 12.076 ms |
| 正式 ASR | 42 个 Qwen final；无回退或强制改写 |
| 正式 TTS | 116 个片段，116 组匹配的 native 启动/终态回执 |
| provider | 46 调用＝42 前台＋4 后台；44 完成、2 取消、0 错误/重试 |
| 输入源 | 中文、英文各 open/close 21 次，全部 producer join |
| 整卡抽样峰值 | 10726 MiB / 12227 MiB |
| Windows 观察进程树峰值 | 1003069440 bytes；不含 WSL 模型 RAM |
| 相机 | 1835 样本、单 epoch；中位约 25.15 fps，最大样本帧龄 128 ms |

首个后台任务 A 在 active 20.070 秒开始，Start 后观察到抢占；第二个 B 在 1524.054 秒开始，Submit 后观察到抢占。两个任务均释放 lease，并以同一 job的第二次 attempt 恢复，分别耗时约 21.374/25.246 秒。数据库共 2 个 job、4 个 attempt、2 个 shadow候选，没有第三次 attempt或替代任务。owned provider开始及抢占回执不证明云端物理计算的开始/停止。

两侧 GPU 角色在 heartbeat、每轮及完成时保持就绪，TTS 可见实例不变；公共接口未暴露 ASR 起始实例 ID，不能补造逐轮实例比较。CPU ASR、final ASR、TTS、相机、scheduler、provider和两组捕获源正常退出；final ASR 无协议失败，桥接/service均 exit0，无强制组终止，音频绑定清空。独立审查没有额外执行 OS 进程组扫描。

## 语义与来源

42 个完成文件均先经过有效 JSON及稳定哈希检查再滚动审阅。终态确认无 WAL/SHM 后，以只读 immutable方式检查 SQLite，逐轮核对原始终稿、来源、request、generation及已交付文本。模拟库有 3 个学习者/会话、44 个来源、84 个 shown turns；fixture real库为空。

前台输入始终是停车请求。回复将实际停止状态作为待确认问题，停车控制的使用附带无需靠近运动部件的前提。第 8 轮保存的目标忠实复制原始意图；转换英语后含义不变。pending_step、reported_result、attempts保持为空，没有把老师的方法当作已接受计划，也没有宣称小车已执行或成功。每轮及终态全状态 memory审计均为 0。

后台 A 保留明确的持续偏好：今后讲科学概念时先举具体例子，再给简短定义。后台 B 保留已接受但未实施的纸桥比较：一折/两折、每桥一张 A4纸、10 cm跨度、相同硬币、尚无结果。两者的 learner、来源 ID、版本、原句和作用域与各自数据库记录一致，保持 simulated/unknown，不持久化。它们是预先带明确偏好或 accepted标记的合成来源，不能替代普通自然输入的记忆验收。

原始未应用/被拒绝的前台候选数组未保留；语义结论只覆盖可见交付文本、实际状态和两个保留的 shadow候选。

## 时延与未覆盖范围

提交接受到首次 native回调为 **7.161–21.325 秒，中位 12.962 秒**，包含教学片段产生及 native提交，未达到 5 秒首句目标。端点关闭观察到终稿的范围为 299.593–517.733 ms，中位 388.361 ms；两种计时不能混用，也都不是声学首音。

本轮未验真实麦克风、浏览器、声学输出、实体 PTZ或 release-main启动；固定成人语音也不代表儿童、自然输入或跨年龄性能。42 次重复请求不是 42 个独立教育任务，也不构成普适质量或 P95证明。

当前模型默认没有切换。DeepSeek Flash已获用户授权并在准备中，**尚未测试，没有速度/语义结论，也未切为默认**。后续仍需推进低时延、自然记忆路径及剩余验收，不能把本次限定通过写成 Goal完成。

## 证据

- [实际报告](../runtime/goal-baselines/20261106/qwen-soak-first/actual-first/report.json)、[runner报告](../runtime/goal-baselines/20261106/qwen-soak-first/actual-first/runner-report.json)。
- [独立运行审查](../runtime/goal-baselines/20261106/qwen-soak-first/independent-runtime-review.json)，SHA256 `8ca4ca9733e60adbbe3be7739a74cd7148ebd444397a9a32dccd63a4af633e17`。
- [独立语义审查](../runtime/goal-baselines/20261106/qwen-soak-first/semantic-review.json)，SHA256 `3b090759e06efdd08ad238bd2acd48fed2c12ea712582d9a3f0ce47faf3b63bf`。
- [42 轮滚动审阅](../runtime/goal-baselines/20261106/qwen-soak-first/rolling-semantic-review.jsonl)，SHA256 `3315db59e6200c97f98622b6ad9c039d35904821ea2a830dcd787ec011ff5b99`。
