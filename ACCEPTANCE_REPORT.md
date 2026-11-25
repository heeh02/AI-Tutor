# AI Tutor Rust 版验收

## 最新软件检查：2026-11-03

本轮软件优化后 workspace **261 项通过、0 失败、1 项真实 Kimi 付费探针忽略**；Rust/WASM 界面正式打包及安装回滚测试通过。覆盖启动单实例、本机请求隔离、投递与记忆事务、录音批次绑定、麦克风故障清理、界面并发与草稿保护、空闲轮询和视觉元数据读取。验证细节与交付结果见 [本轮优化报告](docs/OPTIMIZATION_REPORT_20261103.md)。

原生 release 程序与 WASM 界面已同步安装，9 个交付文件校验一致；安装后的启动器和 CLI 帮助入口通过。版本摘要与旧版本备份位置记录于 `runtime/optimization-release-20261103.json`。

本轮未重跑真实模型、设备和浏览器现场操作；不新增物理能力验收结论。下列 2026-11-01/02 的 PID、软件测试数和设备状态是其对应版本的历史证据，不能当作当前运行状态。

最新使用状态（2026-11-02）：用户表示“现在不方便继续测试”，本轮现场测试已结束，云台模式off，不自动重新启用。新版实体水平跟随的人眼确认仍pending。后文FOLLOWING状态均为先前验收快照；最新控制记录见 `runtime/ptz-user-deferred-20261101T175907Z.json`。

日期：2026-11-01，Asia/Shanghai。按用户最新要求，生产应用与界面逻辑全面使用Rust；不做语言效果比较。

最新更新（2026-11-02）：软件回归242通过、0失败、1项付费探针忽略，Rust原生服务和WASM界面均已部署（PID115048）。实机已验证有限目标到位、短暂丢失后取消旧动作并恢复、本地停止与重新启用；最后快照为external_yolo/FOLLOWING，已发生2次暂停/恢复，水平原生读回Pan26。连续约237秒的跟随窗口最终因目标丢失超过600ms按规则停止，不能称为五分钟PTZ通过；独立视频五分钟测试为PASS_CAPTURE_ONLY。旧版用户反馈仍保留，新版可见实体水平跟随尚待用户确认，完整物理急停／拔插／30分钟运行未验收。权威版本与范围见 `docs/PTZ_TRACKING_REPAIR.md`、`runtime/ptz-follow-release-manifest.json`。以下教学发布与早期设备状态均为历史快照。

**已发布完善教学流程的Rust版本，不能将它表述为所有现场项目全部通过。** 最终workspace191项通过、0失败、1项付费探针忽略；真实Kimi完整流程3轮/7项检查通过，修复中文更正检索后当前程序的实际重启及另1次真实CLI续接亦通过。已交付助手使用的Rust CLI，用户继续使用工作台。当前PID81696、程序摘要和范围见 `docs/PRODUCT_WORKFLOW_REPORT.md` 与 `runtime/product-release-manifest.json`。

结束保存、跨活动召回、模块报告与记忆更正已补齐并实际联调。基本成人语音此前已获用户确认；本轮未重复设备测试。用户已确认唤醒摄像头，后又明确优先完善系统、暂缓云台，当前PTZ关闭且物理验收仍BLOCKED。原FAIL/journal/latch已归档保留。以下详细表保留先前设备阶段证据；旧PID、140项计数与Wake pending属于历史快照。

## 实际运行环境与证据

- Windows11 x64，i5-14600K、约32GB；本轮推理使用CPU，未声称启用RTX5070加速。
- Rust1.98.1／Cargo1.98.1，MSVC14.44.35207，Windows SDK10.0.26100.0。
- `bin/ai-tutor.exe`：原生Rust/Tokio/Axum服务；Rust/Yew WebAssembly界面在`frontend/dist`。
- 实际设备：OBSBOT Tiny3 StreamCamera及其WASAPI麦克风。
- face专用YOLO ONNX、原生ONNX Runtime1.23.2、真正ByteTrack；本地Whisper多语种base、Windows SAPI。
- 真实Kimi Code2.3.1：独立无工具agent、正常OAuth、原生进程管理；教学文字远程，画面与录音不发送远程。
- 关键证据：`docs/RUST_KIMI_REPORT.md`、`docs/RUST_E2E_REPORT.md`、`docs/RUST_VISION.md`、`docs/RUST_AUDIO_REPORT.md`、`docs/PTZ_REPORT.md`。
- 持续运行日志：`runtime/soak-rust.jsonl`；进程资源日志：`runtime/rust-resources.jsonl`。资源监测从预热后约386.8秒开始，不能声称覆盖启动峰值。

自动测试、真实模型调用、真实设备采样、合成语音、现场人工操作分别标明。测试脚本的小车结果严格在simulator空间保存，不是实体小车运行证据。

## 已执行的主要检查

| 检查层 | 当前结果 |
|---|---|
| 最新Rust workspace回归 | 140项PASS、0失败、1项付费Kimi探针忽略；分组证据runtime/rust-ptz-workspace-tests.json。此轮未重复付费模型调用，不能替代物理跟随验收 |
| Rust服务/内核自动测试 | 112项通过，含2项陈旧预览后端保护；已计入140项。真实CLI探针此前单独执行通过 |
| 标定安全与只读辅助程序 | 标定安全10项、原生只读探针/CLI6项通过，已计入140；仅合同/fixture验证 |
| 原生HTTP/SQLite集成 | 2项通过：认证/Origin/WS/尺寸/非法字段，以及投递、取消、隔离和删除epoch |
| 原生子进程I/O合同 | 4项已计入内核测试：真实匿名管道双向排水、逐字节UTF8、双向输出超限、30ms合同超时和回收；生产二进制SHA未变 |
| Rust界面模型测试 | 8项通过，含陈旧预览状态；已计入140，不当作浏览器现场验收或最新程序已部署证明 |
| 原生资源监测逻辑 | 2项进程树测试通过；对运行程序进行只读采样 |
| 真实Kimi无工具验证 | 正常中文回复9007ms；合成秘密诱导8322ms，秘密未泄露、工具事件0、usage=null |
| 真实Rust多轮教学 | 全部脚本检查通过；9次实际教学轮次及1次取消，均为模拟档案和模拟报告 |
| 实际Rust进程重启 | 历史进展与待做移动墙实验可召回，当前配置unknown，B档案仍空 |
| 原生摄像头 | 实际读帧与关闭重开通过；正式release连续1803秒、358条运行采样，无摄像头/请求错误 |
| 真实麦克风/SAPI | 实际采到95040个样本／1980ms；静音不产生文字；SAPI完成、停止、半双工、过期拒绝通过 |
| 本地识别模型 | Rust COM生成的固定中英文临时语音识别通过；临时WAV删除，中文有同音字需编辑 |
| 基本成人按键语音闭环／可听播报 | PASS：2026-11-01用户确认“转写与播报都正常”，source_type=user_confirmation；范围为按住说话→检查或编辑转写→发送→听回复，语言未单独标注 |
| 完整中英文及故障场景语音验收 | NOT_RUN：英语自然语音、现场打断、回声与播报中切档案尚未明确确认 |
| 物理云台 | 旧UVC试验FAIL保留；新OSC尝试FAIL_NO_RETRY/osc_motion_pose_changed_reverify，非零0、同步4+异步4零值提交。随后确认Sleep，物理方向/跟随/停止未通过 |

常规检查入口：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test --locked -p ai-tutor
cargo test --locked -p tutor-ui --target-dir target/ui
powershell -NoProfile -ExecutionPolicy Bypass -File frontend/build.ps1
```

真实模型/设备检查通过Rust辅助程序执行。常规测试里的模拟进程、合成事件与坐标数据只证明相应契约，不替代硬件或真人验收。

## 原始验收清单逐项对照

状态仅使用PASS／FAIL／BLOCKED／NOT_RUN。PASS旁的验证层说明它实际证明什么；未完成的现场子项明确列出。

| 编号 | 状态 | 实际证据与边界 |
|---|---|---|
| A01 | PASS | Rust界面/活动/协议使用control、motor、distance、light功能模块；没有MOVE/TURN卡片模型。完整页面人工操作仍单列待验。 |
| A02 | PASS | 运行状态emotion.enabled=false；没有情绪分数、理解力或注意力接口。 |
| A03 | PASS | 真实Rust多轮与重启测试保留simulated来源；手动来源契约另测。 |
| A04 | PASS | 没有将旧PPT、动画、原方案状态当作实机能力；旧原型已归档。 |
| B01 | PASS | Rust原生Media Foundation发现实际Tiny3名称、稳定symbolic link和可协商格式。 |
| B02 | PASS | 原生实际连续帧与重开，以及正式程序实时运行日志；不是静态图片。 |
| B03 | NOT_RUN | 关闭释放与重开实际通过；权限拒绝、其他程序占用和真实拔插未全部现场执行。 |
| B04 | NOT_RUN | 稳定ID重建映射及新采集周期已实测；拔插后索引变化仍需现场验证。 |
| B05 | PASS | face类、SHA256、输入输出、作者与版本、权重AGPL元数据均记录；未声称数据集再利用授权或商业许可已完全清理。 |
| B06 | PASS | Rust原图坐标/letterbox/奇数补边与镜像数值测试通过；浏览器缩放现场操作未替代为硬件PASS。 |
| B07 | NOT_RUN | 显式选择/不随最大脸变化的Rust测试通过；真实两人同框选择未执行。 |
| B08 | NOT_RUN | 重叠歧义清除、旧目标拒绝有自动测试；真实交叉/短遮挡/长遮挡未执行。 |
| B09 | PASS | 实际重开产生不同camera_session_id，旧epoch/旧目标选择被拒绝；不恢复旧人脸绑定。 |
| B10 | NOT_RUN | 真实Kimi等待时摄像头正常更新；尚未强制90秒模型超时同时进行现场视频测试。 |
| B11 | PASS | 摄像头目标API不创建/切换学习者；原生HTTP及选择测试证明档案和track分离。 |
| C01 | PASS | IAMCameraControl只读与官方Center2.0.15.186/UDP16284握手已实际读取；显式Tiny3两int profile有只读交叉核验。随后状态记录不当作持续实时状态。 |
| C02 | FAIL | 历史UVC PAN试验的未命令TILT漂移保留。新OSC v2-final尝试因osc_motion_pose_changed_reverify在编码/UDP发送前拒绝，非零0、FAIL_NO_RETRY及latch保留；尚无通过的物理方向标定。 |
| C03 | BLOCKED | 用户确认两个厂商AI总开关关闭，实时lock0与确认来源分开；随后握手connected=true但running=false（Sleep），旧预览过期。Wake反馈仍待用户，不能再发非零。不能断言Sleep触发失败瞬间。 |
| C04 | BLOCKED | 显式有限位置actor及软件保护已实现测试，尚未完成真实物理跟随；没有用裁切或软件框替代。 |
| C05 | BLOCKED | 本次同步4+异步4零值均有本地提交回执，物理停止仍未证实；丢失/旧帧/USB/急停的实体保障未验收。 |
| C06 | BLOCKED | 没有提供持续速度命令；实体失控停止保障未验，因此不开放该模式。 |
| C07 | BLOCKED | 云台与其他链的联合运动验收未进行。 |
| D01 | PASS | 实际CLI版本/路径/help、SHA及匹配协议；未升级或改写全局配置。 |
| D02 | PASS | Rust直接调用真实认证Kimi，多轮不同输入回复成功。 |
| D03 | PASS | agent tools/subagents显式空；隔离技能/MCP/hooks/插件；实际无工具与严格协议验证通过。 |
| D04 | PASS | 真实CLI合成secret-canary攻击未读取/泄露秘密，没有工具事件。 |
| D05 | PASS | 真实攻击含引号、换行、反引号和$()，Rust参数数组不经shell；4000引号/emoji完整参数长度测试通过。 |
| D06 | PASS | 真实Windows匿名管道并发读取512KiB stderr和32KiB stdout，保留退出码7；逐字节UTF8分片及256/128KiB双向上限测试通过。是原生合同测试，不冒充供应商真实故障。 |
| D07 | PASS | 仅显示最终assistant业务文本，不展示reasoning/tool/stderr；没有假装逐token输出。 |
| D08 | PASS | 原生Job取消/异步任务中止和READY后30ms合同超时均回收真实测试子进程并清理目录；生产deadline90秒，不声称强制真实Kimi断网超时已发生。 |
| D09 | PASS | 真实模型生成时切档案，B仍空；原生SAPI停止和过期拒绝单项通过，现场边说边切另见G05。 |
| D10 | PASS | 未核验版本/agent/config失败关闭，生产没有默认coding-agent回退分支。 |
| D11 | PASS | 本应用隔离CLI目录结束清除，NTFS junction先移除；实际取消/中止测试不伤共享凭证或旧编程历史。 |
| D12 | PASS | 真实usage不可得保持null；有界故障分类/无无限重试。未故意耗尽真实额度，不声称实际额度故障已现场发生。 |
| E01 | PASS | 真实Kimi按目标引出模块与下一步，真实对话记录可检查。 |
| E02 | PASS | 缺测距来自模拟清单；真实模型说明其来源，没有声称人脸画面识别积木。 |
| E03 | PASS | 连接报告保留口述/模拟等级，硬件验证始终false。 |
| E04 | PASS | 真实模型和快慢记忆保留首次据报告成功、希望更大间隙。 |
| E05 | PASS | 真实模型聚焦只改停车距离、保持其他条件、比较观察；不执行小车。 |
| E06 | PASS | 真实对话处理偏好、继续、否定与自然更正；不是关键词台词播放器。 |
| E07 | PASS | 候选更新只来自源事件，助手建议不自动成为执行完成。 |
| E08 | PASS | Rust真实模型验收全部使用simulator；real空间拒绝simulated事件。 |
| E09 | PASS | 原生严格业务字段及HTTP契约拒绝shell/actuator/PTZ等额外字段。 |
| E10 | PASS | generated未shown不算已投递，取消后不可确认；native测试通过。 |
| E11 | PASS | 无情绪能力字段，策略只回应明确表达；没有伪造脸部情绪。 |
| E12 | PASS | 实际重启与新活动current modules为unknown，历史来源明确。 |
| F01 | PASS | 真实多轮快状态与上一条实际shown提示一致；播报打断保留已显示语义。 |
| F02 | PASS | 实际Rust进程停止/重启后，真实Kimi继续待做移动墙实验。 |
| F03 | PASS | 重启后先确认当前配置；不把历史接线写为当前连接。 |
| F04 | PASS | 原生集成与真实多轮验证B不读取A对话、偏好或慢记忆。 |
| F05 | PASS | 当次偏好留在session；持久偏好需明确以后适用，自动测试与真实轮次通过。 |
| F06 | PASS | 真实Kimi自然更正引用旧+新来源，旧版本退出active召回；教师更正与supersedes可检查。 |
| F07 | PASS | 删除清除相关来源、派生记忆、暂态对话、快状态和本应用CLI文本；不提供未治理导出/备份。 |
| F08 | PASS | 删除epoch与取消/事务检查阻止旧候选回写；原生回归通过。 |
| F09 | PASS | 中文双字检索及活动/学习者过滤，相关为空不补其他档案。 |
| F10 | PASS | 事件存在、同学习者、来源不提升；历史候选明确标历史并保留原始来源等级。 |
| F11 | PASS | 原生事务回滚、未shown取消、speech_interrupted与实际重启测试；没有强行断电测试。 |
| F12 | PASS | 完整当前输入/必要事实保留，整条选择有界；4000引号/emoji完整CLI参数测试通过。 |
| F13 | PASS | 摄像头更新不触发模型或远程摘要；只有明确文本提交产生教学请求。 |
| F14 | PASS | 查看/更正/删除API经实际Rust/真实Kimi数据验证；浏览器教师操作仍待现场复验。 |
| G01 | NOT_RUN | 基本成人按键语音闭环与可听播报PASS（2026-11-01，user_confirmation：“转写与播报都正常”）；用户未单独标注语言，英语真人麦克风识别尚未明确确认，中英文全项验收未完成。合成中英文样本证据维持原范围。 |
| G02 | PASS | 95040个真实静音样本不生成意图；短输入/旧录音有界拒绝；复杂真实噪声另待测试。 |
| G03 | PASS | Rust界面提供转写编辑；任务词表进入本地模型；固定中英文模型样本通过。 |
| G04 | NOT_RUN | 原生半双工和停止单项通过；实际扬声器回声与PTT打断需成人验证。 |
| G05 | NOT_RUN | 代次/队列/旧转写/旧播报回归通过；真人播报中停止、切档案及旧音频清理场景尚未确认。 |
| G06 | NOT_RUN | 原生语音仅读本地模型/调用系统声音，无远程语音代码；没有另行关闭整机网络进行设备测试。 |
| G07 | PASS | 专属音频线程、单ASR槽位、20秒缓冲、停止/过期/队列满回归与实际采样/播报通过；真实拔插另列。 |
| H01 | PASS | 127.0.0.1监听，Origin/Host/随机cookie/WS/视频保护经原生HTTP契约验证。 |
| H02 | PASS | 不存原始视频/脸裁剪/录音；临时合成WAV删除；CLI日志清理，原生ASR token诊断关闭。 |
| H03 | PASS | 静态目录仅界面资产，内部材料/提示词不由前端下载；模型仅接收必要匿名上下文。 |
| H04 | PASS | 正式Rust release连续1803秒，平均检测27.96fps、服务端帧龄p95=99ms，摄像头/请求错误0；进程工作集和末段私有提交范围已记录，资源采样不含启动峰值/浏览器。详见RUST_STABILITY_REPORT。 |
| H05 | PASS | 旧稳定性构建实机停止确认主进程、摄像头、端口和描述文件释放并重新启动；不将它套用到新F493构建的物理停止验收。新PTZ零值提交不当作实体停止或厂商AI停止证明。 |
| H06 | NOT_RUN | 无无限重试，其他链与Kimi分离；没有切断用户网络或耗尽配额来做现场故障验收。 |
| H07 | PASS | real/simulator独立SQLite及请求来源隔离，真实Rust模拟模型轮次未进入真实档案。 |
| H08 | PASS | 当前电脑从Rust源码实际构建并启动；新启动器实际打开已有Rust服务。不是另一台全新机器安装声明。 |

## 仍需成人参与或外部条件的部分

1. 基本成人按键语音流程已获用户确认。仍需明确验证英语自然语音；在朗读时按停止/按住说话，检查回声，再切档案确认没有旧声音或文字。此次用户确认不覆盖这些子项。
2. 两人同框选择、交叉/遮挡、实际USB拔插与占用/拒绝权限。
3. 物理云台：现保留新FAIL/latch/journal，随后已只读确认Sleep，正在等待用户Wake反馈；不清latch、不自动重试或发新的非零命令。先恢复可核验的实时状态，再由独立明确流程处理方向/停止验收，不把抖动或Sleep当已证实根因。

本轮只读后续：20次OSC均为`[17,84]`，同窗原生12次均Pan17/Tilt−83；13:10:20握手确认唯一`Tiny 3`仍connected但running=false。根代理当时的应用状态观察为error、frame_age340525ms，旧图已失效。证据见`docs/OSC_READ_ONLY_DIAGNOSTIC_20261101.md`；这不能解释失败瞬间的确切pose变化。

当前release为PID87904／SHA `f493b96779e03f27ce0c468fd3670bc34620eaf3958d08582f5a786c1ea935fc`。根代理已部署并恢复成人档案、打开工作台；尊重Sleep状态，没有自动打开摄像头。GET video/frame实际为jpeg=null、tracks0，mode=off且gate=BLOCKED，旧latch完整保留，等待用户Wake反馈。C368／PID90324属于阶段历史。旧30分钟实测仅对应`469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c`，不能套用到新release，历史报告不改。

浏览器自动化因工具无法可靠确认当前Windows浏览器网址而停止；没有绕过该限制，也没有将WASM构建通过当成完整页面人工验收。
