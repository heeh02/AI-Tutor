# AI Tutor：从产品方案到真实原型的完整实现规格

版本：1.0 · 编制日期：2026-10-31
负责人：Jinwen He、Haonan Jiang
实施执行者：在用户电脑上运行、获得项目目录权限的 Codex
文档性质：待实施的产品与工程规格，不是现有系统通过验收的证明。

## 0. 阅读顺序与决策优先级

先读本文件，再读 CODEX_EXECUTION_PROMPT.md、ACCEPTANCE.md、SOURCES_AND_DECISIONS.md。
configs、schemas、examples 是待实现接口的起始规格，不代表应用已经存在。
已有仓库的合理架构优先复用；协议、配置和依赖最终以本机探测及对应版本官方文档为准。
需求冲突的优先级：用户本轮明确要求 > 后续已确认产品设计 > v6 产品方案 > 原始提案。
旧提案中的分工、时间表、研究目标，以及演示材料中的完成状态，不得直接当作本机代码状态。

## 1. 项目目的：我们为什么做它

### 1.1 大项目

大项目是面向儿童的实体功能积木 STEM 学习系统。学习者用自然语言提出想法，
通过控制、电机、距离传感器、灯等实体模块构建行为，再通过实际试验理解因果关系。
主循环是 Think–Make–Iterate：提出目标、搭建、测试、观察、调整、再测试。

这里的积木是功能模块，不是把 MOVE/TURN 字样印在平板上的指令卡。
摄像头是 Tutor 的视觉输入设备；它不是积木控制器，也不是小车本体。

### 1.2 Jinwen 与 Haonan 当前负责的部分

当前任务以两人共同建设的 AI Tutor 子系统为中心：
- 使儿童能够用自然语言描述目标、问题、已经尝试的办法和观察到的现象。
- 使 Tutor 给出适量、连续、具体的指导，而不是替儿童一次性完成整个任务。
- 以快记忆维护当前活动，以慢记忆保留有依据的学习进展、帮助偏好与待完成实验。
- 用定制教学指令控制解释方式、提示粒度、语言和追问策略。
- 本地承担高频视觉、语音等工作，远程模型只承担需要推理的教学决策。
- 通过教师操作界面明确当前视觉目标、学习者档案、证据来源和系统能力。

原始提案的完整系统还包括拓扑感知、安全验证、硬件执行、情绪模型和边云协同。
本次必须为这些能力留下清楚的接口，但不假装它们都已经可用。

### 1.3 本轮交付目标

在已经连接摄像头的用户电脑上，形成可以启动、交互、恢复与测试的本地原型：

发现设备 → 实时 YOLO 人脸检测与多目标跟踪 → 手动选择视觉目标；
文字或按键说话 → 本地语音识别 → Tutor 上下文与教学策略 →
本机已有 Kimi Code CLI 的远程对话后端 → 回复校验 → 本地语音播报；
活动记录 → 快记忆更新 → 有依据的慢记忆整理 → 下次会话选择性召回。

软件视觉跟踪是必交项。物理摄像头云台跟随在本机型号、接口和控制测试证实支持时实现。
硬件暂不支持或官方 SDK 需要申请时，明确报出受阻原因，软件交互继续可用。

### 1.4 当前已知条件与未知条件

已知：摄像头已接用户电脑；照片显示 OBSBOT 品牌；本机已配置 Kimi Code CLI；
用户允许 Codex 调用该 CLI；Haonan 的模型源码目前不可用。
未知：电脑系统/CPU/GPU、摄像头准确型号/固件、Kimi 实际版本、
现有仓库结构、当前麦克风及本地语音资源、厂商云台控制能力。
禁止从浏览器系统、照片外观或先前回答推断上述未知项。

## 2. 范围、非目标与完成定义

### 2.1 本轮必做

1. 本机环境/摄像头/音频/Kimi 能力探测和脱敏报告。
2. 摄像头预览、重连、权限/占用提示、设备选择和稳定标识。
3. 真正支持 face 类的人脸检测模型、多脸跟踪、目标选择、丢失状态。
4. 独立于跟踪 ID 的学习者档案、会话、活动管理。
5. Kimi CLI 真实连通；版本适配、无工具 Tutor agent、超时、取消、错误处理。
6. 定制教学指令、任务状态管理、证据来源、逐轮可理解指导。
7. 快慢记忆、重启恢复、按学习者隔离、查询/纠正/删除。
8. 本地 ASR/TTS 和可停止播报；先文字闭环，再完成按键语音闭环。
9. 真实模式/人工报告模式/明确标记的模拟模块接口。
10. 统一教师工作台、基本本地指标、可复现测试与一键启动。

### 2.2 条件必做

实际设备支持且接口可获取时，完成本地规则驱动的物理云台跟随：
发现能力 → 手动小幅控制 → 标定方向/范围 → 自动跟随 → 丢失停止 → 急停。
接口未知时输出 BLOCKED/UNSUPPORTED，而非用数字裁切伪装物理跟随。

### 2.3 明确不做

- 不加载、替代训练、伪造 Haonan 的情绪模型；不输出情绪分数。
- 不根据脸框推断理解力、注意力、身份、情绪或积木连接状态。
- 不实现跨会话人脸识别，不建立人脸图库，不默认启用 ReID。
- 不训练 VLA，不实现小车自主执行，不刷 MCU 固件。
- 不自动部署云服务、开放公网、创建公共仓库或上传源码。
- 不把画面逐帧送远程模型，不让远程模型参与高频 PTZ 控制。
- 不把演示视频、模拟数据或硬编码回复作为真实能力验收。
- 不要求重构为完整 Codex 开源分支；若本地已有 harness，优先复用合适模块。

### 2.4 三个完成等级

CORE_TEXT：真实摄像头＋跟踪＋Kimi 文字对话＋快慢记忆＋活动面板。
CORE_VOICE：CORE_TEXT＋真实麦克风、本地 ASR/TTS、打断与回声规避。
PTZ_VERIFIED：CORE_VOICE＋经过方向和故障测试的实体云台跟随。

交付时分别报告等级，不将 CORE_TEXT 写成完整语音产品，不将软件框追踪写成 PTZ_VERIFIED。
主要目标是 CORE_VOICE；PTZ 状态独立报告。

## 3. 完整用户旅程与积木学习场景

### 3.1 启动与选人

教师启动应用。系统自动列出设备并优先匹配已保存的物理摄像头，首次优先候选 OBSBOT。
多个相近候选时提供预览供选择；不能凭索引猜设备。
教师创建匿名学习者档案，如 learner_A，并可填写指导语言、帮助粒度。
进入摄像头画面后点击 Track A 作为当前视觉目标。

视觉目标、学习者档案和当前讲话者是三个不同概念。选择 Track B 只改变画面关注对象，
不自动把 learner_A 的记忆改成 learner_B。更换学习者需要独立显式操作。
首版在多人场景使用教师选择档案＋按键说话，不声称自动判定谁在讲话。

### 3.2 活动一：搭建会在墙前停车并亮绿灯的小车

目标：理解模块功能、缺失能力、观察结果和单变量比较。

阶段 A / 表达：
儿童：“我想让小车到墙前停下来，再亮绿灯。”
Tutor 确认目标，并询问当前有什么模块；不知道真实配置时必须询问或读取真实接口。

阶段 B / 模块：
CONTROL 管理逻辑，MOTOR 产生运动，DISTANCE 提供距离信息，LIGHT 提供反馈。
人工或模拟接口报告仅有 CONTROL、MOTOR、LIGHT 时，Tutor 引导辨认缺少测距能力：
“电机让它动起来。你觉得哪个模块能告诉它墙有多远？”
不能说“我看到你没有接传感器”，因为本轮只实现人脸检测。
若来源是模拟接口，教师面板和记录持续标记 simulated。

阶段 C / 补齐：
儿童说“我接好了”，记录为 learner_report，不升级为硬件验证成功。
教师可以确认；以后真实拓扑接口可提供硬件事件。
当前版本不会自动运行电机。

阶段 D / 首次结果：
儿童报告“它能停下来，也变绿了，但是离墙太近”。
保存为“首次试验据报告成功，但希望间隙更大”，不是“首次试验失败”。

阶段 E / 改进：
Tutor 保持模块配置、起点、墙位置等可比较条件，聚焦一个调整：
“这次先只改停车距离。你想让它更早停，还是更晚停？”
实际参数更改由教师或未来经过验证的硬件接口完成。

阶段 F / 再测：
儿童报告更大间隙，Tutor 引导比较发生了什么。
不能仅因自己给过建议，就把再测标记已完成。

阶段 G / 结束：
记录已报告进展、一次一步的明确偏好、尚未做的“移动墙再测试”。
用户可查看和修订慢记忆。

### 3.3 活动二：跨会话继续

重启后教师选择同一 learner_id。
Tutor：“上次你说小车能提前停下并亮绿灯。今天的模块还和上次一样吗？”
先确认当前配置，再继续移动墙实验。历史配置只是历史，不等于当前硬件状态。
若更换 learner_id，不召回前一个学习者的历史。

### 3.4 这不是固定脚本播放器

上述情节是验收 fixture，不是运行时按关键词返回固定台词。
运行时应允许新目标、新观察、否定、反复尝试、改变帮助方式及暂时离开活动。
活动模板定义知识和阶段，不决定每句生成内容。

## 4. 产品界面

推荐本机 Web 工作台：Python 后端运行本机设备与模型，浏览器仅作为界面。
不要一开始做 Electron 封装或云端部署。对外仅监听 127.0.0.1。
默认中文教学，提供英文演示切换；不要把汇报语言误当成真实交互必须英文。

主界面：
- 摄像头区：实际视频、face 框、短暂 track 标签、当前目标、丢失状态、独立 PTZ 状态。
- 对话区：文字输入、按住说话、可纠正 ASR 文本、回复、停止播报、网络状态。
- 学习区：当前目标、阶段、已尝试、最新结果、待验证步骤、来源徽标。
- 教师区：学习者档案切换、当前视觉目标选择、教学提示粒度、设备设置、紧急停止。
- 可折叠记忆区：当前快记忆、召回的慢记忆、每条来源、纠正/删除。
- 可折叠诊断区：实际帧率、延迟、Kimi 调用、token 可用性、能力缺失。

视觉：延续先前设计的深蓝 #003366、金色 #996600、暖白；
避免消费级监控面板风格。徽标应使用真实授权素材；无素材时用文字名称，不能重绘假校徽。

状态必须真实：连接中、检测运行中、等待用户、转写中、生成中、播报中、取消、错误、降级。
“正在生成”不是“已经听见”或“已经执行”。
所有模拟来源显著区分。不存在的摄像头、情绪结果、硬件执行不得用绿灯冒充。

## 5. 架构与数据流

建议技术栈：Python 3.11/3.12 中选实际依赖兼容版本；FastAPI；React+TypeScript；
SQLite；OpenCV/原生采集适配器；YOLO-Face；ByteTrack；whisper.cpp；
平台本地 TTS；Kimi CLI provider。版本在本机核验后锁定。
这些是本规格的工程选择，不是对现有代码结构的断言。

三条独立执行链：

视觉链：
CameraManager → FrameBuffer(size=1或2) → FaceDetector → Tracker → TargetManager
→ UI overlay / structured vision facts / optional deterministic PTZ controller

交互链：
text / local ASR → normalized user turn → SessionCoordinator →
ContextBuilder(task + fast + selected slow + teaching policy + limited vision facts) →
KimiProvider → validated reply → UI + local TTS → delivery state

记忆链：
source events → fast-state reducer → append-only turn/event log →
bounded consolidation queue → candidate memories → validation/merge →
SQLite persistent notes → filtered retrieval → ContextBuilder

TaskStateProvider 是独立的手动/模拟/未来真实模块输入；
EmotionProvider 只保留接口或 feature flag，默认 disabled，不运行占位分类器。

### 5.1 并发规则

- 摄像头和推理不能阻塞异步 HTTP/WS 循环；视觉工作放独立 worker。
- 捕获线程只保留最新帧，禁止排队积压数秒旧画面。
- 高速 tracking 事件不全部进慢记忆、不逐帧调用大模型。
- 会话串行提交状态；每轮有 request_id、session_id、learner_id、generation。
- 同一个会话最多一个有效生成；新输入/换档案/结束会话可取消旧轮。
- TTS 带 turn_id 与 generation，旧答案不能播给新学习者。
- 低优先级记忆整理不得占用实时交互唯一执行槽；必要时延后或使用已完成的摘要。
- 摄像头 PTZ 命令另带 target_generation、frame age、expire_at，丢弃过期控制。
- 队列必须有界；内存、磁盘和服务请求都有上限。

### 5.2 标识分离

learner_id：教师创建的匿名、持久档案。
session_id：一次学习会话。
activity_id：一项活动的实例。
camera_session_id：一次采集/重连周期。
track_key：camera_session_id + tracker_id + 出生计数；不持久识别人。
binding_id：显式将某 learner/session 与某视觉目标临时关联的操作。
request_id/turn_id：对话生成与记录标识。

## 6. 阶段零：Codex 必须先做的本机探测

先检查仓库 git status、现有文档/AGENTS、现有环境和代码。禁止覆盖未提交改动。
如果当前路径不是项目，不扫描用户所有私人文件；在选定工作区创建新项目。

### 6.1 系统

检测系统版本、CPU 架构、可用内存、GPU/加速能力、Python、包管理器、Node、
可用本地音频引擎、音频输入输出、占用端口、权限状态。
现有开发环境可用时复用，新增依赖放项目环境，不全局替换用户依赖。
不默认 Windows，不把本聊天运行环境当作用户电脑。

### 6.2 Kimi

定位可执行文件、解析实际路径、记录 --version 和 --help。
确认这是旧 Python kimi-cli 还是新 Kimi Code，以及对应 agent 和输出协议。
只做允许的登录状态/连通性检查；不输出 auth token、密钥或完整配置。
不自动升级、迁移、改写用户已有 Kimi 配置。
登录失效时明确 HUMAN_ACTION_REQUIRED，其余可独立任务继续。

### 6.3 摄像头

先枚举真实系统设备，再逐个有限时试开：
macOS：AVFoundation DiscoverySession/uniqueID，辅助使用 system_profiler/USB 信息；
Windows：Media Foundation/DirectShow 的设备标识及 USB/PnP 信息；
Linux：V4L2 设备和 /dev/v4l/by-id。
命令或接口是否存在须探测，不机械执行不存在的系统命令。

保存设备名、稳定 ID、物理/虚拟标记、VID/PID（可得时）、分辨率/帧率/格式、
成功读帧情况、权限/占用/错误原因、相机麦克风的独立设备信息。
原生设备枚举顺序与 OpenCV index 可能不同，必须验证映射。
超时探测用可回收的子进程，避免一个打不开的设备把启动卡死。

### 6.4 查官方文档

以探测出的型号、系统和固件查厂商支持页、SDK、UVC/OSC 能力。
产出 DEVICE_CAPABILITIES.json 和 RESEARCH_LOG.md：
来源网址、访问日期、型号适用范围、OS、接口/依赖、是否已实测、剩余阻塞。
OBSBOT 官方页面列出多系统 SDK 与云台控制，但不意味着每台设备/每种接口全部支持。
SDK 要登录申请时报告准确入口，不自行冒用身份或提交用户个人信息。
不能抓包猜私有协议后直接发电机命令，不能刷固件或替换驱动解决不了就乱试。

### 6.5 第一项可见交付

应用中出现真实设备列表和真实视频。报告同时明确 Kimi 版本/可用性、
视觉权重计划、PTZ 可用/未知/不支持状态。不得只交设备枚举的文字说明。

## 7. 摄像头、检测和跟踪

### 7.1 CameraManager

只有一个捕获者拥有设备；UI 不再打开第二路同设备 getUserMedia。
建议先以协商成功的 720p/30 为目标，按设备和负载降级；不是硬保证。
分离 video resolution、detector input size、preview fps 与 inference fps。
帧包含 frame_id、单调时钟时间、原始尺寸和采集会话 ID。
显示镜像只在 UI 做；检测、框坐标和 PTZ 控制使用明确的原始坐标系。

实现 start/stop、切设备、断线退避重连、权限指引、占用提示、关闭释放。
禁止悄悄终止其他视频会议软件。
相机重连后清空 tracker、解除视觉绑定，必要时让教师重新选人。

### 7.2 人脸专用权重

不能把 COCO 的 person 类当成 face。
先找项目现有权重；若没有，从可核验作者发布页选轻量 YOLO-Face。
一个可考察候选是 lindevs/yolov8-face 的 nano 权重，作者提供 PT 与 ONNX。
这是第三方人脸权重，不要写成 Ultralytics 官方人脸模型。
记录作者、版本/commit、下载 URL、SHA-256、类别表、输入规格、预处理与输出格式。
分别核对运行库、权重和数据来源的许可证，不根据一个仓库标签推断全部许可。

使用实际 provider 做小型基准，选择 CPU/可用 GPU；Mac 上不默认 CUDA。
优先选能在本机稳定运行的格式，而不是追逐最新型号。
ONNX 支持与跟踪器接口独立适配；不得认为任意 ONNX 都能直接送 model.track。
letterbox 与坐标反变换必须有单元测试，boxes 统一成原始图像归一化 xyxy。

### 7.3 多目标跟踪

固定摄像头优先显式选择 ByteTrack；相机运动时评估 BoT-SORT 的运动补偿。
锁定实际 tracker 参数，不依赖某版本的默认 tracker。默认关闭身份 ReID。
检测器每帧被调用或跳帧时，tracker 的 dt/生命周期应与真实处理频率相符。

Track 状态：tentative、confirmed、selected、temporarily_lost、lost。
初始建议参数仅为可调默认值：
- 连续稳定约 0.5 秒再自动候选；
- 选中目标丢失约 0.4 秒后停止 PTZ，不继续向最后方向转；
- 短暂遮挡可尝试延续，但交叉/歧义后要求重新选择；
- 不依赖“最大脸”每帧重新选择；选中目标保持粘性。

同一 track 并不证明同一人；ID 可能交换或复用。
验收不能承诺无身份模型下任意遮挡后 100% 重认人。
视觉信息只有脸是否可见、框位置、目标状态及帧时效。
不输出 happy/frustrated/understands/attention_score。

### 7.4 物理 PTZ

采集与控制接口分开。接口提供 capabilities、state、pan/tilt、stop、reset；
可用函数、单位、方向、范围来自型号文档和实测，不填猜测 API。
顺序：读能力 → 用户可见提示下小角度/短时手动运动 → 标定 → 自动跟随。
控制模式互斥：off / vendor_auto / external_yolo / manual。
厂商自动跟踪和 external_yolo 不得同时控制，防止互相抢方向。
如无法可靠关闭内置跟踪，不进入 external_yolo。

规则控制，不由 Kimi 决定：
ex = 2*(face_cx - target_cx)/frame_width
ey = 2*(face_cy - target_cy)/frame_height
平滑误差后，用带死区、速率/加速度限制的比例控制产生指令。
速度/角度控制按驱动能力分别实现，不能混淆绝对角与速度。
默认不开自动变焦；校准显示镜像和实际电机方向，不追踪显示变换后的框。

PTZ 安全不变量：
帧过旧、目标丢失、应用关闭、USB 断开、切人、手动急停都必须发停止；
若设备按速度持续运动，需要独立 watchdog/短时脉冲及失联停止方案。
无法保证失联停止时不得启用持续速度控制。
默认低速限幅，由教师手动开启；不做全幅高速扫视。
若只实现数字裁切，UI 明确 digital_crop，不算物理跟随。

## 8. 语音交互

先实现文字以隔离问题，然后完成按键语音；不能最后只有文字却称“实时聊天完成”。

### 8.1 录音和 ASR

推荐先用 push-to-talk。按下开始，松开结束；上限如 20 秒可配置。
列出麦克风，教师可测试音量，明确当前输入设备。
后端本机采集音频，界面只发送开始/停止事件；不要先引入浏览器封装音频兼容问题。
采样率/声道按实际硬件取得，再重采样到 ASR 需要的格式。
whisper.cpp 可作为本地候选；按本机能力选多语言小模型，不能用只支持英文的 .en 模型处理中文。
维护词表提示：主控、电机、测距、超声波、停车距离、RGB。
过短、静音、不确定的语音不作为已确认事实；给用户编辑转写或重录入口。
不使用隐式远程浏览器语音服务伪装成本地 ASR。

### 8.2 TTS

优先可离线使用的系统声音；macOS 可检查 say/AVSpeech，Windows 检查本地语音能力，
Linux 检查实际安装引擎。模型引擎替换作为插件。
用离线测试确认声音本地运行，不把云语音方案标成本地。
回复文本经内容/协议校验后再播报；不播 JSON、工具日志或思考内容。
语音队列一个 active 播放者，支持 cancel，切换档案立即停止旧播报。

### 8.3 避免回声与处理打断

第一版半双工：播报期间不把扬声器回声当新输入。
按“说话”或“停止”先终止播报，再开始用户录音。
新录音取消旧生成并增加 generation；旧完成事件不得重新发声。
正常流程：IDLE → RECORDING → TRANSCRIBING → GENERATING → SPEAKING → IDLE。
每个状态都支持 cancel/error，UI 有明确反馈。
全双工、自动唤醒词、回声消除和说话人分离留后续，不堵塞首版闭环。

## 9. Kimi Code CLI 后端：真实接入而非终端拼接

### 9.1 版本适配

截至编制时，官方同时有旧 Python kimi-cli 与新 Node Kimi Code 文档。
新文档使用 -p/--prompt 和 --output-format stream-json；
旧文档有 --print、stdin/stream-json 等不同形式。
新旧 agent 文件格式和 session 参数也不同。
必须使用本机 --help、版本匹配文档和 smoke test 决定，不能混用参数。
不要擅自升级用户已经配置好的 CLI。

定义统一 ChatBackend：
probe() → version/capabilities/ready
generate(TutorRequest) → normalized events/final reply
cancel(request_id)
close()

第一版采用单轮独立调用，由本应用掌握记忆和上下文。
不要每轮既 resume 全量 CLI 历史，又重复塞入全量快慢记忆。
后续持久连接要验证上下文重置和隔离后再启用。

### 9.2 最重要的权限界线

开发期的 Codex 获准研究、编程、测试；
运行时 Kimi 只做教学对话，没有开发权限。

为 Kimi 建立专用 Tutor agent，工具权限必须在运行时配置中禁用：
无 shell、文件读写、网络工具、MCP、browser、subagent、自主计划循环和设备工具。
新格式 agent frontmatter 的 tools: [] 文档语义是无工具；
但全局 config 的 enabled=[] 可能是“不作限制”，绝对不能等同。
旧格式需根据实际版本构造和测试。
仅在 system prompt 写“不要执行工具”不构成限制。

Kimi 的非交互运行可能自动批准工具调用，因此不允许用默认 coding agent 直接接孩子的文字。
无需也禁止附加 --yolo/--auto 来绕过提示；这里的 yolo flag 与视觉 YOLO 无关。

### 9.3 运行隔离

固定 executable 和 argv，shell=False；prompt 是数据，不是命令拼接片段。
cwd 使用专用无机密运行目录；不要让产品对话在源码根目录中继承开发指令。
检查会自动加载的 AGENTS、SYSTEM、skills、MCP、plugins、hooks。
采用该版本支持的每次调用/独立配置隔离方式，不修改全局已配置环境。
新自定义 agent 不插入 coding base prompt/plugin prompt，避免继承工程师角色。
无工具之外，启动 hooks、插件仍需审核/关闭，不能仅看工具列表。

沿用 CLI 的正常登录机制，禁止导出/打印/复制 token、伪造官方客户端或绕过服务限制。
只传必要教学文本、匿名上下文与粗粒度视觉事实，不传源代码、完整原始资料、
学号、个人照片或连续画面。
远程调用意味着这些文本会离开本机，“CLI 在本机”不等于“模型在本机”。

### 9.4 协议、流式和失败

同时异步读取 stdout 与 stderr，防止管道塞满死锁。
stream-json 先解析外层协议，再提取 assistant 内容，再解析应用 JSON；
不是每行都是 TutorReply。
该格式可能是一条完整 assistant message，而非 token delta；
没有 token streaming 时显示真实等待状态，不逐字打字伪装网络流。
不要展示思考内容或直接把 stderr 丢到儿童界面。

协议适配测试覆盖分片 UTF-8、空行、非 JSON 诊断、多个 assistant/tool 事件、
错误退出、退出前部分输出、超时、取消。
工具调用一旦出现，应当 fail closed，标记安全配置失败，不执行或继续播报。
只有验证后的当前轮回复可交 UI/TTS。
schema 失败允许一次有界格式修复，仍失败则请求重试，不从任意日志中捞文本。
设置启动/首输出/整轮 deadline、最大输出长度、最大并发和有界重试。
禁止无限循环修复、无限重启和不可见重复付费调用。

### 9.5 API 备用

保留独立 direct_api provider 接口。只有用户明确提供合法 API 配置并启用时才使用。
不得从 CLI 登录信息“提取 API key”。CLI 不可用时，诚实报错，离线时保留视觉和本地状态。
mock backend 仅供自动测试，不能在产品里无标记代替 Kimi。

### 9.6 CLI 自带历史/日志

调查该版本 session/transcript/log 存放位置、保留与删除机制。
应用“不保存录音”不等于 CLI 不保存对话。
产品需要记录自己创建的 provider sessions，纳入会话删除策略；
只能清理本应用拥有的记录，不能删除用户既有 coding 历史。
无法关闭某类日志时在教师诊断界面说明，最小化传输内容并实施可验证清理。

## 10. Tutor 智能体与教学策略

### 10.1 ContextBuilder

只组装必要内容：
固定教学政策 → 当前活动知识/安全边界 → 用户明确设置的语言和帮助粒度 →
带来源的当前任务事实 → 快记忆 → 本活动相关慢记忆 → 当前用户输入 →
输出 schema。

源码和内部设计文档不应作为每次学生对话的背景全文传入。
普通儿童输入、记忆文本、外部任务证据都作为数据，不可覆盖系统政策。

### 10.2 Prompt 分层

- teacher_core：友好、清楚、少量帮助，支持孩子观察与解释。
- grounding：只依据已有输入；区分报告、确认、模拟、当前与历史。
- activity：模块功能、可教概念、推荐观察步骤，禁止直接执行硬件。
- learner_style：明确提供的语言、粒度、既有知识，不推断敏感属性。
- interaction：一次一件事，短句；根据请求适量直接解释，避免永远反问。
- safety：不收集不必要个人信息、不索取照片、不让儿童执行危险物理操作。
- output：短回复、阶段/记忆候选、证据 ID；不返回任意命令。

默认单轮 1–3 句话、最多一个核心追问，作为教学默认值而非机械字数裁切。
儿童明确要求答案时可给适龄解释；连续卡住可提高帮助级别。
没有情绪模型仍可以对孩子明确说出的“我不想继续/我累了”作正常语言回应，
但不得声称通过脸判断出状态。

### 10.3 活动状态机

建议阶段：
goal_setting → inventory_check → capability_gap → build_guidance →
test_preparation → observation → comparison → reflection → paused/completed。
可回退、可跳过、可变更目标；状态机约束记录，不强迫模型念脚本。
硬件执行状态独立于教学阶段。给了指导不代表执行成功。

### 10.4 模型输出

建议完整结构：
schema_version、request_id、reply_text、teaching_move、
task_update_candidates、memory_candidates、used_evidence_ids、needs_clarification。
schema 的 additionalProperties=false；禁止 shell_command/ptz/actuator_command。
阶段和记忆更新只是候选：后端验证证据、scope、generation 和允许字段后应用。
不索取或保存长篇隐式推理。教师可查看简短的“使用了哪些证据/本轮教学动作”。

### 10.5 回复投递语义

同时记录 generated、shown、speech_started、speech_completed、cancelled。
生成但未显示/播出的回复不能在下轮被描述为“我刚才已经告诉你”。
半途打断时保留已投递状态；重试不重复提交记忆或任务状态。

## 11. 快慢记忆：本项目的重要能力

### 11.1 快记忆

每个 session 独立，保留：
当前目标与阶段；最新实际输入；近期若干轮（初始如 6 轮）；
已尝试/已报告结果；上一条真正投递的提示；待确认问题；
当前用户偏好覆盖值；当前任务证据及时间；视觉目标状态。
它是结构化活动状态＋短窗口，不是简单固定字符裁剪。
快记忆可持久化检查点用于崩溃恢复，但不因此自动变成长期事实。

### 11.2 慢记忆

按 learner_id + activity scope 存储：
已确认/明确报告的阶段进展，重要试验变化，明确偏好，待做实验，教师更正。
每条至少含 memory_id、kind、content、learner_id、activity_scope、
source_event_ids、source_type、created_at、updated_at、status、supersedes、
适用范围/失效条件。
不存身份证明式人脸数据，不把当次沉默写成“这个孩子注意力差”。
“这次一次一个提示”只对本次适用；“以后都一次一步”才是可跨会话偏好。
用户报告不是硬件验证，摘要不改变证据等级。

### 11.3 写入流程

有效用户事件与已投递回复记入事件日志；
每轮用 reducer 更新快状态；
明确的纠正/记住/忘记请求即时处理；
活动结束、阶段转换或若干完成轮次后生成整理候选；
验证来源存在、同 learner、非 simulated 混入真实、非幻觉；
事务性 merge、supersede 或 reject；生成摘要和审计索引。

第一版规则处理明确字段，可用低优先级 Kimi 辅助摘要；
每次整理最多一次请求并计入成本，不为每个视频帧或每轮都调用第二个模型。
模型不可直接写 SQLite 或文件。
后台整理期间用户更新/删除时用 version check/tombstone，旧 job 不得把信息写回来。

### 11.4 召回

先 learner_id/活动 scope 过滤，再匹配当前目标/阶段/关键词；
优先未完成步骤、最近有效进展、明确仍适用的帮助偏好；
初始 top_k=5，可配置；token/长度预算硬上限。
第一版 SQLite＋结构化筛选即可，不必先上向量库或 RAG 框架。
若用全文检索，注意中文词切分，不能以默认英文分词造成中文召回形同虚设。
无相关记忆返回空，不编造相似历史。

### 11.5 冲突与事实优先级

不是笼统“最新一条覆盖全部”。按同一事实/同一时间范围比较：
当前真实硬件观测支撑当前连接状态；
当前用户报告支撑其口述现象；
历史只能支撑以前；
assistant 建议不能证明动作完成；
simulated 永远不提升为真实。
同一偏好当前明确更正优先；保留历史出处但不继续应用旧值。
新会话必须重新确认硬件配置，不从慢记忆恢复成 current_connected。

### 11.6 隔离、删除和可见性

查询强制 learner_id 过滤，缓存 key 也带 learner/session。
模拟学习者使用单独命名空间/数据库。
教师可看本轮召回哪些条目、从哪里来，可纠正或忘记。
忘记需要覆盖 DB、派生摘要、缓存、待写队列、应用所属 CLI transcript 和导出副本；
不能只删 UI 一条记录，却从旧摘要再次恢复。
备份若存在要说明清除策略，原型默认不自动做未治理的长期备份。

## 12. 积木任务输入和未来集成

定义 TaskStateProvider：
get_current_state(activity_id)、subscribe_events、capabilities、source_mode。
本轮实现两类：
- manual_report：儿童/教师输入，标记 learner_report/operator_confirmed；
- simulator：教师按钮触发明确 simulated 的连接/试跑事件。

real_hardware adapter 暂留协议，不返回假连接数据。
模块字段使用 control/motor/distance/light，与小组功能模块对应；
具体板卡、端口、总线协议不在资料不足时捏造。
将来接入拓扑系统时增添 capability、module ID、连接状态、sensor reading、
timestamp、device epoch、safety validation 状态。

Tutor 只提出教育指导或待审核行为意图；
小组验证层通过后才能交给执行层。第一版 actuator_execution=false，不能有隐式直通。
缺少传感器的模拟例子不由人脸模型发现，而由 TaskStateProvider 明示来源。
内置知识可解释模块用途，但不知道当前是否插好。

## 13. 成本、延迟与观测

成本设计：
本地 YOLO/ASR/TTS，远程教学推理；
按明确用户输入触发，而非脸框抖动触发；
短上下文＋相关记忆；无工具、不自主派生 agent；
有界重试；整理请求低频且可观察。
Kimi CLI 可能使用订阅额度，不能假设“本地 CLI 免费无限”。

记录应用请求次数、provider 报告 usage（可得时）、调用时长、
context 估计长度、整理调用、错误/重试/取消次数。
没有真实 usage 时 reported_tokens=null，estimated_payload_tokens 另列；
不能把发出的 payload 长度当成供应商实际计费量，CLI 可能添加自己的开销。
计费价与额度不写死，实测前不宣布节省百分比。

初始工程目标（不是现有性能声明）：
- 可用 preview ≥20 fps、检测/跟踪 ≥10 fps；
- 摄像头到显示帧龄 p95 <250 ms（目标档位，具体降级如实报告）；
- UI 点击/停止无明显阻塞；
- 文字/语音分别报告 time-to-first-text、ASR、完整回复、首音频 p50/p95；
- 网络时间与本地时间分离，不承诺固定 1 秒；
- 30 分钟运行内无持续无界内存增长，资源关闭可回收。
PTZ 停止和取消不依赖远程网络。

## 14. 数据、API 与目录

### 14.1 持久化表

learners、sessions、activities、turns、task_events、fast_snapshots、
memories、memory_provenance、memory_jobs、provider_runs、deletion_tombstones。
SQLite 写入用单写者/事务与版本检查；具体 WAL 设置在本机核验。
对话投递状态、来源、generation 必须有字段，不能仅存 role/content。

### 14.2 应用接口（本规格建议，非第三方现成接口）

GET /api/health
GET /api/devices/cameras
POST /api/camera/open
POST /api/camera/close
POST /api/tracking/select
POST /api/tracking/clear
POST /api/ptz/enable
POST /api/ptz/stop
POST /api/learners
POST /api/sessions
POST /api/sessions/{id}/turns
POST /api/sessions/{id}/cancel
POST /api/audio/record/start
POST /api/audio/record/stop
POST /api/audio/playback/stop
GET /api/sessions/{id}/context
GET /api/learners/{id}/memories
PATCH /api/learners/{id}/memories/{memory_id}
DELETE /api/learners/{id}/memories/{memory_id}
POST /api/activities/{id}/events
POST /api/sessions/{id}/finish
WS /api/events
GET /api/video/preview

WS 事件包括 camera.status、vision.frame_meta、tracking.updated、target.changed、
asr.final、tutor.started、tutor.reply、tutor.cancelled、tts.status、memory.updated、system.error。
所有会话事件附 learner/session/request/generation；预览帧与框以 frame_id 对齐。
WS 与图片端点均有本地访问保护；不要因 MJPEG img 标签方便而泄露无鉴权画面。

### 14.3 推荐目录

ai-tutor/
  README.md
  pyproject.toml
  uv.lock 或实际锁文件
  configs/default.yaml
  prompts/tutor_core.md
  prompts/activities/modular_rover.yaml
  prompts/private/                 # 忽略提交，保留现有私有 prompt
  src/ai_tutor/
    api/
    camera/{discovery,capture,capabilities}.py
    vision/{detector,tracker,target_manager}.py
    ptz/{base,drivers,controller,watchdog}.py
    audio/{capture,asr,tts,playback}.py
    agents/{base,kimi_cli_legacy,kimi_code,direct_api,mock}.py
    tutor/{orchestrator,context_builder,policy,task_state}.py
    memory/{fast,store,retrieval,consolidation,validation}.py
    events/
    storage/
    observability/
  frontend/
  models/manifest.json
  tests/{unit,contract,integration,e2e,hardware}/
  scripts/
  docs/{DISCOVERY,RESEARCH_LOG,ARCHITECTURE,ACCEPTANCE,KNOWN_LIMITATIONS}.md
  runtime/                       # 不提交，不作为 coding agent 工作目录

核心实体接口见 schemas。文件切分按仓库实际规模调整，避免为每个按钮发明微服务。

## 15. 安全与隐私边界

- 默认为成人/团队内部演示；儿童实际参与前取得相应监护与学校流程批准。
- 摄像头/麦克风启动有显式状态；默认不录制原始视频，不保存人脸裁剪和声纹。
- 原始音频仅在转写需要时短暂存在，用后清除；诊断录制需单独明确开启。
- 仅发送必要教学文本到远端；显示 remote reasoning，而不是 local AI 的误导标签。
- 匿名档案，按用户选择持久保存必要记忆；不传学号和原始开题文档。
- 服务只在本机；随机会话 token、Origin 检查、无通配跨域，防止网页驱动本机设备。
- 限制输入长度、请求频率、上传大小；网页渲染输出使用转义，无不受控 HTML。
- 子进程禁止 shell 拼接，禁止用模型文本生成控制命令。
- 下载依赖/模型来自核验来源；记录摘要，不从不明网盘加载可执行权重对象。
- 不发布源码、不推送公共仓库、不把私有 prompt 放到浏览器可下载 bundle。
- 不修改全局 Kimi 设置，不删除其他会话，不杀其他用户应用。
- 权限弹窗、SDK 申请、必要登录或不可访问硬件如实列为 HUMAN_ACTION_REQUIRED；
  这不允许以“全任务受阻”为由不做其他可完成部分。

## 16. 实施顺序与阶段出口

P0 / 盘点：
真实环境和设备报告，Kimi 版本与无工具能力验证，计划复用的代码；
摄像头首帧和一次无副作用真实 Kimi 回复。
若 Kimi 尚未满足无工具隔离，不得接入真实儿童输入；连通性测试仅用开发者固定无害 prompt。

P1 / 视觉：
真实视频、face 模型、框 ID、选择/丢失/恢复；
测试镜像坐标、多人交叉、重连；禁用未核验 PTZ。

P2 / 教学文字垂直闭环：
学习者/会话/任务；真实 Kimi、教学 prompt、来源校验、UI；
不能先做一堆空 provider，而没有真实一问一答。

P3 / 记忆：
快状态、持久慢记忆、来源、纠错/删除、重启续接、跨学习者隔离；
用小车两次活动做真人输入验证，不只单元测试。

P4 / 语音：
真实麦克风、本地 ASR/TTS、按键说话、转写修订、取消和半双工；
最终完成 CORE_VOICE。

P5 / PTZ：
已确认能力时实现并实测；无法获得接口则提供证据、明确状态，
保持软件追踪及交互运行，不能以数字裁切替代验收。

P6 / 集成打磨：
模拟/真实来源隔离、成本/延迟、崩溃恢复、30 分钟运行、教师演示脚本、
一键启动、依赖锁定、测试证据和已知问题。
达到每个阶段出口再推进；任务长时维护 TASKS.md 和明确续接点，不宣称未做内容完成。

## 17. 验收原则

详细用例在 ACCEPTANCE.md 和 examples/acceptance_scenarios.yaml。
必须同时有无硬件自动测试、真实设备验证、真实模型连通和公开可解释演示。

测试结果四种状态：
PASS（已执行通过）、FAIL（已执行失败）、BLOCKED（明确外部阻塞）、NOT_RUN。
“代码存在”“测试被跳过”不等于 PASS。
每条硬件结果附设备、实际版本、配置、日期和执行日志（不含机密）。

## 18. 最终交付

运行应用；一键启动及关闭；依赖与模型清单；设备能力报告；
Kimi 版本适配/无工具证明；用户手册；架构与数据流；
可检查快慢记忆；全部测试和结果；模拟教学活动；已知限制；
无 Haonan 模型时依然可用的完整 Tutor。

Codex 的最终答复必须说明：哪些已经真实运行、哪些只测了 mock、
PTZ 到底是否转动、语音是否本地、使用哪个 Kimi 版本、如何启动、
摄像头/会话/记忆如何切换以及需要人手的最小操作。
