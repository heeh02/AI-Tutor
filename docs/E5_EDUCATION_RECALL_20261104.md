# 教育记忆召回：固定扩展评测

状态：已完成真实本地INT8/FP32同集评测，跨语召回未通过。默认召回保持原有行为。

输入为 `rust/ai-tutor/tests/fixtures/e5_education_recall_v2.json`，SHA256 `0392be52062844b0dd0e558c500d5afe9c4f38b1cad923a6092e5fda125f89d7`。每个问题使用相同16条合成记录，共25问：21个有相关记录的问题，4个无对应证据的对照。包含简单措辞、专业措辞、中英混用、20/30厘米、已接受但未做的计划、提示前后、控制条件、仅本次偏好与不确定性。

标签在首次运行前由实现者编写，未经过独立盲评；这些不是各年龄段真实学习者语料，也不能证明学习收益。临时看图要求只是用于本次检索实验的主题记录，不是允许生产系统将其升为长期偏好。

增强后的 `e5-embedding-probe` 保留原默认10问，并支持 `--fixture`。每个问题创建独立模拟档案，保存带版本来源、按真实Store接口提交向量、重新校验有效证据，然后比较原词面召回、纯向量、探针对照RRF及实际Store RRF。计划输入使用plan来源和pending_experiment记忆类型。没有生产档案、网络、模型下载或默认召回切换。

报告每一问的排名、来源等级、记忆原文、相似度与候选状态，以及整体和单问时延。多事实问题同时报告命中和前三条覆盖比例，避免找到一半就当完整回答。无相关记录的问题独立统计仍返回候选的数量；没有调过阈值，更不能用高相似度代替事实可信度。所有输入与结果保留，不依本轮结果删难例或据此宣称独立泛化通过。

运行入口（完成当前源代码统一构建后）：

```powershell
target/release/e5-embedding-probe.exe --local-synthetic --root . --threads 2 --repeats 3 --precision int8 --fixture rust/ai-tutor/tests/fixtures/e5_education_recall_v2.json
```

原10问的历史结果与本轮不混算：本轮新增了真实向量缓存和来源路径，旧式未分类来源也改为明确observation；原查询、正文和标签未改。

## 实测与报告修订

`runtime/goal-baselines/20261104/e5-education-first/report.json`保留首次全部结果。只读审查发现三个报告问题：来源字段误读为不存在的source_grade，MRR混用不同排名窗口，q01实际是英文问英文但分组标签写成跨语。首次报告没有回写。

第二轮 `e5-education-second/report.json`使用同一冻结输入和SHA，在同进程依次加载INT8、FP32；使用真实source_type、统一MRR@5、单独完整排名MRR，并记录同分组、排序规则和q01标签勘误。原始分组和标签仍保留。查询时延明确仅计向量编码，Store召回和证据校验另计，不能拿编码时延作为完整教学响应。

| 方法 | INT8首位命中/21 | INT8前三命中/21 | FP32首位命中/21 | FP32前三命中/21 |
|---|---:|---:|---:|---:|
| 原词面召回 | 2 | 4 | 2 | 4 |
| 纯向量 | 3 | 4 | 2 | 4 |
| 探针对照RRF | 3 | 5 | 3 | 4 |
| 实际Store RRF | 2 | 5 | 3 | 4 |

双事实问题必须都覆盖才算完整：INT8纯向量前三完整覆盖仅2/21，Store为3/21；FP32均为2/21。4个无证据问题中，向量和两种RRF均仍返回候选，词面召回也有3个返回。这里没有自动判为相关或作为事实应用，不能宣称拒答或相关性门控已完成。

16个纯单证据跨语问题在首次INT8中均未命中前三，正确记录多排在第7至9位；同语言干扰记录显著靠前。41个文本的INT8/FP32向量余弦最小0.99544、均值0.99666，25问有2个纯向量top1改变；FP32并未解决问题。因此不能将本次失败简单归因于量化。

首次25问中17问存在精确RRF同分，9问的离线与Store前五顺序或集合不同，但所有对应候选分数一致。离线按fixture ID排同分，Store按持久记忆UUID；重建模拟库会改变后者。首次两者总体MRR的差异可由q10同分顺序解释，不能当作缓存或来源过滤损害质量。第二轮明确暴露这一边界，没有用正确标签挑选排序规则。

## 独立前端核对

`scripts/memory-lab/e5_python_reference.py`在已有WSL环境以离线AutoTokenizer和FP32 ONNX Runtime CPU运行同一41条文本，没有下载或修改模型。`e5-python-reference/preflight.json`核对HF包装器与原始tokenizer.json的IDs、mask、type IDs全一致，25问token数量也与Rust一致。`report.json`保存全部输入、384维输出向量、hidden状态形状/摘要与少量坐标。

`e5-python-reference/rust-fp32-comparison.json`显示：25问全部16候选排名与Rust FP32逐项一致，400对余弦分数最大绝对差约9.16×10⁻⁸、均值约2.01×10⁻⁸。这不支持Rust调用、池化或Windows运行平台是本次失败原因。

随后从同一官方revision下载唯一缺少的`model.safetensors`，470,641,600字节，SHA256 `1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477`；精确来源、MIT许可和本地README链保存在`e5-native-reference/download.json`。没有下载另一份bin权重或安装新依赖。`scripts/memory-lab/e5_torch_reference.py`在已有环境以FP32 CPU、2/1线程、eval/inference_mode和本地模型执行41条固定输入，199/199项权重加载完成。

`e5-native-reference/report.json`中，原生PyTorch与Python ONNX的25组完整排名再次全部一致，41条向量余弦最小约0.999999999999787，最大分量误差约1.23×10⁻⁷；400对检索分数最大差约1.92×10⁻⁷，命中仍为2/21和4/21。至此本固定集上的原生权重、导出和两种调用前端相符，不能再将失败解释成此处的前端、量化或导出链差异。停止同模型调用方式的重复尝试，当前模型继续只作候选实验，不启用默认语义召回；下一片转向独立词面和跨语候选评测。
