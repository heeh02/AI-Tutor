# AI Tutor 实施记录

## 当前 Goal：实时中英教育助手升级（2026-11-03）

用户已审核方案并授权开始。Goal 为 ACTIVE，完整范围和验收条件见 [Goal 执行文档](docs/GOAL_REALTIME_EDUCATION.md)。覆盖 P0–P5：真实模型基线、实时自然语音、摄像头追踪、教育快慢记忆、分层双语教学与工作台、并发/恢复/回退和实体云台验收。主程序保持 Rust，允许隔离的本地推理服务。现场运动仍遵守此前暂缓安排；未验项目不标通过。

2026-11-04首个检查点：目标局部追踪歧义、独立预览、记忆完整检索与来源版本、独立教学偏好已实现；312项回归通过、原生与WASM构建通过；真实模型基线揭示混读和首音延迟缺口。新阶段尚未安装，Goal继续推进，详见 [实施进展](docs/GOAL_PROGRESS_20261104.md)。

## 2026-11-03：软件全面审查与优化

- 已阅读原始实现规格、当前 Rust 架构与教学/设备验收，完成后端、界面、启动和音视频并发审查。
- 已修复同项目重复启动、教学投递/记忆部分写入、SQLite 提交失败恢复、旧录音请求干扰新录音、麦克风断开残留状态、发送失败覆盖新草稿及迟到记忆列表问题。
- 已优化会话历史查询索引、空闲视频轮询与视觉元数据读取，保留现有教学和云台保护规则。
- 最终 workspace 回归 **261 通过、0 失败、1 项真实付费 Kimi 探针忽略**（较此前 242 项新增 19 项）。WASM release 打包和安装回滚检查通过。
- 原生 release 构建完成，4 个程序与 5 个界面文件已同步安装，逐文件 SHA-256 和安装后帮助入口检查通过；交付清单 `runtime/optimization-release-20261103.json`，旧程序/界面保留可回退。
- 详细改动、交付记录和仍待现场验收项见 [本轮优化报告](docs/OPTIMIZATION_REPORT_20261103.md)。以下使用状态、PID 和测试数均为历史记录。

最新使用状态（2026-11-02）：用户表示“现在不方便继续测试”，本轮现场测试已结束，云台模式off，不自动重新启用。新版实体水平跟随的人眼确认仍pending。后文FOLLOWING状态均为先前验收快照；最新控制记录见 `runtime/ptz-user-deferred-20261101T175907Z.json`。

## 最新：用户恢复实体跟随修复

2026-11-02最终发布：软件回归242项PASS、0失败、1项付费探针忽略。真实双轴方向校准PASS_DIRECTION_ONLY（run65b33c8c-e8c3-4cb2-bc47-8c279abf313b），旧失败报告／日志／锁保留。生产跟随每轴≤3°／速度4，标定仍1°／速度1；250ms发送新帧要求不变。已加入同采集owner读取、项目内缓冲区解锁修复、Rust JPEG解码、分段停滞诊断、独立选择代次及600ms短暂视觉暂停恢复；WASM暂停界面仍可停止。

当前PID115048，成人档案已恢复，最后快照FOLLOWING。已实测有限目标到位、短暂失踪后取消并恢复、本地停止／重新启用；最后水平目标Pan24→26读回26，暂停／恢复各2次、stop_fault=false。跟随约237秒后因超过600ms失踪按规则停；独立视频五分钟测试无重连或诊断事件，不等同PTZ五分钟验收。新版实体水平跟随的人眼确认仍pending，旧版“仍然没有转动”反馈未改写；完整物理急停、拔插、30分钟运行仍NOT_RUN。详情 `docs/PTZ_TRACKING_REPAIR.md`、`runtime/ptz-follow-release-manifest.json`。以下均保留历史版本与证据范围。

## 当前交付：全面 Rust

用户明确要求全面使用Rust，不比较语言效果。已完成生产迁移：`rust/ai-tutor`为原生服务/设备/语音/Kimi/记忆，`rust/tutor-ui`为Rust/Yew WebAssembly。旧Python与TypeScript原型已归档，不作为当前运行程序。

## 最新：教学流程与 CLI 发布

- PASS：最终workspace **191通过、0失败、1项付费探针忽略**；证据 `runtime/product-workspace-tests.json`。
- PASS：修复报告-only清空模块、未保存观察的活动收尾、旧活动复活、选择性记忆更正、模型更正后旧事实再次写回；当前更正回复仍可显示与播报。
- PASS：全面Rust控制入口 `bin/tutorctl.exe` 已发布，助手只通过CLI/本地API控制，界面交给使用者。CLI10项及实际进程合同4项、UI17项已计入总数。
- PASS：首轮三次真实Kimi与7项流程检查；修复中文更正召回后，当前程序实际重启与另一次真实CLI续接通过。资料严格为simulator合成输入；新活动重新确认当前模块。首轮重启发现的召回缺口另存FAIL证据，未改写历史。
- PASS：程序、界面和CLI已部署；PID81696，应用SHA `c53c404f92dfb4481530531b6d57729adbcc9f373a269dd08d42f0d58ca6b778`。恢复原成人档案并通过启动器CLI打开工作台，摄像头关闭、音频空闲、PTZ off/BLOCKED。旧程序与数据库已备份。
- 按用户最新要求，物理云台暂缓。用户已确认唤醒；原FAIL/journal/latch归档保留，没有再开始运动测试。新版本30分钟持续运行与完整现场故障场景NOT_RUN。

详见 `docs/PRODUCT_WORKFLOW_REPORT.md`、`docs/CLI.md`、`runtime/product-release-manifest.json`。以下设备阶段记录是历史快照，其PID、测试数量与Wake pending不代表当前状态。

## 先前设备阶段快照（历史）

- PASS：最新Rust workspace回归140项通过、0失败、1项付费Kimi探针忽略（本轮未重跑付费模型）。分组为内核112、标定安全10、原生只读探针/CLI6、资源监测2、HTTP/SQLite2、Rust UI8；陈旧预览后端2项和UI8已包含。证据runtime/rust-ptz-workspace-tests.json；不替代实机物理跟随。
- PASS：Rust原生摄像头发现、持续读取与重开、真实ONNX检测推理、真实ByteTrack；辅助进程Job回收及启动握手测试。
- PASS：Rust真实WASAPI采样、本地SAPI播放/停止、半双工与过期请求保护、本地中英文合成样本识别。没有将合成语音当作成人现场测试。
- PASS：最终Rust release连续1803秒，无摄像头/请求错误；实机停止释放主进程、摄像头与端口，再由启动器重开。
- PASS：2026-11-01用户确认“转写与播报都正常”（source_type=user_confirmation），覆盖基本成人按键说话→检查或编辑转写→发送→听回复流程及可听播报；独立记录，不改写历史自动探测。
- NOT_RUN：G01完整中英文验收、英语自然语音、现场打断、声学回声拒绝及播报中切档案；用户反馈未明确覆盖这些子项。
- PTZ历史FAIL：早期UVC 1° PAN试验出现未命令TILT漂移，证据保留。新官方Center2.0.15.186已提取运行、UDP16284只读/握手/显式Tiny3两int profile已实测；用户已确认两个AI总开关关闭。Rust有限位置门禁、停止partial回执、持久故障锁与显式actor已实现，但新的实体方向、跟随及停止尚未验收，仍off/BLOCKED，不能写物理PASS。
- PTZ预检：v1固定mask为BLOCKED_NO_MOTION；动态两张不同帧、双独立ROI预检为PASS_NO_MOTION_ONLY（命令0、latch=false），只证明背景信息可用。新的低幅标定结果须独立追加，不覆盖旧FAIL。
- PTZ本轮结果：v2-final真实尝试为FAIL_NO_RETRY/osc_motion_pose_changed_reverify，源代码确认非零0；记录同步4+异步4零值提交，物理停止未确认。FAIL、journal与latch全部保留，未重试/清除。
- PTZ随后只读：OSC20次[17,84]、同窗原生12次17/−83；后续握手connected=true/running=false（Sleep），应用error、frame_age340525ms。Sleep是随后观察，不能认定为失败瞬间根因；边界抖动假说未证实。用户Wake反馈pending，任何新非零禁止，physical_follow保持BLOCKED。
- 当前release：PID87904／SHA f493b96779e03f27ce0c468fd3670bc34620eaf3958d08582f5a786c1ea935fc已部署，成人档案/工作台恢复；Sleep状态下未auto-open相机，GET video/frame为jpeg=null、tracks0，modeoff/gateblocked，旧latch保留。用户Wake仍pending。
- 部署历史：C368／PID90324仅为阶段版；旧1803秒实测仅对应469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c，未套用到新release。
- NOT_RUN：多人交叉/遮挡、真实USB拔插、完整浏览器自动化（工具无法确认浏览器URL而停止）。

当前入口为`Start AI Tutor.cmd` → `bin/ai-tutor.exe`，停止为`Stop AI Tutor.cmd`。权威结果见`ACCEPTANCE_REPORT.md`与`docs/RUST_*`。

## 前期实现记录（历史）

日期：2026-11-01；本目录原先只有规格 ZIP 与两份 Markdown，没有代码仓库或未提交改动。

| 阶段 | 当前结果 | 剩余验收 |
|---|---|---|
| P0 环境盘点 | Windows11、Python3.10、32GB、RTX5070、OBSBOT Tiny3、Kimi2.3.1 已实测；依赖放项目 `.venv` | 云台 SDK/Center 控制接口待取得 |
| P1 视觉 | 原生稳定设备路径、真实连续视频、YOLO face ONNX、显式 ByteTrack、选择/丢失/重连代码与坐标测试 | 多人交叉、遮挡、实际拔插人工测试 |
| P2 文字教学 | 专用无工具 Kimi agent、真实连通及 canary 测试通过；工作台和服务已启动 | 连续小车场景真实 Kimi 联调正在执行 |
| P3 快慢记忆 | SQLite 分空间、来源与 epoch 校验、实际投递确认、中文召回、纠正/删除、恢复测试 | 真实小车两次会话和重启验证正在执行 |
| P4 本地语音 | OBSBOT 麦克风真实读入、Windows中英文声音、本地 Whisper base；离线合成样本转写与播报打断通过 | 成人真人按键语音教学对话 |
| P5 云台 | 已查 Tiny3 官方 SDK 与 Center OSC；当前保持 off、没有运动指令 | BLOCKED：本机缺官方可调用控制端；方向和故障保障未验证 |
| P6 综合 | 自动测试、生产前端构建通过；启动/停止脚本已运行 | 30分钟真实摄像头运行正在记录；完整浏览器交互检查受工具阻塞 |

持续证据在 `docs/`。原始验收标准保留于 `implementation-package/AI_Tutor_Codex_Implementation_Package/`。

实现边界：不接情绪模型、不识别积木、不执行小车电机、不识别人脸身份、不判断说话者、不发送图片/声音给 Kimi。教学文字调用远程模型。
