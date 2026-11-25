# 冻结类型化向量的真实 Store 诊断

本片只使用已完成的真实投影及 BGE 编码产物，重建新的临时 simulator Store 并验证缓存与检索。没有运行模型、设备，也没有改生产默认、原 raw-60/raw-18、投影或编码脚本/方法/报告。

## 固定输入

| 产物 | SHA256 |
|---|---|
| typed-projection-first/report.json | `5feb84ddc072b05a8bc9106082bfec096f8f5ec3208403f0ca9c018cca19e426` |
| typed-projection-first/encoder_inputs.jsonl | `d93cfa39f184e51359837ad6ed67f31688ada1a54a8c8d5f57df447df38e2368` |
| typed-encode-first/report.json | `a1545f053fc843ff6aeee0c274790b83e66aeb6032d15a4bd04845b11b2a5a7d` |
| typed-encode-first/bge_m3-encodings.json | `925fffa9bb29737fcaa6014edb42a55a010c38d7276859e2981aa578a37ea6f2` |
| 原 challenge v2 manifest | `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81` |

文件 hash、报告交叉引用、BGE profile，以及所有 234 条输入/向量 key 和文本 hash 都必须一致。每条向量复核 1024 维、有限归一化值及 f32 little-endian hash。查询只从冻结 encoder inputs 读取，再与原 v2 当前问题或 question+goal（含末尾空格）核对；不重写查询。

## 重建和缓存

新 probe 的紧凑重建函数重复现有公开 Store API 调用顺序：按原六主题创建 session，录入原 60 条事件；42 个受支持候选按每批不超过五个 enqueue/claim/stage/apply。旧投影 probe 函数为私有且含独立 main，因此没有修改冻结源码来抽通用框架。

每条实际晋升结果都与冻结投影比较：正文、memory kind/scope/activity/source type/status/epoch、完整 education metadata（含 grade/support/importance）、来源内容/类型/版本/状态，以及真实 source links。新生成的 owner/session/event/memory ID 和时间不强制伪造为旧值，而是在报告中保留新旧映射。编码文本必须与实际 `memory.content` 逐字一致。

`embedding_work` 首批 32 条经真实 CAS 提交后，再取 10 条；确认无重复、剩余 work 为零、eligible 为 42。最后逐条检查实际读出的缓存向量和 snapshot 正文与已冻结编码输入完全一致。这一片补足了前一投影探针只检查首批 32 个 work snapshot 的边界。

18 条未投影来源及原因原样保留。没有把 condition_limit 变成回答/观察，没有把未接受计划或临时请求升级，也没有生成新的教育状态。

## 检索与计分

两种查询条件各 96 问，共 192 行，分别保存：

- 当前真实 Store 的 `recall_candidates(..., None)`。
- 真实 eligible 缓存的完整 42 条纯 cosine 排序及整条记忆预算选择。
- 当前真实 Store 的 BGE + legacy 词面 RRF 返回。

前五条、2400 个 Unicode 字符、整条记忆预算，不截断。当前 Store RRF 仍是两路融合，不能称作纯 BGE；legacy 词面也不是此前冻结的新词面 shadow 算法。Dense 和最终 RRF 使用真实 memory ID 排同分，legacy 保留时间/行序；记录完整 dense 的同分组及实际返回范围内的 RRF 同分组，不声称观察了返回范围之外的边界候选。

原标注 72 正题、24 负题不变。另列至少有一套**完整最小充分证据集合全部落在这 42 来源中**的可覆盖正题分母。不能从原集合里裁掉缺失来源后再算完整；多个合法最小替代集按“完整覆盖任意一个”计分。报告原分母/可覆盖分母的 complete@5、原正题 MRR@5、coverage、原跨语 24 问、mixed_multi 11 问及主题分层，并列出 RRF 相对纯向量完整证据退步的逐题 ID。

负题仅报告是否仍返回候选，`abstention_tested=false`；不从候选排名推导拒答或事实正确性，也不继承 raw-60/raw-18 的质量 gate。

## 本次真实控制

检索计分完成后再操作临时来源，避免控制变更污染前面的排名：

1. 同 namespace 下第二个实际学习者拥有独立事件和同一类型晋升后的相同正文，并提交同一合法向量。两个 owner 都非空，核对各自 eligible 数量、召回归属和伪造跨 owner receipt 拒绝。
2. 通过 `revise_education_event` 更正主学习者当前 session 的真实来源；旧 evidence 和迟到 CAS 失效、旧条目不再返回，另一 owner 的缓存仍保留。当前实现的 learner epoch 会清主 owner 全部派生缓存，此行为明确记录。
3. 主 owner 的一条仍有效记忆重新提交其冻结向量后，通过真实 `forget` 删除第二 owner 的条目；其 evidence/cache/候选均消失、迟到 CAS 被拒，主 owner 的缓存保持。
4. 本次分别检查错维度、错 profile 和未知 profile。错 profile 控制使用合法归一化的 384 维 E5 单位向量及 E5 key：其自身维度/范数通过 E5 校验，但不能提交到 BGE snapshot；真实 BGE 缓存非空、E5 缓存为空时，该合法 E5 query 只能得到非空词面回退，不能读出 BGE semantic rank。这条合成单位向量只用于控制，不混入 42 条 BGE 向量或质量计分，排除了“只因维度错误而拒绝”的混淆。没有继承旧探针的控制通过记录。

这些都是临时 Store 路径，不代表设备、生产接口或跨活动授权已验证。逻辑 workshop 到现有 modular_rover 的合成 activity 映射限制保持不变。

## 编译与执行

根代理可单独注册 bin `typed-store-probe`，路径 `../../scripts/memory-lab/typed_store_probe.rs`，保留默认 `test=true`。四项纯测试覆盖不可裁缺失证据、替代集、负题候选不等于拒答、超预算整条跳过；无需运行模型或读取向量资产。

```powershell
cargo test -p ai-tutor --bin typed-store-probe
cargo build -p ai-tutor --release --bin typed-store-probe
target/release/typed-store-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --output runtime/goal-baselines/20261104/typed-store-first/report.json
```

方法 SHA：`4d4a9fffe0aaa1ebf244f3e3b216a34c5bb1ade4e99b3eb8a8f13eb3e412b5ec`。
源码 SHA：`21e07f0c13d17c6492d4e1269baa8d44db0a58f1706f5977aa9eb7b7db6eb190`。

根代理已完成 release 编译和首次执行，退出码 0。真实结果为 `runtime/goal-baselines/20261104/typed-store-first/report.json`，SHA256 `ff9d876a315074cc5c1bd7bcd8b277719a37f6df9968cae217703640d87a38ad`。进程墙钟 3.875 秒；报告内部测量 2.128 秒，范围为 JSON/hash 核验、临时 SQLite 与排名，不是模型编码延迟。本节不把 release 构建和诊断执行当作尚未确认的独立单测通过记录。

## 首次实际结果

42 条记忆均通过实际类型晋升与逐项 evidence 比较，缓存两批为 32 + 10，42 个唯一提交、42 个 eligible、剩余 work 为零。四组实际控制全部通过，包括两个非空 owner（42/1）、来源修订、真实 forget、彼此独立的 profile/维度控制；合法 E5 单位向量仍只在控制中出现。

原 72 个正题中有 60 个至少保留一套完整最小证据集合，另 12 题无法由这 42 条支持。原跨语 24 题的可覆盖分母是 18，mixed_multi 保持 11。以下完整证据数首先以原分母计分，不把可覆盖子集当作整个挑战集：

| 查询条件 | legacy 词面（原 72 / 可覆盖 60） | BGE 纯向量（原 72 / 可覆盖 60） | 实际 BGE + RRF（原 72 / 可覆盖 60） | 跨语纯向量 / RRF（原 24 / 可覆盖 18） | 双证据纯向量 / RRF（11） |
|---|---|---|---|---|---|
| 当前问题 | 21/72；21/60 | 55/72；55/60 | 55/72；55/60 | 各 15/24；15/18 | 11/11；11/11 |
| 问题 + 目标 | 20/72；20/60 | 55/72；55/60 | 52/72；52/60 | 各 15/24；15/18 | 11/11；10/11 |

两条件、三种方法的 24 个负题都仍返回候选；这不是拒答通过。当前问题纯向量的主题完整数分别为发芽 8/10、纸桥 10/10、分数 10/10、阅读 10/10、循环 10/10、蒸发 7/10（每主题原正题仍为 12）。

“问题 + 目标”的实际 RRF 相对纯向量丢失 `germination_q02`、`fractions_q02`、`fractions_q12`。它们需要的可用来源分别是 `germination_s02`（dense 第 4）、`fractions_s02`（第 3）、`fractions_s05`（第 4，另需已保留的 s06）。词面返回的其它计划或回答参与融合后占据前五，相关来源缺少词面名次而退出。没有发生字符预算跳过；不能把该下降归因给 BGE 编码。当前问题虽然两方法完整数同为 55，RRF 的原 72 题 MRR@5 为 0.581，纯向量为 0.723，完整数相同也不表示排序等价。

## 原文、候选池与类型前缀的事后配对

为区分“删去 18 条来源”与“合法投影增加显示前缀”，新增只读脚本 `scripts/memory-lab/typed_store_paired_analysis.py`，SHA256 `bb91c074dd6b6fbef9f170b2314af778abad5078eca4c9941529dccd3010a2cd`。输出 `runtime/goal-baselines/20261104/typed-store-paired-first/report.json`，SHA256 `cacff0c66517f831df7cc25971ba62d5c1d42d298c9ee9089bd97f71ee05f7e0`。它复用冻结完整排名，只过滤同一组来源，不重新编码或打开 Store。

除本页上述固定产物，新增输入为 raw-60 首次报告（SHA `b614aa9fcad3d12fb0ff6f4f3319eb0b73fbceac903a5aa967f7b76442a09cae`）及其 `bge_m3-encodings.json`（SHA `a8fd7529581098579f28941d57099ed59b1de5f4bec4ecda2a9b89bf0839ed24`），均在 `runtime/goal-baselines/20261104/bge-challenge-first/`。

全部 192 查询的文本 hash、token inputs、向量逐值完全一致。42 条存储文本逐字等于“已识别的中文类型前缀 + 原始完整文本”。保留同一 top-5/2400 字整条预算；全部纯向量排名无相邻精确同分，全部选择无预算跳过。因此本次成对差异不由查询编码、预算或 tie 排序产生。

| 条件 | raw-60（完整数 / 原 72） | 同组 raw-42 | typed-42 | 同一可覆盖 60 题中的 raw-60 → raw-42 → typed-42 |
|---|---:|---:|---:|---|
| 当前问题 | 69 | 54 | 55 | 57 → 54 → 55 |
| 问题 + 目标 | 66 | 56 | 55 | 54 → 56 → 55 |

当前问题的五个可覆盖失败并不是由中文前缀统一造成：

| 问题 | 必要的可用来源 | raw-60 排名 | 同组 raw-42 排名 | typed-42 排名 |
|---|---|---:|---:|---:|
| germination_q02 | germination_s02 | 12 | 8 | 6 |
| germination_q06 | germination_s06 | 25 | 18 | 25 |
| evaporation_q02 | evaporation_s02 | 12 | 9 | 6 |
| evaporation_q06 | evaporation_s06 | 9 | 8 | 7 |
| evaporation_q08 | evaporation_s05 | 11 | 6 | 6 |

两个 q02 在 raw-60 中都有第 1 名的另一合法充分来源 s03（条件限制）；该类型尚不能合法晋升，删除后剩余 s02 虽被前缀提高排名仍未入前五。三条跨语题在 raw-60 中已失败。`germination_q06` 的前缀确实让排名 18→25，但没有使原本通过的题退步。当前问题 raw-42→typed-42 没有完整证据退步，`paper_bridge_q02` 的 s02 从第 8→5，净增一题。

“问题 + 目标”存在一个可归于本次文本表示变化的完整证据退步：`evaporation_q06` 的 s06 从 raw-42 第 5 降到 typed-42 第 7。raw-60 中它原为第 6，缩池曾使它进入前五。相反，其他排名变化可能改善。不能据这一题断言应移除全部安全限定前缀，也不能由当前问题净增一题宣称前缀无损；后续若采用独立检索文本，仍须把完整显示正文、来源与类型限定保留，并冻结新方法再比较。

这是已见结果后的解释性分析，不是新盲测、真实 Store 重跑、原 60 源质量门槛通过或生产启用依据。原方法、投影、编码、挑战标签和实际 Store 报告均未修改。
