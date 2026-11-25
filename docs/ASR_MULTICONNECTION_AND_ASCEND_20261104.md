# ASR 多连接与 ASCEND 验证记录

恢复核验及文档完成日期：**2026-11-06**。模型实测来自暂停前已保留的批次；文件名及 `runtime/goal-baselines/20261104/` 是历史归档标识，不能据此认定本次恢复新增了模型测量。恢复后核验源码和运行环境，完成原生发布构建及隔离HTTP回归，没有新增模型或设备测试。

已落地的源码完成了 **1,315 条全量推理和独立预声明的 12 条实时回放**，两个阶段均正常清理。执行完成不代表识别质量已全面达标：全量仍有 104 条合法空结果，英语主评分 WER 为 50.68%。

服务最多接纳 8 个 HTTP 连接，连接编号按 socket 映射；全部 Recognition 状态与模型操作共用一把 RLock，body 读取和响应写出在锁外。空闲连接不再占据唯一 HTTP handler，expiry 不阻塞 accept；停止只设标志，各 handler 自行关闭连接，全部 join 后才释放 Recognition。成功 start/chunk/final 保活，cancel、health、错误及明确 close 仍关闭。原 3 秒读取预算、50 ms 停止轮询、500 ms connect、2 秒请求及 5 秒 final 超时不变，没有添加音频重试。见[独立静态审阅](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/asr-multiconnection-static-review-independent.json>)。

本次使用固定 Zipformer int8、CPU 两线程、greedy search、auto 语言及原始 16 kHz PCM，20 ms 分块；未启用模型端点检测。600 ms 尾零仅用于模型 final 上下文，不是源音频编辑或额外等待。两个新服务实例各执行一个阶段，每条每阶段一次，无重启、重试或 fallback。详见[测前计划](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-multi-full-plan26.json>)。

| 阶段 | 声明／尝试／有效终稿 | 合法空结果 | 失败／未运行 | 阶段耗时 |
|---|---:|---:|---:|---:|
| 全量 accuracy，无媒体时钟等待 | 1,315／1,315／1,315 | 104 | 0／0 | 323.23 秒 |
| 独立 realtime，原固定子集 | 12／12／12 | 0 | 0／0 | 66.57 秒 |

两阶段均 `forced=false`、`cleanup_confirmed=true`，子进程与解释器退出，两个 Job 均为空；保护文件哈希未变，身份、顺序及 PCM 回执一致。[全量原始报告](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-multi-first/accuracy/report.json>)、[实时原始报告](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-multi-first/realtime/report.json>)及[独立运行审阅](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-multi-first/independent-runtime-review.json>)保留具体身份和清理证据。

主评分限定为原参考不含 `[UNK]` 的 **1,279 条，占 97.26%**；另外 36 条仍保留原参考、输出和状态。104 条空结果（中文 56、英语 45、混合 3）均保留并计入删除错误，没有按模型输出筛选样本。以下转录[全量评分摘要](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-multi-score-accuracy-first/summary.json>)的 corpus-weighted 结果；错误率为总编辑错误／总参考单位，不是句子错误率的平均。

| 原语言标签 | 原始／主评分条数 | CER | MER_project_v1 | 英语 WER |
|---|---:|---:|---:|---:|
| zh | 685／679 | 11.95% | 11.87% | 不适用 |
| en | 257／242 | 35.96% | 52.49% | 50.68% |
| mixed | 373／358 | 33.60% | 24.89% | 不适用 |
| 合计 | 1,315／1,279 | 28.56% | 22.08% | 50.68%（仅 en） |

总 CER 为 5,215／18,261，MER 为 2,451／11,101，英语 WER 为 560／1,105。`MER_project_v1` 使用项目确定性中英混合单位规则，不是作者 jieba／论文评分复现；独立运行审阅也未重新认证评分算法。完整推理不消除参考不确定性，更不能把运行通过解释为中英教学质量验收通过。[RT 评分](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-multi-score-realtime-first/summary.json>)单独保留，其 12 条中只有 11 条属于参考完整集合。

实时回放的时间原点如下，数据来自[根分析](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend26-multi-root-analysis.json>)：

| RT 指标，n=12 | 中位数 | 本组最大值 |
|---|---:|---:|
| 首个 PCM 请求 → 首个非空 partial 回执 | 898.15 ms | 1,700.99 ms |
| EOF final 请求 → final 回执 | 35.44 ms | 39.14 ms |
| 最后 PCM 请求 → final 回执 | 37.35 ms | 40.94 ms |

这是预录文件到服务／客户端的回放；EOF 不是实际松开录音键，partial 回执不是 UI 字幕显示。它不覆盖现场麦克风、VAD、声学端点或端到端交互延迟。n=12 的 nearest-rank 95 分位恰为本组最大值，不能当作人口 P95 保证。全量无节奏等待阶段的耗时用于吞吐统计，不作实时延迟结论。

传输观测覆盖全部已验证 start/chunk/final 响应：accuracy **167,978** 个，RT **3,205** 个；每个阶段的 request_id 分别从 1 连续到上述计数，无缺失、非法或非递增计数。**两个不同实例各自仅观测到 connection_id=3**，没有观测到数据响应连接切换；不能将“3”解释为连接总数，也不能据此声称本次实跑实际使用了多连接并发。未使用或失败连接及 OS connect 尝试不在该统计内。见[传输分析](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend26-multi-transport-analysis.json>)。

此前失败仍按原记录保留：[首轮全量 362／1,315](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-first/accuracy/report.json>)、[前缀一 114／363](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-failure-prefix-first/failure_prefix/report.json>)、[前缀二 143／363](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-failure-prefix-second/failure_prefix/report.json>)、[单录音复用 123／1,315](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-reuse-first/accuracy/report.json>)、[跨录音复用 69／1,315](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-zipformer-cross-first/accuracy/report.json>)条完成，均各有一条失败。本次不补齐或重判旧运行。受控双连接假引擎实验支持旧同步服务可出现排队阻塞的机制，但不能反向认定这些真实失败均由该机制造成，也不能统一追认为端口池耗尽。

ASCEND test 的真实预录音频仅来自两名匿名成人；作者历史年龄标注为 24、25 岁，不能推出当前年龄、年龄效应、儿童能力或广泛成人覆盖。[年龄来源补充](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/ascend-test-speaker-age-supplement.json>)保留原出处。英语错误率和空结果仍是下一步质量优化依据。

2026-11-06 恢复时 root 核验暂停前 324 项声明源码未变；现有speech环境仍为Python3.10.11、sherpa-onnx2.0.1、NumPy2.2.6，仅读取包元数据。[测量构建绑定](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/checkpoint26-multi-first-binaries/manifest.json>)对应实际模型实验所用libtest；[workspace-fiftieth 日志](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/workspace-fiftieth.log>)实际为 **860 passed、0 failed、13 ignored、0 warning**。

恢复后全部原生bin发布构建成功（345.56秒），[新二进制归档](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/checkpoint26-release-measured-binaries/manifest.json>)绑定相同324项声明源码。[新release HTTP回归](<C:/Users/RBMS/Desktop/AI TUTOR/runtime/goal-baselines/20261104/release-smoke-twenty-sixth/report.json>)通过，使用隔离130条合成历史、没有模型或设备；浏览器视觉验收仍未运行。发布候选按同一份原生bin及ASR服务脚本一并归档，安装版尚未替换，完整Goal仍为ACTIVE。
