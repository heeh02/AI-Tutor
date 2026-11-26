# Rust 命令行工作台

界面交给使用者操作；助手通过 `bin/tutorctl.exe` 读取状态、提交报告和控制已有服务。CLI 不打开浏览器、不点击界面、不调用 shell，也不直接读取数据库或调用模型。所有业务操作都经过 Rust 服务现有的认证 API。

## 启动与状态

在项目根目录运行。服务启动与停止沿用 `ai-tutor.exe`：

```powershell
.\bin\ai-tutor.exe --root . --no-browser
.\bin\tutorctl.exe --root . status
.\bin\ai-tutor.exe --stop --root .
```

服务须已运行。CLI 从 `--root` 下的 `runtime/server.json` 读取端口与凭证，不输出凭证。未指定 `--root` 时，从当前目录、可执行文件所在目录及其祖先寻找该描述文件。

`status` 默认仅返回运行状态、当前对象编号、请求指标和能力状态，排除档案名称、对话、活动内容及记忆。需要明确检查完整上下文时才运行：

```powershell
.\bin\tutorctl.exe --root . status --full
.\bin\tutorctl.exe help
```

除帮助外，成功结果是标准输出中的单行 JSON；失败以标准错误中的 JSON 返回。退出码 `0` 表示命令成功，`1` 表示参数、连接或服务错误，`2` 表示等待结果已超时、被替代或活动失败。超时不会自动重新提交或取消已发出的模型请求。

普通请求可附加 `--timeout-seconds 1..30`（默认 10 秒）。`audio record stop` 的本地转写请求单独使用 90 秒有界超时，不受普通请求设置缩短；其前置状态校验仍使用普通超时。连接超时 2 秒；响应最多 4 MiB。CLI 固定使用 `127.0.0.1`，禁用代理和重定向，不接受外部主机地址。错误、嵌套响应及文字中出现的本地凭证都会脱敏。

## 档案与活动

```powershell
.\bin\tutorctl.exe --root . learners list
.\bin\tutorctl.exe --root . learners create --name '成人试用' --namespace real
.\bin\tutorctl.exe --root . learners create --name '模拟小车' --namespace simulator
.\bin\tutorctl.exe --root . session select --learner learner_ID
.\bin\tutorctl.exe --root . session context
.\bin\tutorctl.exe --root . session finish
.\bin\tutorctl.exe --root . session new --learner learner_ID
```

`learners create` 只创建档案，不自动切换。`session select` 恢复该档案的未结束活动，或创建新活动；`session new` 明确开始新活动。它们会调用服务已有的状态隔离与活动收尾逻辑。恢复之后仍需重新报告当前模块，历史搭建不作为当前硬件事实。

`context`、`finish`、`cancel`、活动报告与文字输入，未指定对象时先读取当前会话。可用 `--session session_ID`（或 `--session active`）明确绑定会话，也可用 `--learner learner_ID` 校验当前档案；两者不能同时指定。指定对象与当前活动不一致时，CLI 拒绝操作，不自动切换档案。只有 `session select/new` 会改变当前对象。

```powershell
.\bin\tutorctl.exe --root . session context --session session_ID
.\bin\tutorctl.exe --root . session cancel --session session_ID
```

`session cancel` 停止当前生成与旧播报；结束活动须使用 `finish`。

## 模块、观察与教学文字

```powershell
.\bin\tutorctl.exe --root . activity report --source operator_confirmed --modules control,motor,light
.\bin\tutorctl.exe --root . activity report --source learner_report --report '孩子报告：测距模块已接上，尚未硬件验证'
.\bin\tutorctl.exe --root . activity report --source operator_confirmed --modules control,motor,distance,light
.\bin\tutorctl.exe --root . activity report --source learner_report --report '停车和亮绿灯成功，但离墙太近'
.\bin\tutorctl.exe --root . activity report --source operator_confirmed --modules none
```

只有显式传入 `--modules` 才更新模块。仅传 `--report` 会保留现有模块；`--modules none` 明确记录空模块列表。模块只允许 `control,motor,distance,light` 的无重复逗号列表。来源只允许 `learner_report`、`operator_confirmed`、`simulated`；模拟来源只能写入独立模拟档案。报告不会被改写为传感器验证，也不会执行小车动作。

```powershell
.\bin\tutorctl.exe --root . turn send --text '我想让小车在墙前停车，然后亮绿灯'
.\bin\tutorctl.exe --root . turn send --session session_ID --text '它已经能停车了，我想让间隙更大' --wait --wait-seconds 120
```

`turn send` 通过现有服务调用真实 Kimi，可能产生远程用量；没有自动重试。`--wait` 在 1–180 秒内（默认 120 秒）轮询当前请求，仅返回本轮请求编号、回复和状态，不输出其他轮次。新代次或档案切换会报告 `superseded`。分句模式同时检查 `tutor_stream.phase`，离开 `generating` 后从只读 stream 接口拼接已校验正文；`final_pending` 仍待真实显示，`completed` 才表示显示流程完成。失败、取消或中断不会包装成成功。

CLI **不调用 `delivery/shown`、不自动朗读**。输出中看到 `generated` 只表示模型已生成；真正显示后的投递确认由用户工作台完成，之后才应用相关教学与记忆候选。已有 UI 确认时，CLI 如实报告该状态。请通过用户界面完成成人教学交互验收。

## 查看、更正与删除记忆

```powershell
.\bin\tutorctl.exe --root . memory list
.\bin\tutorctl.exe --root . memory list --learner learner_ID
.\bin\tutorctl.exe --root . memory correct --learner learner_ID --memory memory_ID --content '第二次试跑已成功，停车间隙更大；信息来自学习者报告'
.\bin\tutorctl.exe --root . memory delete --learner learner_ID --memory memory_ID --confirm
```

更正和删除要求明确的档案、记忆编号；删除还要求 `--confirm`。CLI 不提供隐式清空所有档案的命令。来源检查、关联记录失效及删除逻辑继续由服务负责。

## 摄像头与本地音频

以下命令会在显式请求时枚举或控制真实设备：

```powershell
.\bin\tutorctl.exe --root . camera list
.\bin\tutorctl.exe --root . camera open
.\bin\tutorctl.exe --root . camera open --device '真实枚举返回的设备标识'
.\bin\tutorctl.exe --root . camera pose
.\bin\tutorctl.exe --root . camera track list
.\bin\tutorctl.exe --root . camera track select --track '完整临时track_key'
.\bin\tutorctl.exe --root . camera track clear
.\bin\tutorctl.exe --root . camera close
.\bin\tutorctl.exe --root . audio devices
.\bin\tutorctl.exe --root . audio record start --session session_ID --device 0
.\bin\tutorctl.exe --root . audio record stop --session session_ID
.\bin\tutorctl.exe --root . audio play --session session_ID --turn turn_ID
.\bin\tutorctl.exe --root . audio stop
```

`camera track list` 返回当前帧元信息与目标列表，去掉原始 JPEG。选脸只切换临时视觉目标，不切换学习者档案，不开启物理云台。

`camera pose` 读取受本机认证保护的 `/api/camera/native-pose`，使用当前视频采集拥有者已有的设备连接，只返回原生 Pan/Tilt、采样序号、采集周期及相关数值状态；不返回 JPEG，不另开摄像头，不调用设备 `Set`。接口在请求进入后等待最多 500 毫秒，要求得到比 `after_sample_seq` 更新的 `sample_seq`；旧缓存不能被重复当作两次稳定读数。采集周期变化、设备不可用或无法取得新鲜采样时应拒绝读取，而不是复用旧姿态。

供跟随使用的 `canonical_reader=capture_owner` 是 `runtime/ptz-osc-config.json` 的显式配置，CLI 不自动修改它。该读取路径保留单个读取槽与 900 毫秒总截止时间，核对保存的稳定设备标识、同一采集周期、新采样序号、采样帧龄不超过 250 毫秒，以及原生 manual 模式和整数步进 1；请求前后重新核对设备绑定，失败不静默回退到旧 helper。未选择该配置时保留原 helper 兼容路径。

录音、结束转写和播报要求显式指定当前会话编号。`record start` 实际启动麦克风；`record stop` 结束录音并返回本地转写，不自动发给 Kimi。转写请求允许等待最多 90 秒，无需另加超时参数；请求超时后不要盲目重复录音。`audio play` 仅尝试播放该会话的指定回复，服务仍会检查真实投递状态和代次。`audio stop` 只停止扬声器播报；结束录音使用 `audio record stop`。

2026-11-03 起，CLI 会把读取到的录音批次编号与会话编号一起提交给服务；操作期间若已换成另一段录音，旧请求会被拒绝。直接使用 API 时，`/api/audio/record/stop` 和 `/api/audio/record/discard` 均须提供 `session_id` 与 `recording_generation`，后者来自开始录音的响应。`discard` 只丢弃指定录音，不转写、不取消新的教学轮次。升级时应同步替换服务、CLI 与界面。

## 云台边界

```powershell
.\bin\tutorctl.exe --root . ptz status
.\bin\tutorctl.exe --root . ptz stop
.\bin\tutorctl.exe --root . camera track list
.\bin\tutorctl.exe --root . camera track select --track '完整临时track_key'
.\bin\tutorctl.exe --root . status
.\bin\tutorctl.exe --root . ptz enable --camera-session camera_ID --track '完整临时track_key' --target-generation 7
```

`ptz enable` 只请求既有服务启用 `external_yolo`，必须明确提供最新 `camera_session_id`、所选 `track_key` 与 `target_generation`。这三个值可在选定目标后的 `status` 中读取，示例编号须换成实际返回值。CLI 先读取当前状态，要求服务报告 `ready_for_follow=true`、相机运行、所选脸可见且关联明确、三个值完全匹配，才调用已有启用接口；服务仍会再次核验目标、标定、厂商模式和停止保护。目标代次、相机连接或选择变化之后，旧请求参数会被拒绝；短暂PAUSED恢复由控制器重新取得新授权，不复用这些旧参数。

标定与生产跟随使用不同的有限动作参数：标定每轴最多 1°、速度 1；最终生产跟随每轴最多 3°、速度 4，仍发送有限绝对目标，不使用持续速度命令。发送前仍要求同目标、同代次且不超过 250 毫秒的新帧，初次必须明确启用。每次有限目标的到位期限在读取前后均检查，不能把跨过4秒期限的读回认作成功到位；单次读取与停止处理仍是有界异步操作，不承诺硬实时停止。

短暂失踪时控制器暂停发动作并进入 `PAUSED`：`mode`仍可为`external_yolo`，`external_yolo_enabled=false`表示当前暂停发送，不代表停止入口失效。UI停止按钮和 `tutorctl ptz stop` 仍可用。只允许同一相机epoch、同一track key、同一`selection_generation`在600毫秒内恢复；旧future、session和command先失效，zero提交后重新核验设备、姿态及新帧，再恢复FOLLOWING，无需再执行enable。超过600毫秒、换目标、重连或关联不明确仍退出，需明确选脸并重新启用，不会自动改追另一个人。250毫秒发送门限保持不变。

最终版本的控制器、有限目标到位、真实短失踪恢复与本地取消已取得 `PASS_LIMITED` 证据：再启用记录包含PAUSED→FOLLOWING、同selection_generation下target_generation更新；最新水平目标Pan24→26也取得两新样本到位。此前历史FAIL全部保留。约237秒、416样本的长窗口最终因失踪超过600毫秒退出，不是PTZ五分钟通过；五分钟证据仅为PTZ关闭期间的 `PASS_CAPTURE_ONLY`。用户最后“不转”的实体反馈针对旧版本，新版尚未答复；人的实体水平跟随确认、完整急停、拔插和长期跟随仍待验收，不写human physical PASS。具体记录见 [实体摄像头跟随修复记录](PTZ_TRACKING_REPAIR.md)。

CLI 不修改标定文件，不提供 `--force`、解除停止故障或绕过门禁的命令。未标定或 `BLOCKED` 时保持关闭，不影响独立摄像头与教学功能。检测到脸框并不等于已选目标，更不等于已启用物理跟随。用户界面的启用入口位于“云台高级控制与标定”折叠区，仅在服务就绪并选中可见目标后可用。

### 受限设备标定工具

安装脚本会同时构建并分阶段复制 `bin/ptz-calibrate-probe.exe`。它是成人工程测试工具，会在满足设备与视觉前置检查后发送有限角度的真实云台命令；不是儿童交互接口，也不是日常开启跟随的步骤。已有适用于当前设备和采集设置的有效 `PASS_DIRECTION_ONLY` 记录时，直接选择视觉目标并使用上述 `tutorctl ptz enable`，无需重跑标定。

只有明确需要一次受限设备标定测试时，才使用其独立的 **四个位置参数**，而非 `tutorctl` 的选项语法：

```powershell
.\bin\ptz-calibrate-probe.exe . 16284 0 'Tiny 3'
```

顺序为项目根目录、OBSBOT Center OSC 接收端口、设备索引、枚举得到的精确设备名。本项目已确认的配置为端口 `16284`、索引 `0`、名称 `Tiny 3`；根目录下 `runtime/ptz-osc-config.json` 还必须与稳定 USB 设备标识、`Tiny3Center2015PanPitch` 配置和已有的用户确认一致，用户已确认关闭厂商“AI 跟踪”与“AI 模式”两个总开关。运行前应核对这些条件仍然成立，工具不会替使用者切换厂商模式。

该工具先检查原始 1280×720 视频和背景纹理，再以每轴最多 1°、速度 1 进行单次有限方向校验；不使用生产跟随的 3°／速度 4 参数，不放宽标定的精确到位要求，也不会自动启用生产跟随。`runtime/ptz-calibration-rust.json`、对应 `.journal.jsonl` 和 `ptz-calibration-rust.latch.json` 保存标定与失败证据。不得为了重试而随意删除、清空或覆盖已有 latch、`FAIL_NO_RETRY`、`CANCELLED_NO_RETRY` 或其他失败记录；同样应保留已通过的记录。遇到锁定或失败应先检查已有报告并处理真实原因，不能把安装、重新构建或重启当作解除限制的方法。方向通过仍不代表物理急停已独立验收。

## 构建与测试

```powershell
cargo test --locked -p ai-tutor --bin tutorctl
cargo test --locked -p ai-tutor --test cli_contract
cargo build --locked --release -p ai-tutor --bin tutorctl
```

构建输出位于 `target/release/tutorctl.exe`，正式安装脚本将它放入 `bin`。单元测试覆盖严格参数、报告不改模块、文字只作为 JSON、默认状态隐私、投递边界、对象匹配及凭证脱敏。契约测试使用临时描述文件和本地假 HTTP 服务验证真实 CLI 进程，不连接真实设备或模型。

`scripts/install-rust.ps1` 的程序交付清单包含 `ai-tutor.exe`、`ptz-read-probe.exe`、`tutorctl.exe` 和 `ptz-calibrate-probe.exe`；全部先复制并校验后才进入正常停止、替换与回滚流程。标定记录、失败锁定和学习数据不属于程序替换清单。
