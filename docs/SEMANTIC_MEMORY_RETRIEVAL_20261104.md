# 中英教育记忆：本地语义检索研究与独立测量入口

状态（2026-11-04）：已完成官方来源核对、固定文件下载及 SHA256 复核；root 已运行独立 Rust CPU 评测，见下方实测结果。本阶段未修改生产 `Store::recall` / `build_context`，未读取真实学习者数据库，未发起远程模型调用。不能据此声称两个跨语缺口已在产品中修复。

## 选择与来源

选择作者仓 `intfloat/multilingual-e5-small` 的现成量化 ONNX，避免新增 Python 推理服务或另建向量数据库。模型为 12 层、384 维，多语检索用固定 `query: ` / `passage: ` 前缀，包括中文；对 attention mask 内 token 做均值池化，再 L2 归一化。512 token 是上限。本实现遇到超长输入报错，不默默截断教育证据。模型卡标记 MIT。[作者固定版本模型卡](https://huggingface.co/intfloat/multilingual-e5-small/raw/614241f622f53c4eeff9890bdc4f31cfecc418b3/README.md)

量化文件由 HF 的 tomaarsen 使用 Sentence Transformers 导出，intfloat 于 2025-02-17 合并。公布的配置是动态 QOperator / IntegerOps、U8 激活、S8 权重、per-channel、无 reduce-range；这是作者仓中可追溯的转换，不是另找同名第三方文件。[转换及合并记录](https://huggingface.co/intfloat/multilingual-e5-small/discussions/12)

锁定 revision：`614241f622f53c4eeff9890bdc4f31cfecc418b3`。以下大小与大文件 LFS SHA 来自固定版本 API，并与本地文件复核。[作者仓固定版本文件元数据](https://huggingface.co/api/models/intfloat/multilingual-e5-small/revision/614241f622f53c4eeff9890bdc4f31cfecc418b3?blobs=true)

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `onnx/model_qint8_avx512_vnni.onnx` | 118346824 | `dd476dd0c2514e9b9be83aeb3853fac0763e0bdf4a71645407587d77c48a2d88` |
| `tokenizer.json` | 17082730 | `0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39` |
| `config.json` | 655 | `69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959` |
| `tokenizer_config.json` | 443 | `a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b` |
| `special_tokens_map.json` | 167 | `d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7` |
| `1_Pooling/config.json` | 200 | `987f7a67a38fa564c849bb5d277c52ab9088a84368fc0be31a354125aebb12a0` |

本地目录 `models/memory/e5-small` 保存模型卡、配置和 `download-manifest.json`，来源元数据在 `runtime/goal-baselines/20261104/e5-source-metadata.json`。小文件 SHA 是下载后计算值，不冒充上游独立 SHA；两个大文件有上游 LFS SHA。下载脚本的 curl 直连不可达后，使用系统 HTTPS 代理通过 PowerShell 获取相同官方 URL，先 `.part`，核验再改为最终文件名。未安装大框架，未下载约 470 MB 的 FP32 对照模型。

Rust 复用 `ort = 2.0.0-rc.10`；该 crate 为 MIT OR Apache-2.0，ONNX Runtime 为 MIT。root 新增并锁定 `tokenizers = 0.22.2`，关闭默认特性，仅启用 `fancy-regex`，许可证 Apache-2.0。`encode` 单条本地调用，不启用 tokenizer 的网络下载特性，也不使用批量编码线程池。[tokenizers 官方 Cargo 配置](https://raw.githubusercontent.com/huggingface/tokenizers/v0.22.2/tokenizers/Cargo.toml)、[ort crate](https://docs.rs/ort/2.0.0-rc.10/ort/)、[ONNX Runtime 许可证](https://github.com/microsoft/onnxruntime/blob/main/LICENSE)

## 本机兼容性边界

本机 CIM：Intel Core i5-14600K，14 核 / 20 逻辑线程，x64。现有 `models/native/onnxruntime.dll` 文件版本为 `1.23.20251021.3.a83fc4d`，SHA256 为 `2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492`。`ort-sys rc10` 请求 C API 22；其加载器接受较高 runtime minor 并提示兼容性风险，再获取 API 22。独立 probe 保留这一已在项目内使用的 DLL，不偷偷替换视觉运行库。

文件名中的 `avx512_vnni` 说明量化预设，不能直接当作 CPU 必须具备 AVX512 的结论。标准量化算子由 Runtime 选择内核；官方 MLAS 源码分别按 AVX2、AVX-VNNI 和 AVX512 能力分派。由此推断该模型值得在本机 CPU EP 实测，但不据源码推断宣称已经跑通。[ONNX Runtime v1.23 MLAS 分派源码](https://raw.githubusercontent.com/microsoft/onnxruntime/v1.23.0/onnxruntime/core/mlas/lib/platform.cpp)

U8S8 在部分非 VNNI AVX2/AVX512 机器上存在饱和导致质量下降的可能；官方给出 reduce-range 或 U8U8 的替代路径。故 probe 输出实际 CPU 能力、运行库构建信息、图输入输出和排名，不能把“成功加载”或小样例检索正确等同于 FP32 一致性。[ONNX Runtime 量化文档](https://onnxruntime.ai/docs/performance/model-optimizations/quantization.html)

CPU EP 默认 2 个 intra-op 线程、1 个 inter-op 线程，可以 1 intra-op 对照；不自动占满 20 线程。未来生产接线须复用同一进程的 ORT 初始化和 DLL 路径，使用独立 embedding Session，不能借用可变的视觉 Session。

## 已实现的独立测量

入口：`rust/ai-tutor/src/bin/e5-embedding-probe.rs`。fixture：`rust/ai-tutor/tests/fixtures/e5_retrieval_probe_v1.json`。

```text
e5-embedding-probe --local-synthetic --root <项目根> --threads 2 --repeats 5
e5-embedding-probe --local-synthetic --root <项目根> --threads 1 --repeats 5
```

执行前验证模型、tokenizer、DLL 的固定 SHA。每次只建立临时 simulator DB，真实调用现有 `Store::recall` 作为词法基线，不复制一个简化版本。文档向量一次计算，查询向量重复测量；无远程调用、无设备、无自动启用检索。输出如下：

- 审计、tokenizer / Session 加载总时间；首条文档的冷推理；热查询的 tokenize、inference、total 的均值 / P50 / P95 / 最大值。
- 词法、dense cosine 和等权 RRF（k=60）的每条排名；正例 hit@1、hit@3、MRR。
- 10 个合成用例：原始两个纯跨语缺口、2 个单语对照、2 个新跨语待做计划、数字与更正的难负例各 1 个、完全无关查询 2 个。未硬编码翻译词表或重写问题。
- 无关用例仍报告顶部相似度，不把有排名误写成正确拒绝；目前没有校准接受阈值。RRF 也是对照策略，不是已选定的生产排序。

这些热延迟仅指 embedding 路径，不是完整 HTTP / 生成响应延迟；本轮不测视觉/音频共载争用或峰值 RSS。小样本的 hit@3 在 3 个候选上尤其宽松，判断应优先看 hit@1、逐例排名和无关查询分数。语义相似度不能把数字、否定、更正或待做计划改成已完成事实。

新增 3 个纯单元测试，root 的 `workspace-sixth.log` 已记录 3/3 通过：

- `attention_mask_pooling_excludes_padding_and_normalizes`
- `rank_fusion_keeps_semantic_only_candidate_without_inventing_lexical_overlap`
- `original_cross_language_questions_and_negative_controls_are_preserved`

## 首次 CPU 实测及精度复核

root 执行的 `runtime/goal-baselines/20261104/e5-embedding-second.log` 记录固定 INT8 模型在现有 DLL 上成功推理。CPU 检测为 AVX2=true、AVX-VNNI=true、AVX512=false；Runtime 构建为 a83fc4d58c，绑定 API 22。2 intra-op / 1 inter-op 下，包含 SHA 审计的加载总计约 815 ms；50 条短查询（10 个问题各 5 次）的 embedding total P50 约 2.84 ms、P95 约 3.29 ms。仅描述本次短文本负载，不外推到满 512 token 或音视频并行运行。

8 个正例的词法 hit@1=3/8、MRR=0.375；dense 与等权 RRF 均 hit@1=7/8、MRR=0.9375。原始中问英忆命中 rank 1；英问中忆的正确记忆仍仅 rank 2（0.7722），待做计划误排 rank 1（0.7745）。因此只能说候选覆盖有所改善，不能写两个原始缺口都已正确排名第一。

无关中文木星问题给 `zh_plan` 高达 0.8283，高于一些相关跨语结果；无关英文问题的顶部也达 0.7950。当前 RRF 无拒绝策略，旧词法还因常见英文字符二元组误收无关英文记忆。单一未经独立校准的余弦阈值不能保证既保留跨语证据又拒绝这些无关项。模型卡也说明 E5 的绝对余弦集中在较高区间，排名比绝对值更有意义；这不能替代产品对“不召回”的要求。[作者模型卡 FAQ](https://huggingface.co/intfloat/multilingual-e5-small/raw/614241f622f53c4eeff9890bdc4f31cfecc418b3/README.md)

只读复核未发现明显的计算契约偏差：tokenizer.json 自带单段 `<s>` / `</s>` 模板和 type_id=0，encode 开启特殊 token；query / passage 前缀正确；单条无 padding，仍使用 attention mask；池化包括前缀和特殊 token，再 L2，与作者示例一致。运行图输出为未池化的 `[1, sequence, 384]`，未发生双重池化。INT8 的浮点输出并不代表其内部计算等于 FP32。

root 已授权下载同 revision 的作者 `onnx/model.onnx`（470268510 B；SHA256 `ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665`），并扩展独立 probe 作相同输入、tokenizer、池化与 CPU EP 对照。上游 `onnx/tokenizer.json` 与本地 tokenizer SHA 一致。

```text
e5-embedding-probe --local-synthetic --root <项目根> --threads 2 --repeats 5 --precision compare
```

默认仍是 `--precision int8`；可单独指定 `fp32`。`compare` 先运行 INT8，释放 Session 后再运行 FP32，避免同时保留两份推理 Session。两侧均进行固定 SHA 核验，使用同一个 tokenizer 文件和固定 fixture。输出完整两份排名/时延报告，并为 10 文档 + 10 查询逐项报告两种精度的向量 cosine、分量 RMSE、最大绝对分量误差及汇总分布；另逐查询报告排名变化、top1 变化及相同候选的 cosine 差值。结果不持久化原始向量。FP32 在同一进程第二个加载，冷启动时间不是随机顺序的严谨对照。

增加 `precision_metrics_compare_the_same_embedding_coordinates`、`precision_comparison_rejects_different_tokenizer_provenance` 两项纯单元测试，等待 root 统一运行。原 INT8 日志、文件及下载 manifest 保留。下载完成后另写 `fp32-download-manifest.json`，不覆盖 INT8 来源证明。

FP32 下载已完成并核验上述字节数与上游 LFS SHA，独立 manifest 已写入。root 已运行 `runtime/goal-baselines/20261104/e5-precision-first.log`：20 个同文向量的 INT8/FP32 cosine 均值 0.996396、最小值 0.995333；10 个查询 top1 变化为 0。FP32 与 INT8 都为 7/8 正例 hit@1，保留英问中忆 rank 2 与相同无关高分问题：FP32 无关英文顶部 0.793589，无关中文顶部 0.825036。FP32 的 50 条短查询 P95 约 5.88 ms。

因此，替换 FP32 不能修复此次发现的检索判别问题；现象不能仅归因于 INT8。高同文向量相似不证明所有输入量化等价，小样本也不足以选择生产接受阈值。默认检索继续不变。

## 测量后最小生产接线方向

只拆两个模块：本地 `memory_embedding` 负责 tokenize / ORT / pooling；Store 子模块负责向量来源快照与有效性。模型 key 应同时包含作者 revision、量化文件 SHA、tokenizer SHA、池化版本。返回 384 维标准化向量，不生成或改写记忆文本。不引入一般向量数据库或 LLM 检索框架。

建议窄接口：

```text
Embedder::embed_query(text, cancellation) -> vector + model_key
Embedder::embed_document(text, cancellation) -> vector + model_key
Store::embedding_work(lid, limit) -> Vec<snapshot>
Store::commit_embedding(snapshot, vector) -> inserted | stale
Store::eligible_embeddings(lid, model_key) -> 当前有效 memory ID + vector
```

新增一个旁表即可：按 namespace / learner_id / memory_id / model_key 唯一，保存 learner_epoch、记忆内容 digest、排序后的来源 event_id / version digest、dimension 和 384 个 little-endian f32（每条向量 1536 B）。不复制原文；digest 是完整性关联而不是匿名化承诺。索引和向量仍是用户派生数据，随删除清除。

取任务快照在 Store 短锁内；本地推理在锁外；写入同一事务重新验证 namespace、owner、epoch、记忆 active、内容 digest、每条 `memory_evidence` 的 active 来源及版本。过期结果返回 stale。更正、删除、来源失效与记忆 supersede 同事务删除对应向量；排队任务和已计算待写结果会因 epoch / digest / 来源版本变化而失效。重开数据库不盲目扫描旧事件重建已删除内容。确需索引回填时只选择当前有效记忆，不能复活无效来源链接。

查询先取本人 namespace 内的当前活动有效记忆，不使用 UI 的 LIMIT 100。只计算通过来源与版本核验的候选向量余弦，按候选 ID 合并词法结果；写入上下文前再通过现有 `memory_evidence` 核对一次。语义向量不能绕过真实来源检验、跨档案、跨 real/simulator 或扩大当前 `modular_rover` 活动范围。保留现有最多 5 条 / 2400 字符预算，以及明确偏好与待做实验的类型政策。

规模较小时，owner 过滤后的精确余弦 O(N×384) 足够，不需要 ANN 服务。未索引、缺模型、损坏文件、超长输入、取消或推理失败均继续使用现有词法检索；不要在前台临时下载模型。后台索引每次一条、低线程；前台输入应使后台 ORT RunOptions terminate，候选计算与最终提交均检查取消 / 版本。前台 query 的超时和并发额度须按本机测量决定，不能用未测的数字写成 SLA。

默认检索策略继续不变。下一门槛是：两处跨语缺口在本地模型可重现命中；扩大独立中英与混合语料，包含数字、否定、计划、临时偏好、无关问题；与词法、dense、hybrid 分开报告 Recall@1/3、MRR、负例误收率和延迟。接受阈值必须在独立负例上校准，不能围绕这 10 条调参。再加入跨 owner、namespace、删除、更正、源版本与 101 条以上的集成回归，之后才接入 shadow 检索比较。当前未证明 FP32 等价、广泛教育语义准确或生产共载稳定。

## 已实现的独立基础模块，未接默认 context

`src/memory_embedding.rs` 从 probe 抽出唯一的 E5 实现，probe 已改用它，没有复制第二份 tokenizer / ORT / pooling。`E5Embedder::load(root, threads, Precision)` 固定文件 SHA、最多 2 个 CPU 线程，常驻 tokenizer / Session；`embed_query` / `embed_document` 接受可选 ORT `RunOptions`，调用者可在另一线程 terminate。本地推理须在 Store 锁外运行。模型 key 对 revision、模型/tokenizer SHA、前缀、池化版本和维度作哈希，INT8 与 FP32 不混缓存。

新增 `src/store/embeddings.rs`，只增加一个 `memory_embeddings` 旁表，存 namespace / owner / memory / model key、learner epoch、记忆内容及元数据 digest、排序后的源 ID/版本 digest、1536 B 向量。无重复原文、无自动历史回填、无 scheduler 或额外模型调用。实际接口是：

```text
embedding_work(lid, model_key, limit 1..32) -> Vec<MemoryEmbeddingSnapshot>
commit_memory_embedding(snapshot, model_key, vector) -> bool（false = 已过期）
eligible_memory_embeddings(lid, model_key) -> 当前有效向量及来源快照
recall_candidates(lid, query_text, Option<QueryEmbedding>, top_k) -> Vec<RecallCandidate>
```

快照在短 Store 调用内取得；推理在锁外；commit 在同一事务重核 owner、namespace、epoch、当前记忆 digest、来源版本 digest 和完整 `memory_evidence` 后写入。记忆删除/更新、源删除/版本变化、来源链接变化、学习者 epoch/删除和来源 session 删除触发清除向量。读取仍实时核验，而不信任触发器或旧缓存；过期内容和已删除来源不能因重开而自动重建。

scope 沿用当前慢记忆边界：仅 `modular_rover` 的 current_activity，或 help_preference 的 cross_session_explicit；current_session 临时偏好及其他活动不参与索引与候选召回。索引遍历不使用 UI LIMIT 100。候选接口先取现有词法结果，再把有效向量的精确余弦 top5 以 RRF 合并，保留 5 条 / 2400 字符预算；无有效 query 向量、缓存缺失或过期时保持词法结果。排序和最终来源核验在一个短事务里完成，事务中不调用模型。

`RecallCandidate` 仅有 memory、lexical_rank、semantic_rank、cosine、ranking_score，明确 `relevance_status=candidate_requires_relevance_review`；不输出 confidence，不修改 source_grade，不把候选写回事实。无关问题仍可能得到高分候选，这是调用层必须解决的相关性判别问题。该接口没有接入 `build_context`、server 或默认 `Store::recall`。

新增 `tests/memory_embeddings.rs` 的 8 项临时数据库回归（无模型），等待 root 统一验证：旧 schema 增量迁移与重开、owner/namespace/epoch/model/content CAS、缺失/损坏/过期词法回退、更正删除后迟到提交、源版本/链接/内容及档案活动删除、scope 不扩大、102 条索引、最高余弦仍仅待核候选且默认召回不变。共享模块保留原池化单元测试；probe 的其余 4 项测试继续保留。

根代理于 02:11 完成验证：上述测试纳入 `workspace-eighth.log` / `workspace-ninth.log`，完整结果均 457 通过、0 失败、2 忽略。所有原生 release 构建成功；抽取共享编码模块后重新运行真实本机 `e5-precision-second.log`，10 查询 top1 变化仍为 0，原有无关查询高分问题保留。默认召回和后台自动索引未启用，这些通过项只覆盖当前已实现的基础模块。
