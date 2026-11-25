# 实体摄像头跟随修复记录

最新使用状态（2026-11-02）：用户表示“现在不方便继续测试”，本轮现场测试已结束，云台模式off，不自动重新启用。新版实体水平跟随的人眼确认仍pending。后文FOLLOWING状态均为先前验收快照；最新控制记录见 `runtime/ptz-user-deferred-20261101T175907Z.json`。

更新：2026-11-02，Asia/Shanghai；各次实测时间以证据文件中的时间戳为准。全部通过 Rust CLI 和本地接口完成，没有使用电脑界面自动操作，没有发送摄像头画面或录音给模型。

**当前产品结论：控制器、到位读回、真实短失踪恢复与本地取消为 PASS_LIMITED；人的实体水平跟随确认、完整急停等仍待验收。** 最终版本已取得有限水平目标到位、PAUSED→FOLLOWING自动恢复和手动停止后的新鲜姿态保持证据；当前快照为external_yolo/FOLLOWING。约237秒的本版长窗口因真正超过600毫秒的失踪而清空选择并保护停止，未满五分钟；另有五分钟记录仅为PTZ关闭期间的PASS_CAPTURE_ONLY。历史FAIL和旧版“仍然没有转动”的用户反馈全部保留，新版尚未获得用户实体反馈，不写human physical PASS，也不称PTZ五分钟或完整故障停止通过。

## 已确认与修复

人脸框与物理云台是两条独立链。最初物理模式为off，缺少通过的方向标定，也未明确选择视觉目标；随后已分别进行了方向校准、明确选脸与多次有限跟随尝试。出现脸框本身仍不等于物理跟随已启用或可用。

1. 标定改为每轴使用固定区域的两张新鲜画面确认当前静止。返回整数读数不再被当成精确恢复旧像素位置；首始整数角度、同一采集周期、原质量与时效门禁均保留，先写测量日志再校验。
2. 原大区域候选不足时搜索较小区域；不降低500/1400像素、纹理、峰差、双区域非重叠与人脸排除门限。防止用平坦区域填充小纹理块凑足面积。
3. 方向校准在原平移判定不适用且各区域质量都通过时，可使用受限的单位尺度刚体判断。旋转不超过本次1°步长；不丢区域、不引入缩放或剪切，旋转补偿后的两两残差仍保持原0.75像素门限。静态及通用运动检测没有放宽。该模型只提供方向一致性证据，不能证明旧失败由画面旋转引起。
4. 修复跟随发送时序：设备核验后，必须再取同目标、同代次的新鲜新帧，确认原有限动作仍朝向目标。250毫秒新帧门限不变。居中或反向时零发送、重新规划；目标丢失、过期、换人或取消仍停止。限速以实际UDP接受时间为起点；此修复没有直接证明实体跟随通过。
5. 新增严格的CLI启用命令，明确绑定相机采集周期、目标及代次。安装脚本同时打包标定工具；已有通过报告无需重复标定。
6. 新增显式 `canonical_reader=capture_owner`：经本机认证的 `/api/camera/native-pose` 从当前视频采集拥有者取得请求进入后的新采样，不再为每次读取另开DShow设备。读取保持900毫秒总截止时间、设备与采集周期绑定、新序号及不超过250毫秒的新鲜度检查；不会在失败时静默回退到旧helper。真实只读验证如下，不把它作为运动验收。
7. 已将生产有限跟随与标定参数分离：生产每轴最多3°、速度4；标定仍每轴最多1°、速度1且保持精确到位要求。此前速度12版本的保护停止及到位超时均作为历史FAIL保留，不用新参数改写旧结果。
8. 已部署版本保留受门禁保护的500毫秒初始等待，到位过渡时命令轴目标端允许2个整数单位的带宽，最终arrival仍要求目标±1，并要求真实离开起点且两次不同新采样同pose后才继续。4秒期限在读回前后均检查，跨过期限的读回不能被接受为成功到位；单次读取与停止处理本身仍是有界异步操作，不称硬实时停止。最新水平目标也取得两次新采样同pose到位，结论仍限定为控制器与读回证据。
9. 排查停帧时发现pinned `nokhwa-bindings-windows 0.4.6` 的buffer Lock/Unlock路径缺口，已在工作区vendor补丁以RAII修复，全局依赖缓存未改。但该版本随后仍在约28秒停帧，诊断阶段为 `capture/decode_frame`，因此不能把buffer锁缺口单独认定为已证实根因或已完成修复。
10. MJPEG改用项目现有的Rust `image/zune-jpeg`解码，限制5MB输入、32MB解码分配并核对宽高，非MJPEG才保留旧fallback。该阶段完成视频-only五分钟验证；此前JPEG版跟随仍因单帧目标miss中断，相关失败保留，不能用视频通过代替跟随通过。
11. 已部署显式 `selection_generation` 与typed PAUSED：只允许同一epoch、同一key、同一selection_generation在600毫秒内恢复；旧future、session与command销毁后，先发送zero再重新核验。250毫秒发送帧龄门限不放宽。真实再启用记录验证了同selection_generation=6时target_generation从7变8、PAUSED→FOLLOWING恢复；超过600毫秒或选择变化则清除旧授权并退出，需显式重选与开启。界面在PAUSED时仍可停止，不把历史非零计数误当作新的未完成动作故障。

## 证据与边界

| 项目 | 结果 | 证据 |
|---|---|---|
| 最新软件回归 | PASS：242通过、0失败、1项付费探针忽略 | `runtime/ptz-pause-workspace-tests.log`；枚举243项、UI21项、doc-tests0；未重跑付费探针；原生与WASM构建均成功 |
| 跟随与发送保护 | PASS：36项离线测试 | ptz17项与ptz_follow19项，包含短暂停授权隔离及历史动作清理；不代替实体验收 |
| 方向校准逻辑 | PASS：19项离线测试 | 局部静止、严格原角度、旋转判断与错误拒绝；不替代设备验收 |
| 双轴真实方向校准 | PASS_DIRECTION_ONLY | `runtime/ptz-calibration-rust.json`，run `65b33c8c-e8c3-4cb2-bc47-8c279abf313b` |
| 实体校准动作 | PASS：4条有限非零目标 | pan正向及返回、tilt负向及返回；起始整数pose yaw18/pitch−38，pan_sign−1、tilt_sign1 |
| 首次旧控制器尝试跟随 | FAIL，非零发送0 | `runtime/ptz-follow-live-samples-01.json`；发送被保护拒绝，样本随后也出现目标丢失；旧帧时序缺陷独立修复，未断言单一根因 |
| 旧版有限跟随尝试 | FAIL，累计10条有限非零目标后停止 | `runtime/ptz-follow-live-20261101T143543Z.json`；最终off/BLOCKED，读回超出有限位移范围或未命令轴偏移门禁触发；不能仅靠计数宣布实体跟随成功 |
| 同采集owner真实原生姿态读取 | PASS_READ_ONLY：16/16 | `runtime/ptz-owner-hardware-fresh-readonly.json`；16个不同新序号均大于请求基准，原生Pan19/Tilt27与既有DShow读数一致，最大采样年龄123ms，额外设备打开0、Set调用0 |
| owner版有限跟随尝试 | FAIL／待修复，累计3条有限非零目标后停止 | `runtime/ptz-owner-follow-live-20261101T153200Z.json`；过渡读数触发 `reverse_or_more_than_one_unit_past_target`，最终off/BLOCKED；用户仍反馈“仍然没有转动” |
| 生产3°／速度12第一次跟随 | FAIL，累计3条有限非零目标后停止 | `runtime/ptz-production-follow-20261101T160513Z.json`；运动过程出现 `osc_motion_canonical_consistency_rejected`，最终off/BLOCKED |
| 加500ms初始等待版本跟随 | FAIL，累计1条有限非零目标后停止 | `runtime/ptz-settled-follow-20261101T161159Z.json`；start(22,−19)→target(22,−22)，等待后规范化native(22,−24)、raw OSC(22,−23)触发过渡外带保护；随后规范化native稳定在(22,−23) |
| 同窗背景只读观察 | 已观测视野位移，不是实体跟随PASS | `runtime/ptz-settled-background-20261101T161159Z.json`；两块独立背景区域位移一致，原图dy=18.009px、dx=−0.594px，各区域NCC≥0.989；该观察本身发送设备命令0 |
| 速度12最后一次跟随 | FAIL，1条有限非零目标后到位超时停止 | `runtime/ptz-final-follow-20261101T162522Z.json`；start(22,−23)→target(22,−21)，读数反复变化，未取得两次同pose到位 |
| 停用后的原生姿态只读观察 | 40个新样本稳定，非跟随PASS | `runtime/ptz-off-passive-20261101T162851Z.json`；约8.7秒内原生Pan22/Tilt19稳定；只反映off期间的读数 |
| 历史速度4短窗口有限跟随 | 历史PASS_LIMITED：约67秒内2次tilt到位 | `runtime/ptz-speed4-follow-20261101T163209Z.json`；140样本，首末67.05秒，含启动2项ARMING、8项SETTLING、130项FOLLOWING；启用后的138项均external_yolo，原生Tilt19→18→17、Pan22，两目标均有两次新鲜同pose确认；仅该窗口内stop_fault为false |
| 上述短窗后的继续跟随 | 中断，不能作为稳定跟随通过 | `runtime/ptz-speed4-continuation-20261101T163444Z.json`；随后真实停帧触发250ms保护停止，10秒watchdog重启相机；原短窗PASS_LIMITED不能推广 |
| buffer RAII修复版跟随 | FAIL／中断，约27.98秒 | `runtime/ptz-buffer-live-20261101T164653Z.json`；帧龄418ms触发停止，同阶段诊断定位 `capture/decode_frame`；修补buffer锁路径不能单独证明根因已排除 |
| Rust JPEG版跟随尝试 | 中断，约55.35秒、非零命令0 | `runtime/ptz-jpeg-live-20261101T165552Z.json`；单帧目标miss使跟随失效，随后即使脸重新可见也未恢复；不把无运动命令的运行时间算作运动验收 |
| Rust JPEG视频-only五分钟 | PASS_CAPTURE_ONLY | `runtime/camera-jpeg-soak-summary.json`与`runtime/camera-jpeg-soak-20261101T170124Z.json`；526样本、300.25秒、1个采集epoch、0诊断错误；帧龄median155ms、p95 196ms、max254ms，仅1个样本>250ms；窗口内PTZ off，不是PTZ五分钟 |
| PAUSED版长窗口及真实长失踪 | PASS_LIMITED：到位与超窗保护退出，非PTZ五分钟 | `runtime/ptz-pause-live-20261101T172155Z.json`；416样本、约237秒，1条非零目标到位，视频error0；最后失踪>600ms，选择清空、off/BLOCKED，pause1/resume0 |
| 显式本地停止及后续读回 | PASS_LIMITED：本地取消与姿态保持 | `runtime/ptz-final-stop-20261101T172708Z.json`；FOLLOWING→STOPPED/off，16个新pose中Pan22/Tilt16保持，四zero全部本地提交，非零增量0、stop_fault=false；不是设备急停验收 |
| 再次显式开启与真实短失踪恢复 | PASS_LIMITED：PAUSED→FOLLOWING | `runtime/ptz-final-rearm-20261101T172759Z.json`；2条实际非零目标，selection_generation=6保持，target_generation7→8，pause1/resume1，stop_fault=false |
| 最新水平控制目标与到位读回 | PASS_LIMITED：有限水平目标到位 | `runtime/ptz-final-operational-snapshot.json`；startPan24→targetPan26，规范化native(26,−16)、raw OSC(25,−15)，两新样本同pose确认；未命令的nativePitch−16保持，前次停止Pan22 |
| 最新运行状态与用户实机确认 | FOLLOWING；用户确认待回复 | 同上快照为external_yolo/FOLLOWING，原生Pan26/Tilt16，pause2/resume2、error0；用户最后实体反馈仍针对旧版本，新版未答复 |
| 完整实体停止、拔插与长期跟随 | NOT_RUN | 不把UDP零值提交或方向校准升级成实体急停验证 |

最初失败 `aa2650f8-ce36-4c28-8808-b98e8d93dece` 与随后失败 `6eb04e83-e60c-4273-ae7f-10530ece73f4` 的报告、日志和锁均在 `runtime/ptz-calibration-history/` 各自目录保留，附摘要和人工审查。没有把旧FAIL改成PASS。用户确认期间手动调整过相机或设置，因此24°到38°的变化不是未命令漂移的结论；旧姿态的单轴证据未用于替代新姿态双轴校准。

用户最新确认相机已固定且OBSBOT两个AI总开关均关闭。官方OSC目前只能读取目标锁定，不能据此宣称读取了两个总开关；新确认来源保存在 `runtime/ptz-osc-config.json`。

owner版最后一次保护读数中，起点为yaw21/pitch−26，目标为yaw22/pitch−25；原生读回yaw22/pitch−27，raw OSC为yaw21/pitch−26，因此该版拒绝继续。这里只记录当时的数值和停止原因，不断言用户看到的运动幅度、完整物理到位或停止已被验证，也不把新版本待验证的量化策略回写成旧尝试成功。

后续500毫秒初始等待版在规范化yaw/pitch起点(22,−19)发往(22,−22)，读回规范化native(22,−24)、raw OSC(22,−23)，因 `outside_start_to_target_integer_band` 停止；随后原生规范化读数稳定为(22,−23)。同期被动背景观察中，两块非重叠区域的匹配相似度分别约0.994和0.989，位移共识对应原图向下18.009像素、向左0.594像素。该证据支持“视野发生位移”，不单凭它判断完整运动归因、跟随是否满足使用者需要，或宣布实体停止通过。用户尚未确认转动或跟随可用，故该次跟随仍记FAIL。

随后速度12版本从规范化(22,−23)发往(22,−21)，原生Tilt读数在19、20、21、23等整数间反复，未形成两次同pose到位，在到位期限检查后停止。停用后的40个新样本中原生Pan22/Tilt19持续稳定，未据此重写该次超时为成功。速度4版本虽在约67秒窗口内取得两条有限tilt目标的新鲜稳定到位读回，后续却真实停帧并被保护停止，因此该改善只保留为历史短窗PASS_LIMITED，不能视作稳定跟随通过。

buffer RAII修复版未排除停帧；进一步诊断指向采集链的 `decode_frame` 阶段。更换为有界Rust JPEG解码后，视频-only记录的526个样本覆盖300.25秒，始终同一采集epoch、无诊断错误；其中一个254ms样本超过250ms，不能用于PTZ发送授权。该窗口的PTZ一直关闭，故结论严格限定为PASS_CAPTURE_ONLY。此前单帧miss使整个跟随失效的问题已由受限暂停与重验恢复方案在后续真实短失踪中验证，但约237秒记录最终遇到的长失踪仍正确退出，未构成PTZ五分钟。

最终版本先验证手动停止：16个请求进入后的新pose均保持原生Pan22/Tilt16、未再增加非零命令，停止回执记四项zero本地提交成功。此证据支持本地取消和读回保持，不验证断电、拔插、进程崩溃或设备物理急停。随后再次显式开启时观察到真实PAUSED→FOLLOWING恢复；更晚水平目标从Pan24发往26，原生规范化yaw26/pitch−16及raw OSC yaw25/pitch−15满足两新样本到位。它支持有限水平控制与读回，仍不能替代使用者目视确认实体是否按预期跟随。

## 当前交付

当前已部署PID115048，应用SHA-256 `2fbea30e1007f29b731f980eb54b4fe472afd0f4657cb141b0bbeff1425cf4c3`，UI WASM SHA-256 `57de5e720f78b9b5f8345b0b37a11f5af8efa03527900619156a8b4f7f718c55`；其余程序与发布清单由根任务更新manifest。软件回归为242通过、0失败、1项付费探针忽略，原生与WASM构建成功。最新运行快照为external_yolo/FOLLOWING、Pan26/Tilt16、pause2/resume2、error0。交付结论是控制器、有限目标到位、短失踪恢复和本地取消PASS_LIMITED；用户对新版的实体水平跟随确认、完整急停、拔插及长期运行仍待验收。摄像头画面留在本机，工作台由使用者操作。

当前部署的短失踪会暂停发动作并进入PAUSED；在同epoch、同key、同selection_generation且≤600ms时，可重新取得新授权后恢复，无需再次enable。长失踪、换目标、重新连接或跟踪关联不确定后仍退出并要求显式重选与开启，不会自动改追另一个人。PAUSED时mode仍为external_yolo、external_yolo_enabled为false不表示停止入口失效；界面停止与 `tutorctl ptz stop` 均继续可用。

命令与条件见 [CLI.md](CLI.md)。现有教学、语音和记忆保持原有接口；本轮没有新增远程模型调用或小车执行。
