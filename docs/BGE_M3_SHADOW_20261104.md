# BGE-M3 独立跨语候选实验

状态：根代理已完成真实 Rust 词面适配器与 BGE/E5 CPU 推理；本页末尾记录实际结果及独立审读。**BGE 仅当前问题条件通过候选覆盖门槛，拼接 goal 条件未通过；不启用默认召回，不表示答案或拒答质量通过。** 生产召回、E5 实现、模型默认值和冻结挑战集均未修改。本页原准备记录保留，下文按时间区分。

## 固定方法与资产

`scripts/memory-lab/bge_m3_shadow_method_v1.json` SHA-256：
`2f7abcf3f30ae3c31f62730f59e3dc792aedcadeac570585d1e8f0be2085bf17`。

- BGE-M3 官方 revision `5617a9f61b028005a4858fdac845db406aefb181`，权重 `pytorch_model.bin` 2,271,145,830 bytes，SHA-256 `b5e0ce3470abf5ef3831aa1bd5553b486803e83251590ab7ff35a117cf6aad38`。模型卡声明 MIT；保存原卡，不用社区量化或转换文件代替作者权重。
- 1024 维，原始文本不加 E5 前缀，CLS 后 L2。不运行 sparse/ColBERT，不下载它们的 head。官方该版本没有 safetensors，限定 `weights_only=true`、`local_files_only=true`、`trust_remote_code=false`。
- E5 使用已核验的原始 FP32 safetensors、同一固定 revision，384 维、query/passage 前缀、attention-mask mean 后 L2。词面使用现有 Rust 模块，模块 SHA `3a829d33ec17f4c1f44da457d53285a1486adb40a411bce5e8ef610f1ef6a988`；没有 Python 分词近似实现。
- CPU FP32、batch=1、2 个计算线程、1 个 interop 线程、eager attention、eval/inference_mode；先 E5，卸载后 BGE。禁 GPU、禁在线兜底。输入 512 token 上限，超长即失败，绝不截断来源。

原始链接：[固定模型树](https://huggingface.co/api/models/BAAI/bge-m3/tree/5617a9f61b028005a4858fdac845db406aefb181?recursive=true&expand=false)、[固定模型卡](https://huggingface.co/BAAI/bge-m3/blob/5617a9f61b028005a4858fdac845db406aefb181/README.md)、[CLS 配置](https://huggingface.co/BAAI/bge-m3/blob/5617a9f61b028005a4858fdac845db406aefb181/1_Pooling/config.json)、[官方推理代码](https://github.com/FlagOpen/FlagEmbedding/blob/897baa67c0ffddbfccf4e6776ff55310d67809d4/FlagEmbedding/inference/embedder/encoder_only/m3.py)。模型的公开跨语成绩只支持选择这个候选，不能替代本项目验证。

`download_bge_m3.py` 仅从该固定 revision 下载；LFS 文件校验作者 SHA-256 和大小，普通文件校验 Git blob ID、记录 SHA-256。下载到 `.partial`，通过校验再就位；不同的已有文件不覆盖。资产位置 `models/memory/bge-m3`。Windows 使用 `download_bge_asset.ps1` 中与先前 E5 一致的 HTTPS 栈，不改变系统执行策略；仅该子进程显式允许执行自有脚本。

前两次下载尝试的网络超时/脚本执行策略错误保留在 `runtime/goal-baselines/20261104/bge-m3-assets-first` 与 `bge-m3-assets-second`；第三次使用 Windows HTTPS，`bge-m3-assets-third/report.json` 实际为 `verified`，8 文件全部通过，耗时 892.594 秒。该报告 SHA-256 `33c039e4790f3183eaddd69caf21fdde6ee2f81263941e36c6d0555ce6743c3a`；没有更换镜像、revision 或权重格式。

## 数据与比较口径

只接受审读后的 `education_memory_diagnostic_postfreeze_v2_20261104`，必须由根代理传入其精确 manifest SHA。v1 含最小替代证据遗漏，探针明确拒绝；本片不修数据。v2 的 loops_q11 改为 mixed_single 后，多证据分母为 11，达标数量仍为 10；分类与分母从冻结标签读出，不按旧的 12 硬编码。

全部 60 来源、96 问题，当前问题与精确 `{text} {goal} ` 两个条件。每模型编码 252 项（60 来源 + 192 问题条件），不是只跑最难或最有利的子集。问题标签、主题、语言与 claim 类型不进入向量或词面打分。

保留完整排名；选取时最多 5 个来源、2400 Unicode codepoint 整条来源预算，超过剩余额度则跳过整条，不截正文。排序 tie 按来源 ID，不用标签。报告无预算 top5 与实际预算选择两套指标，主要结论用后者。替代最小证据集合采用“任一集合完整覆盖”，不能把替代集合求并集或把部分命中算完整答复。

输出 hit@1/@3、MRR@5、complete-evidence@3/@5、合法集合最佳覆盖、首次凑齐完整证据的 RR@5；按题型、语言、主题和 tag 分组，逐题列 BGE 相对 E5/词面的退步，以及 goal 对各方法的改变。负例只报告候选返回，不把空结果称经过校准的拒答。原词面解释和 goal-only 命中保留。

12 个 owner/namespace/activity/status/version 控制由逻辑 fixture gate 单独报告，不混入向量效果，不声称这覆盖真实 Store 权限集成。没有用户 DB。参考门槛仍为 72 正例完整覆盖 >=65、24 单一跨语 >=20、多证据 >=10、范围零泄漏；即使通过也只支持下一轮 opt-in 验证。

## 根代理执行顺序

1. 等 v2 标签审读冻结后，保存 manifest SHA；等根代理允许修改构建清单，再将 `scripts/memory-lab/lexical_challenge_adapter.rs` 注册为专用 bin `lexical-challenge-adapter`。其相对路径已按 scripts 位置书写，不必复制进生产模块。仅使用已有 anyhow/serde_json/sha2/regex 依赖。
2. 用已有 WSL Ubuntu Python 执行 prepare。示例在 Linux shell 中，`ROOT='/mnt/c/Users/RBMS/Desktop/AI TUTOR'`，`PY="$ROOT/runtime/speech-gpu/omni/bin/python"`；`V2_SHA` 必须替换为审读实际 hash：

```sh
"$PY" "$ROOT/scripts/memory-lab/bge_m3_shadow.py" \
  --root "$ROOT" \
  --fixture "$ROOT/rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2" \
  --fixture-manifest-sha "$V2_SHA" \
  --output "$ROOT/runtime/goal-baselines/20261104/bge-challenge-prepare-first" \
  --prepare-only
```

3. Windows 编译后的适配器只在新版数据上执行；输入准备目录的 `lexical-request.json`，输出一个新 `lexical-result.json`。它输出 192 行实际 Rust 词面排名与编译时源码 hash。Python 探针拒绝输入 SHA、模块 SHA 或适配器源码 SHA 不一致的结果。
4. 在根代理安排的无编译/模型负载 CPU 窗口执行同样命令，改为 `--run-model`、另一个新的 `--output`，并增加：

```sh
--prepared "$ROOT/runtime/goal-baselines/20261104/bge-challenge-prepare-first" \
--lexical-report "$ROOT/runtime/goal-baselines/20261104/bge-challenge-prepare-first/lexical-result.json" \
--load-note 'root-controlled idle CPU window; no other model inference or build'
```

预备与运行之间重新核对资产、脚本、checker、方法、fixture、分词数组和词面请求 hash；不允许悄悄替换样本。每模型保存有限向量与逐输入耗时，不保存巨大 hidden tensor；完整排名和分组统计进入 `report.json`。调用顺序固定，不能把耗时称平衡性能比较。资产约 2.30 GB，内存粗估 4–8 GiB；这是未实测估计，不是性能或资源验收。

已只用 `importlib.metadata` 在现有 WSL Ubuntu 环境确认 torch 2.13.0+cu130、transformers 5.19.0、numpy 2.3.5、safetensors 0.8.0、tokenizers 0.23.3。随后根代理已审读并授权 v2 manifest `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81`，并允许分词准备；没有安装软件或执行编码模型。

实际准备报告 `runtime/goal-baselines/20261104/bge-challenge-prepare-first/report.json` 为 `prepared_no_retrieval`、`model_executed=false`、`network_used=false`，SHA-256 `acddb446d01720b57a7d0734b857d5a542de9d77fb3d8828c7bab0fd2044c9d7`。两模型各完整记录 252 个输入，E5 长度 13–68 token、BGE 10–66 token，没有截断；12 个逻辑范围控制符合预期。词面请求 SHA-256 `e0df496db9477ca68ed9cdd62903a3bb78a2bbe98eadd98893fcd059d59e0dc3`。准备时探针源码 SHA 为 `7593c7db527fe1f872737899eeafee8697680c9765189183e4490b997bf8d922`，checker SHA 为 `aaa373dc9527cc42c5dbcf9e929c28fa05f8684cec5b7820fe4b8a03a49207f2`。后续真实词面与模型运行仍由根代理安排。

## 静态验证边界

准备阶段 `python -m unittest discover -s scripts/memory-lab -p test_bge_m3_shadow.py -v` 实际 9/9 通过；仅测试替代集合、完整/部分命中、RR@5、整来源预算、负例口径、范围控制、拒绝 v1、goal 格式、v2 的 11 条多证据分母，以及总体提升不能掩盖主题退步。Python 语法检查通过。当时本代理没有运行 Cargo，Rust 适配器尚未编译；没有新增依赖。后续由根代理挂接独立 bin（test=false）、编译并实际运行，结果见下一节。

运行报告新增每模型加载前、加载后、编码结束、卸载后的 Linux `/proc/self/status` VmRSS/VmHWM。VmHWM 是整个进程累计峰值，包含先前模型和运行时分配器，绝不称模型独占峰值。`quality_gate` 按冻结门槛显式列分子、分母、比较与是否通过；负例单列 `negative_quality_pass=null`、`abstention_tested=false`，候选覆盖门槛通过也不表示负例正确拒答或可默认部署。

## 实际结果与独立复核

根代理实际运行 `runtime/goal-baselines/20261104/bge-challenge-first/report.json`，exit 0、总墙钟 101.375 秒。报告 SHA-256 `b614aa9fcad3d12fb0ff6f4f3319eb0b73fbceac903a5aa967f7b76442a09cae`；真实 Rust 词面结果 `lexical-challenge-first.json` SHA-256 `f4ab9a5482f8ac5c9632bf3798c7803b37b2f448300c75cc7e602c6fd0f4cd3b`。方法、v2 manifest、资产和预分词绑定保持原样。v2 从此已经运行检索，后续发现标签问题只能另建新版本、保留本结果，不能回改。

独立审读直接从冻结标签和既有排名重新计算 576 行（3 方法 × 2 条件 × 96 问）的整来源预算、hit、MRR、替代集合完整覆盖和覆盖率，并核对全部四组 BGE 相对基线非退步门槛。全部与报告一致，12 个逻辑来源范围控制符合预期。派生核对摘要为 `runtime/goal-baselines/20261104/bge-challenge-audit-first.json`；没有调用探针的计分函数、运行新模型或生成新排名。所有方法/条件本轮均没有触发整条预算跳过，所以实际预算排名与原 top5 相同。

| 输入条件 | 方法 | 完整证据@3 /72 | 完整证据@5 /72 | 单一跨语完整@5 /24 | 多证据完整@5 /11 | MRR@5 | 负例返回候选 /24 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 当前问题 | 冻结词面 | 35 | 42 | 0 | 7 | 0.5720 | 22 |
| 当前问题 | E5 FP32 | 36 | 41 | 0 | 6 | 0.5646 | 24 |
| 当前问题 | BGE-M3 | 68 | 69 | 21 | 11 | 0.8843 | 24 |
| 问题 + goal | 冻结词面 | 35 | 40 | 1 | 6 | 0.5144 | 24 |
| 问题 + goal | E5 FP32 | 46 | 49 | 12 | 3 | 0.6683 | 24 |
| 问题 + goal | BGE-M3 | 58 | 66 | 20 | 9 | 0.8106 | 24 |

BGE 当前问题条件的 69/72、21/24、11/11 分别达到 65、20、10 的预注册数量门槛；相对 E5/冻结词面新增完整覆盖 28/27 条，没有损失任何原已完整覆盖的问题，各主题也没有完整覆盖回退。因此 `next_opt_in_candidate_gate_passed=true` 与计分一致。它只表示值得做下一片候选验证；本集是见过方法后编写的合成诊断，不能称盲评、真实用户准确率或默认部署验收。

BGE 问题加 goal 条件总体和跨语达到数量门槛，但多证据仅 9/11，低于 10，所以同一 gate 为 false。现有 `context.rs` 会把 goal 拼进召回查询，不能把当前问题条件的通过直接套到现有接线上。E5 加 goal 后 12 条跨语命中全部来自英文问题→中文来源，中文问题→英文来源仍为 0/12；这是加入中文 goal 后的具体排名变化，不能称 E5 已具备双向跨语能力。

两个向量方法在所有 24 个负例都返回候选；冻结词面当前问题只有 2 个零交集，加 goal 后也全部返回候选。探针没有接受阈值或回答生成，`negative_quality_pass=null` 正确，不能将零交集称可靠拒答，也不能将高余弦称原文足以回答。所有候选继续 `candidate_requires_relevance_review`。

### 主题、逐题回退与剩余缺口

各主题均为 12 个正例，以下为完整证据@5：

| 主题 | 词面当前 / +goal | E5 当前 / +goal | BGE 当前 / +goal |
| --- | ---: | ---: | ---: |
| 发芽 | 7 / 6 | 5 / 7 | 11 / 11 |
| 纸桥 | 6 / 5 | 7 / 8 | 12 / 12 |
| 分数 | 7 / 6 | 8 / 8 | 12 / 11 |
| 阅读 | 7 / 8 | 7 / 9 | 12 / 12 |
| 循环 | 8 / 8 | 7 / 9 | 12 / 10 |
| 蒸发 | 7 / 7 | 7 / 8 | 10 / 10 |

当前问题下 BGE 剩余 3 个失败都涉及跨语的提示依赖、独立回答范围：

- `germination_q06` 应找“无提示提出各盘起始种子数量相同”的 `germination_s06`，原文排第 25；前列混入同语的已接受计划和提示后回答。
- `evaporation_q06` 应找“独立建议平视凹液面，不代表此前已如此测量”的 `evaporation_s06`，排第 9。
- `evaporation_q08` 应找“初始读数是在教师提示后指出”的 `evaporation_s05`，排第 11；前列混入英文临时请求和其他任务回答。

这说明跨语覆盖显著改善，但模型仍可能把表面相关的计划、请求或回答排在真正支持问题的证据之前。当前 69/72 不是“教育状态理解完全正确”。没有为了这些题调整类别权重、同义词、阈值或标签。

加入 goal 后没有新增 BGE 完整覆盖，反而丢失 `fractions_q12`、`loops_q07`、`loops_q12`；分别把必需的 `fractions_s05`、`loops_s07`、`loops_s06` 挤到第 6。前两类中有“提示后与独立回答”的双证据对比，也有“已接受计划尚未运行”的状态问题。这是 goal 接线导致的实质回退，不应通过把 top5 临时改为 top6 掩盖。

与同条件基线比较，+goal BGE 相对 E5 丢失 `loops_q07`、`evaporation_q08`，相对词面丢失 `fractions_q12`、`loops_q12`；每主题各最多 1 条，故非退步门槛本身仍过，但多证据门槛失败。当前问题条件虽然没有完整覆盖回退，仍存在首位排序回退：相对 E5 的 `germination_q09`，相对词面的 `fractions_q10`、`loops_q12`；不能把完整覆盖无退步说成所有排名都改善。

冻结词面 +goal 有 76/96 问出现只与 goal 相交的已选候选，共 174 个位置；自身丢 4 条、增 2 条完整覆盖。E5 自身丢 4 条、增 12 条；BGE 自身丢 3 条、增 0 条。完整清单在报告的 `goal_comparisons`，没有只呈现增加项。tag `multiple_evidence` 仍有 12 条，因为 `loops_q11` 存在多来源替代；真正必须多来源的 mode 分母为 11，质量门槛正确按 mode 计算。

### 资源与耗时的实际边界

| 项目 | E5 FP32 | BGE-M3 FP32 |
| --- | ---: | ---: |
| 原生模型参数数 | 117,653,760 | 567,754,752 |
| 报告加载耗时 | 8071.60 ms | 223.23 ms |
| 252 项编码累计 | 3420.49 ms | 32598.20 ms |
| 单项算术平均 | 13.57 ms | 129.36 ms |
| 加载前进程 RSS | 0.902 GiB | 0.955 GiB |
| 加载后进程 RSS | 0.939 GiB | 0.958 GiB |
| 编码结束进程 RSS | 1.084 GiB | 2.159 GiB |
| 卸载后进程 RSS | 0.953 GiB | 0.966 GiB |

以上是单次固定顺序、CPU FP32、2/1 线程、短输入 10–68 token 的离线事实，不是 p95、冷启动或线上实时承诺。模型加载可能受 mmap/惰性触页影响，固定小词表并不触及全部 embedding 页；E5 先运行，还可能承担首次导入/初始化成本。BGE 的 223 ms 绝不能解释为冷启动完整读入 2.27 GB、其资源成本低于 E5。全程序 101.375 秒包含准备、校验、导入、计分和写盘等，不能与仅编码累计直接混用。

原 `/proc/self/status` 采样还存在小幅非单调：E5 VmHWM 从 1,163,669,504 降至 1,160,761,344 bytes；BGE 从 2,317,963,264 降至 2,315,280,384 bytes。保留原报告，不“修正”为漂亮峰值。本轮只说**最大已观测进程 RSS 为 2,317,963,264 bytes，约 2.159 GiB**；不称精确单调高水位、模型独占峰值或未来最大常驻预算，原先 4–8 GiB 仍只是准备阶段估计。

## 建议的下一片（尚未实施）

1. 先做不调用新模型的合成 Store 接线验证：复用本次已冻结向量，给现有向量旁表增加明确的 BGE 1024 维 profile，保留 E5 384 维 profile。绑定模型/权重/tokenizer/pooling/前缀/维度到 model_key，禁止跨 profile 读取或计算，继续 source digest、版本、learner epoch 的 CAS 与删除失效。当前 `store/embeddings.rs` 直接引入 `memory_embedding::DIM` 并要求 384，不能只换权重文件或全局改 DIM。
2. 保留当前问题为 BGE 查询输入，goal 作为单独教学上下文；任何合并候选策略都需另立方法和报告，不继承这次 pure-dense 的通过结论。现 `Store::recall_candidates` 用旧 `Store::recall` 结果与 dense top5 做 RRF，它不是本探针的冻结词面，更不是本次 BGE 单独排名。接线时要独立比较实际 Store 返回、范围过滤、预算及来源失效；本次原始 60 来源实验不等于实际慢记忆 Store 已通过。
3. 再验证候选相关性和不足证据处理：保留原文与类型，让既有受限教学链明确区分“原文支持”“只部分支持”“没有支持”；重点检查 24 负例、3 个跨语失配、提示依赖和未执行计划。生成结论必须绑定原来源，不能从相似度得到可信度、长期偏好或完成状态。通过本集仍需未看模型输出的新题审阅/实际任务评价；不把同一 v2 调参后称新的 holdout。

本次只更新说明与独立核对摘要；没有修改冻结方法、v2、探针、原始报告或生产代码，没有新增模型运行。
