# 材料依据与新增工程决策

## 来源层级

A. 本轮用户明确条件：本机摄像头已连接；Kimi Code CLI已配置且允许调用；
暂时没有Haonan模型代码；目标是实际开发，不是继续制作PPT。优先级最高。

B. 后续明确设计：Jinwen与Haonan负责Tutor；快慢记忆；定制教学prompt；
API负责复杂教学推理，本地承担视觉与语音；标签选目标；代码不公开。

C. AI_Tutor_Progress_HKUSTGZ_v6.pptx（本次已读取全篇文字与讲稿）：
- 第1–4页：功能积木场景；Tutor与小组硬件/拓扑/验证/执行分工；信息来源。
- 第5–6页：快慢记忆、未完成实验、一次一步；历史配置需要重新确认。
- 第7–9页：本地与API协作、情绪模型位置、两类成本机制。
- 第10–14页：状态、后续集成、更新与召回、目标标签不是身份/音频说话者/PTZ证明。
其中“已连接”的演示描述不替代当前本机检查；本轮明确排除情绪模型。

D. 原始上传材料：
1. Emotion-Aware STEM Tutoring System Design Based on Multi-Modal Large Language Models(3).docx，
   2025-12-23提案；1.1/1.2/2.1/2.2提供Think–Make–Iterate、四层架构、
   MCU/电机/超声波/LED、拓扑、验证和执行。旧分工不覆盖当前两人负责Tutor的要求。
2. Annex II_MPhil Thesis Proposal Jinwen He.pdf：
   提供YOLO视觉到VLA的长期研究方向、模块状态与执行的接口背景。
   本轮只做用户要求的人脸检测追踪，不自动扩大到模块识别、VLA或训练目标。
3. MPhil Thesis Proposal Haonan_jiang.docx：
   提供边云分工、多模态感知与部署方向。本轮不实现其模型，也不照搬旧性能指标。
4. Vibe_Coding_Emotion_Aware_Robotics.pdf：
   功能积木、儿童意图、拓扑验证、具身执行与教学反馈的产品叙事。
   作为产品背景，不使用其中未核验的市场统计作为工程论据。
5. Figures.pptx：
   EQ-Harness/多模态情绪架构，作为未来EmotionProvider接口背景。
   不根据图推断已经拥有模型代码或权重。
6. 两张OBSBOT实物照片：
   支持品牌线索，不足以确定型号、固件、USB设备信息与云台SDK能力。
7. 先前视频与场景插画：
   作为功能与交互验收案例；不是实拍测试、真实硬件轨迹或已完成代码证明。
   v6讲稿已明确150秒模块化小车情节是scripted simulation。

以前各版PPT是迭代设计历史；最终流程以v6和最新用户限制为准，不并列执行互相冲突的版本。

## 新增工程决策

以下由本规格提出，并非声称原始资料已经规定：
- 本地FastAPI+React工作台、SQLite、无工具Kimi provider。
- learner_id/track_key/说话者分离；多目标视觉与档案切换独立。
- source-aware事件、schema校验、快慢记忆写入与删除策略。
- 按键语音、半双工、单轮独立CLI调用、应用自行管理上下文。
- 确定性PTZ及watchdog、scope/generation防串话。
- 先文字垂直闭环再语音，再按设备条件加入物理PTZ。
- 工程帧率/延迟目标及验收清单。
这些均需在真实仓库/机器上实现、验证，不能标成当前已实现。

## 外部技术核验（2026-10-31）

1. Kimi 新版命令：
https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command
-p/--prompt、stream-json、自定义agent、恢复参数限制；非交互模式自动权限。
本机 --help 优先于假定的最新文档。

2. Kimi 迁移说明：
https://www.kimi.com/code/docs/en/kimi-code-cli/guides/migration
官方区分旧Python kimi-cli与新Node Kimi Code。不要未经许可迁移。

3. Kimi 新版 agents：
https://www.kimi.com/code/docs/en/kimi-code-cli/customization/agents
agent frontmatter tools:[] 禁用工具；system prompt和工具权限不是同一件事。

4. Kimi 配置：
https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/config-files
全局tools.enabled空数组是不限制，与agent tools:[]不同。
资料取自编制时版本，运行时复核。

5. 旧版 print：
https://moonshotai.github.io/kimi-cli/en/customization/print-mode.html
--print、stdin和stream-json；自动处理审批的风险不能忽略。

6. OBSBOT官方SDK：
https://www.obsbot.cn/sdk
支持多系统、设备管理与云台控制等；申请可能需要登录/邮箱。
具体型号/操作/SDK可得性必须本机查证。
https://www.obsbot.com/sdk （编制时部分请求受到网页验证，不绕过）

7. Ultralytics tracking：
https://docs.ultralytics.com/modes/track/
ByteTrack/BoT-SORT等；默认项随版本变化，因此显式锁定tracker。

8. COCO类别：
https://docs.ultralytics.com/datasets/detect/coco/
普通person类别不等于人脸专用检测权重。

9. 人脸权重候选作者仓库：
https://github.com/lindevs/yolov8-face
作者提供YOLOv8-Face、PT/ONNX及摘要。第三方权重，不冒称官方。
分别核验库/权重/数据许可。

10. 本地语音候选：
https://github.com/ggml-org/whisper.cpp
本地ASR、多平台/加速和麦克风示例。以本机实测决定模型和后端。

11. macOS摄像头发现：
https://developer.apple.com/documentation/avfoundation/avcapturedevice
DiscoverySession、设备uniqueID等。系统和权限由本机探测。

## 没有被本次回答验证的事项

用户电脑的真实OS/设备型号/固件；摄像头读帧；本机Kimi版本/认证；
PTZ实际移动；本地ASR/TTS质量；项目代码运行；任何节省比例、帧率或延迟。
它们是给本地Codex的必做验证，不是本文件已经完成的实验。
