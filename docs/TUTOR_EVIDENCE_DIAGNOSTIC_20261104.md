# 教学回复的记忆证据充分性诊断

`tutor-evidence-probe` 是独立的合成快照回放入口。它使用已冻结 `typed48-store-first` 的真实 Store 当前问题/纯 BGE 前五结果，验证候选进入教学上下文后，模型是否正确区分相关证据、检索遗漏与没有记录。它不重新检索或编码，不打开学习者数据库，不显示、播放或应用回复中的更新候选，也不改变生产默认召回。

本轮快照 SHA 为 `b058a6922b0790090ea8003607ca8be1ed5a3803952c1a3552e55c542d54d6bd`；原 challenge v2 manifest SHA 为 `858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81`。来源、查询、标签和范围控制文件仍按原 hash 读取，旧数据与结果不修改。

## 实际执行与发现

2026-11-04，先在原策略下运行固定批次1的六题（`tutor-evidence-first`），随后给生产 `prompts/tutor_core.md` 添加三项通用证据规则：匹配当前活动、逐项支持历史断言、缺证据时说明边界。新策略另行 prepare 到 `tutor-evidence-grounding-first`，以独立 manifest 运行全部36题；新旧请求除随机身份和当前事件时间外逐字段一致。原策略另30题未运行，不能写成36题前后对照；也不能把只重复的六题作为独立样本叠加成功率。

新策略36题均完成结构验证，无重试，六批 Provider close 返回成功且 running=0；未投递、未应用候选。共出现0个记忆候选、1个任务候选。开放逐例审阅发现四类问题：

| 案例 | 实际问题 | 判定边界 |
|---|---|---|
| germination_q06 | 植物任务缺少所需来源，却引用纸桥的垫圈/跨度记录回答 | 严重活动错配；原策略和新增规则后都发生，合法引用不能保证相关性 |
| fractions_q16 | 正确表示不知蜂蜜数据，却补充“两杯都没有称重”；只有E杯明确未称重 | 无依据的附加否定历史，不等同于主要问题答错，也不能算整条通过 |
| loops_q07 | 正文准确表示重跑尚未执行，却建议把旧计划写入当前 reported_result | 不合适的任务候选；生产计划来源护栏静态上会拒绝，但本诊断未执行apply，不能报告实际拦截 |
| evaporation_q06 | 条件推理“不能证明以前每次都这样”正确，但未明确说明缺少具体独立读法的记录 | 回答/证据缺口说明不完整；“即使”是条件句，不判为编造已发生事实 |

其余案例的回答/引用边界在本次已知方法的 agent 审阅中通过。九个具有完整来源的正题均答对主要内容，但上述 loops_q07 的附加候选不通过；24个原负题有1条附加断言不通过；三个检索缺证据正题有严重错配1条、缺口说明不完整1条。逐例审阅及根侧确认留在单独的新 JSON，原请求、标签和结果不回写。它不是人类或盲评，也不是整体教育质量估计。

**语义召回仍未默认启用。** 新规则没有解决首个严重错配。后续代码采用真实来源活动编号限定候选；这36个冻结请求没有显式选择历史活动，其结果不能被新代码追认通过。新的活动关联必须用新方法验证，不可依据六个主题标签过滤旧输入后改成绩。

原策略 manifest SHA：`5b7bf964f70a4edc231ddb8eb19ebd37aafd02af1a1f58a6f168560443b029af`；新策略：`fc8e7900297a7f566dfdd430bbb8a3ff2073e259d7139bcf6179b9aa7081c4d0`。本次实测 probe 二进制保存到 `checkpoint15-measured-binaries`，与后续活动隔离代码的构建区分。

## 固定案例与输入

36 个案例分为六个固定主题批次，每批先两道正题、再该主题全部四道负题。完整顺序在 `scripts/memory-lab/tutor_evidence_method_v1.json`，不是根据新模型输出临时选题。首轮先执行批次 1，审阅质量及输入真实性后，再由根代理决定推进其余批次。

| 批次 | 两个正题 | 另四个负题 |
|---|---|---|
| 1 germination | q06、q07 | q13–q16 |
| 2 paper_bridge | q02、q11 | q13–q16 |
| 3 fractions | q08、q12 | q13–q16 |
| 4 reading | q01、q09 | q13–q16 |
| 5 loops | q07、q12 | q13–q16 |
| 6 evaporation | q06、q08 | q13–q16 |

24 个原标签负题均保留。12 个正题中，9 个在当前候选中具有一个完整原最小证据集合；`germination_q06`、`evaporation_q06`、`evaporation_q08` 是原数据可回答、但本次检索遗漏所需来源的三题。它们仍是正标签，单列为“当前上下文证据不足”，不能改成数据集负题。这十二题包含六道跨语和三道多来源题，以及条件、计划尚未执行、提示依赖、数值和更正边界。

模型请求只含原问题、原附带目标、已选五条记忆及其来源引用。正文逐字、原序、整条最多 2400 Unicode 字符。保留真实快照的 opaque memory/event ID、同一合成 owner、版本、source type、education_metadata/support_level；每题另生成 request/session/current-question event，旧来源均标 historical。`evidence` 使用生产形状的 `content_reference` 指向实际选中记忆，不额外加入原始完整来源或未召回内容。

目标只放 `task.goal`，`goal_source=synthetic_fixture_context`，`field_evidence` 为空。它是数据集附带教学背景，不是新增学习者陈述、传感器事实或已完成动作；没有虚构来源 ID。`user_input` 不加前后说明。英语问题输出语言为 en，中文和 mixed 问题为 zh。原查询语言与 mode 留在本地审阅记录。

答案、最小证据集、claim_checks、forbidden_claims、source_id 别名、相似度和评审要求均不送模型。`requests.json` 里的 query_id 是本地包装键，调用只传其中的 `request` 对象。原标签与映射存于单独 `review-cases.json`。

## 运行边界

prepare-only 只校验文件、构造请求并调用纯函数 `build_prompt` 检查生产提示预算；不构造 Provider、不 probe、不进行远程请求。它创建四个新文件：`method.json`、`requests.json`、`review-cases.json`、`manifest.json`，拒绝覆盖目录。manifest 绑定编译源码、方法、全部请求/审阅记录及当时生产 policy/schema hash。

真实模式要求显式 `--real-synthetic`、已检查的 manifest SHA 和一个 case/batch/all 选择器。复用 `KimiProvider::probe → generate → close`，使用当前生产 `tutor_core.md` 与 TutorReply schema、既有无工具核验、隔离目录、请求 gate、输出上限与完整回复验证，不另开模型运行时。

现有 Provider audit 仅报告版本、binary/agent SHA 与空 tools/subagents，**没有独立报告或固定模型 alias/id**。此实验限于运行时既有已核验 Provider 配置，不声称精确远端模型身份或采样参数已冻结。程序不读取 auth、不复制凭证 config；此限制不能用客户端二进制 hash 代替。

每题在 generate 前写并同步 `attempt-QUERY.json`，只用 create_new。存在 attempt 就拒绝再次提交，包括上次中断、尚无 result 的情况；不自动重试。每个成功结果保存完整合成 validated TutorReply、候选与引用检查，不保存 thought 或原始 CLI 流。Provider/协议失败保存错误并停止当前批次，后续未执行题可显式单题选择；失败题不能在原准备目录自动重试。

批次结束或正常 Result 错误均走 Provider close，summary 保留 close 结果和 running count。它不是另行验证的远端取消 ACK 或每个进程的清理证明。退出 0 仅表明该选择范围完成结构验证/收尾，不表示语义通过。尚未执行的批次不计通过。

## 审阅与决策

生产 `validate_reply` 只验证引用 ID 属于本轮可用来源，不能证明引用内容支持回答。因此自动结果仅记录结构、引用集合覆盖、候选数量与收尾，所有 `semantic_quality_pass` 保持 null。

每题需审读回答与所有 task/memory candidates，核对原标签、实际可见内容和引用：历史是否有依据、单位/实体/数值是否正确、是否混淆提示与独立、计划与执行、更正与新增试验、短期要求与持久偏好。合法 ID、相似度或 `needs_clarification=true` 都不是语义成功条件。对“没有相关记录”的解释可以引用相近但不足的记录，不强制空引用；引用当前问题也不使问题中的预设成为过去事实。

首片预设门槛：24 个原负题加三个检索不足题，共 27 题不得编造历史或更新事实，并说明实际证据边界；九个充分题都应给出正确完整回答并引用一个原完整最小集合，不能靠一律拒答过关。任何严重的无依据历史/持久更新阻止默认启用。即便全 36 题逐项审阅通过，也只支持下一步受控影子，不等于原 96 题、现场儿童、长期记忆或线上安全全面通过。审阅来源应如实标记为 agent 对合成、已知方法数据的审读，不能称人工或盲评。

**保留四个准备文件不变。** 实际语义审阅另存新的 `semantic-review.json` 或 Markdown，不往冻结的 `review-cases.json` 回写，否则 manifest 校验将正确阻止后续批次。逐题保留意见和未决项，不先汇总成未经审阅的成功。

## 根代理命令与冻结状态

```powershell
cargo test -p ai-tutor --bin tutor-evidence-probe
cargo build -p ai-tutor --release --bin tutor-evidence-probe
target/release/tutor-evidence-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --output runtime/goal-baselines/20261104/tutor-evidence-first
Get-FileHash -Algorithm SHA256 runtime/goal-baselines/20261104/tutor-evidence-first/manifest.json
```

检查 36 请求与方法后，用真实 manifest hash 替换 `APPROVED_SHA`；先六题批次 1，不立即跑全部：

```powershell
target/release/tutor-evidence-probe.exe --root 'C:\Users\RBMS\Desktop\AI TUTOR' --prepared runtime/goal-baselines/20261104/tutor-evidence-first --prepared-manifest-sha256 APPROVED_SHA --real-synthetic --batch 1
```

五项无模型合同覆盖：完整 24/9/3 分层；只传选中正文/来源/版本、不泄漏标签与未召回原文；合法引用不伪作语义成功；固定单题/批次边界；prepare-only 与拒绝覆盖。测试读取固定本机合成快照，不需要模型、网络或学习者 DB。

| 文件 | 冻结 SHA256 |
|---|---|
| tutor_evidence_probe.rs | `62fe34bc6119b9849084c3c5a85ffc117d5fcd6cde356c4fb76ce8d31b68dad7` |
| tutor_evidence_method_v1.json | `90b6c9dbb1a1006d9ada74e11285d9ecac6e8b1dc70b0bdd05d2d25155f3861d` |
| tutor_evidence_tests.rs | `56ce038b0d415b2f0e954b10ebbcbfc060ac5635e73df8882e00cb580e77b025` |

实现初期由子代理完成源文件、方法和静态快照核对，根侧随后完成编译、5项合同测试、prepare及上述实际调用。注册备份在 `runtime/goal-backups/20261104/architecture_audit/tutor-evidence-registration/Cargo.toml`。probe本身不修改生产 Kimi、Store、上下文、旧实验方法或原标签；本片另行发生的策略和活动隔离改动分别记录，不能混为冻结probe的原始行为。
