# 儿童英语评测元数据准备

已固定作者仓库 `jimbozhang/speechocean762` 的提交 `613968e3b0b789fc33936fb5eba1973176ba7d11`（2025-10-26），读取README及test的`spk2age/utt2spk/text/wav.scp`，五份文件均按Git blob SHA-1验证，再记录本地SHA-256。仅保存公开元数据，没有下载录音、运行模型或接受访问协议。

实际test有125名匿名说话人、2500条目标文本；其中64名未成年说话人、1280条录音条目，作者标注年龄6–15岁。13岁仅1人、14岁2人，不能假定各年龄均衡；test没有16–18岁条目。其余61人为19–43岁。上述年龄是数据集标注，不是当前年龄或声线推断。[固定版本年龄表](https://github.com/jimbozhang/speechocean762/blob/613968e3b0b789fc33936fb5eba1973176ba7d11/test/spk2age)、[录音与说话人映射](https://github.com/jimbozhang/speechocean762/blob/613968e3b0b789fc33936fb5eba1973176ba7d11/test/utt2spk)。

OpenSLR标注CC BY 4.0；作者说明允许商业及非商业下载使用。此处只核实许可标签和公开入口，未来获取或分发保留署名与修改说明。[OpenSLR官方数据页](https://www.openslr.org/101/)、[作者固定README](https://github.com/jimbozhang/speechocean762/blob/613968e3b0b789fc33936fb5eba1973176ba7d11/README.md)。

这是中文母语者的英语朗读/发音评价集。目标文本与实际说出的词可能因漏读、错读而不同；不能把对目标文本的全部差异归为识别错误，也不能据此声称完成儿童普通话或自然中英混读验证。下一步先按年龄与说话人冻结抽样和实际逐字稿核验规则，再取固定录音。仍需另行覆盖16–17岁、儿童普通话/混语及课堂远场。

元数据与清点结果：`runtime/goal-baselines/20261104/speechocean-metadata25`。当前没有选择测试子集、下载音频或产生识别成绩。
