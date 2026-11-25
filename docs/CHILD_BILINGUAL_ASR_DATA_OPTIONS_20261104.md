# 儿童、青少年与中英混读 ASR 公开语料候选

检索日期：2026-11-04。下文保留最初的一手资料可行性研究口径；checkpoint23 已取得 ASCEND 固定 test 并完成非 audio 清点，详见 `ASCEND_ASR_EVALUATION_PREPARATION_20261104.md`，尚未运行 ASR。其余三项仍未获取语音。没有登录、接受访问协议或付款。以下来源不能合并宣称“全年龄儿童中英混读已验收”；年龄只采用作者记录，不从声线、语速或年级推算。

| 候选 | 能补的真实录音范围 | 获取与许可结论 | 本地评测定位 |
| --- | --- | --- | --- |
| speechocean762 | 中文母语儿童、青少年和成人的英语朗读；官方 test 年龄表包含 13–15 岁 | 公开下载、CC BY 4.0；作者明确允许商业和非商业用途 | 优先准备儿童/青少年英语压力集，但需先处理“朗读目标文本≠实际说出的逐字稿” |
| ASCEND | 19–30 岁成人自然中英对话，含句内混读 | 官方公开 HF，无访问门槛提示；数据卡 CC BY-SA 4.0 | 优先准备真实混读集；不能充当儿童混读集 |
| ChildMandarin | 3–5 岁幼儿普通话，含年龄标注 | HF 账号及接受访问条件；非商业研究，明确禁止商业产品开发 | 许可适用时补低龄普通话；不是英语或中英混读 |
| MyST | 3–5 年级学生与科学虚拟导师的英语对话 | 非商业研究 DUA；商业另授权；作者入口本次无法访问 | 最贴近教育自由回答，但获取及版本需先落实 |

## 1. speechocean762：最容易落实的儿童与青少年英语候选

作者发布 250 位中文母语英语学习者的 5,000 条录音，其中一半说话人为儿童；约 6 小时，手机近讲、安静房间，属于真实朗读。官方 train/test 各 125 位说话人，test 2,500 条，提供 `spk2age`、`utt2spk`、`text` 等。只读核对当前官方 test 年龄表可见 6–15 岁未成年记录，包括 13、14、15 岁；不据此声称覆盖 16–17 岁或每个年龄均衡。[作者论文](https://www.isca-archive.org/interspeech_2021/zhang21x_interspeech.pdf)、[官方仓库及 split 结构](https://github.com/jimbozhang/speechocean762)、[官方 test 年龄元数据](https://raw.githubusercontent.com/jimbozhang/speechocean762/main/test/spk2age)。

OpenSLR 标明数据许可为 CC BY 4.0、压缩包约 520 MB，公开提供下载；作者明确商用和非商用均可免费使用。须保留署名、许可和修改说明，不把代码许可替代语音许可。[官方数据页](https://www.openslr.org/101/)、[作者许可声明](https://www.isca-archive.org/interspeech_2021/zhang21x_interspeech.html)、[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)。本次只读页面和匿名年龄元数据，未取音频。

关键限制：这首先是发音评价语料。原论文的 completeness 就是目标文本中实际读出词语的比例，包含漏读和错误发音。因此，直接对目标文本算 WER 会混入学习者本身的漏读/错读，不能全部归因 ASR。建议在看模型输出前固定一小份年龄分层子集，独立核听实际逐字稿，保留原目标文本与更正记录；不删除“难听懂”的样本来提高结果。它测中文母语者说英语，不测同句中文混读。

## 2. ASCEND：小规模、许可清楚的真实中英混读候选

作者采集 23 位 19–30 岁大学生的自然对话，约 10.62 小时；同时包含中文、英文单语句及真正句内混读。论文明确三套 split 的说话人组合不重叠；官方 test 为 1,315 条，含 `language`、说话人及会话字段。可固定 test 全集并另报 `language=mixed` 子集，而非把所有双语语料都算成混读。[作者论文 §4–5](https://aclanthology.org/2022.lrec-1.788.pdf)、[作者仓库](https://github.com/HLTCHKUST/ASCEND)。

官方 HF 页面和文件目录公开，无 gated 提示或付费要求；数据卡标 CC BY-SA 4.0，允许商业用途但要求署名，分发改编材料须遵循相同许可。论文 PDF 的出版许可不能替代数据卡许可。[官方数据卡](https://huggingface.co/datasets/CAiRE/ASCEND/blob/main/README.md)、[官方文件目录](https://huggingface.co/datasets/CAiRE/ASCEND/tree/main)、[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)。本次未验证大文件实际下载。论文与现卡片的 validation 数量有 1 条差异（1,129/1,130），后续需锁定具体 revision 和文件清单，不能混用版本统计。

它只能补成人混读，不补儿童或青少年混读；主要为干净近讲、已经切句的对话片段，也不独立证明连续开麦的停顿切分或远场表现。

## 3. ChildMandarin：低龄普通话，使用范围受限

3–5 岁真实儿童 397 人、41.25 小时，手机录制的日常交流；作者提供人工字符转写和说话人年龄。官方 train/validation/test 分别 317/39/41 人；test 4,198 条、4.12 小时。中文单语范围明确，可在许可成立后沿官方 test 分年龄抽样，不重新随机切说话人。[作者仓库与 split](https://github.com/flageval-baai/ChildMandarin)、[ACL 作者论文](https://aclanthology.org/2025.acl-long.614.pdf)。

官方 HF 标注 CC BY-NC-SA 4.0，同时 gated 条款要求账号、分享 HF 信息并接受条件；额外条款明确仅学术/非商业研究、禁止商业产品开发及商业语音服务，禁止未经许可再分发，且有参与者撤回后的删除义务。页面未标下载费用，但不等于免审批或可直接用于商业项目内部验证。当前只能列为有条件候选，不据“开源”两字纳入本产品默认资产。[官方访问条件及数据卡](https://huggingface.co/datasets/BAAI/ChildMandarin)。

## 4. MyST：教育自由回答最贴近，年龄与版本要诚实

真实 3–5 年级学生与虚拟科学导师的英文对话。2024 作者论文统计约 393 小时、1,371 名学生，train/dev/test 按学生分离；**作者明确没有记录每位学生的年龄、性别或第一语言**，因此不能写成已核验的“8–10 岁英语母语儿童”。原 LDC 发布有部分转写错误，2024 论文使用修正后的 test；不得把新论文基线直接套到旧包。[作者论文 §2、§4](https://aclanthology.org/2024.lrec-main.1052.pdf)。

作者论文说明非商业研究可在完成 DUA 后免费获取、CC BY-NC-SA 4.0；作者入口 `myst.cemantix.org` 本次工具无法打开，不能确认现时申请是否通畅。LDC2021S05 仍有官方目录和固定 train/dev/test，要求签署非商业用途协议；页面写需登录查看适用费用，未登录，不报价。商用另授权。[作者获取入口](https://myst.cemantix.org/)、[LDC 官方目录](https://catalog.ldc.upenn.edu/LDC2021S05)、[LDC 专用协议](https://catalog.ldc.upenn.edu/license/myst-childrens-conversational-speech-agreement.pdf)。这两条发布路径不能当成相同版本或相同协议。

## 建议下一片与仍缺的覆盖

先准备 speechocean762 与 ASCEND 的固定小样本方法，分别验证儿童/青少年英语和成人混读，保持两套结果独立；许可适用后再加入 ChildMandarin 与 MyST。先锁数据 revision、官方 split、文件/转写 SHA、说话人和已知年龄分层、文本规范化与抽样规则，再运行同一 ASR 链路，不能看输出后挑样本。汉语报 CER、英语报 WER；混读固定汉字与英文词的计分口径并报 MER，同时保留语言子项。真实时间回放与不限速准确率分开，首个 partial、稳定 final、截断/漏音和实际音频终点均单列。

这四项没有形成“同一真实儿童/青少年说话人的中英自然混读”覆盖；也没有充分覆盖 16–17 岁、学龄普通话全段、课堂多人远场、设备回声和长思考停顿。公开预切句语料不能替代本机连续录音/终点检测验收。已有 SAPI/Qwen 合成语音、变速或变调副本仍是合成压力输入，不是儿童人口覆盖。未核验公开 ASR 预训练是否包含这些语料，未来分数需标明训练污染未知。

额外排除一个容易误认的来源：TALCS 虽来自在线教学，但作者明确录音者为 **22–28 岁教师**，不能补儿童混读；本次不把它再列入首选。[TALCS 作者论文 §3.1](https://www.isca-archive.org/interspeech_2022/li22j_interspeech.pdf)。研究过程中不检索、展示或尝试识别具体儿童，后续仅需匿名计数及评测所需元数据。
