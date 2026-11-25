# OBSBOT Tiny 3 物理云台调查

日期：2026-11-01；Windows 11 x64；实际 USB 视频设备名称 OBSBOT Tiny 3 StreamCamera，VID 0x3564 / PID 0xff02。

更新（2026-11-02）：最新Rust版本242项回归通过，已部署PID115048。实机有限目标到位、短暂失踪取消与恢复、本地停止和重新启用为PASS_LIMITED；水平读回Pan26、最后快照FOLLOWING。约237秒跟随窗口最终因超过600ms失踪停止，独立五分钟视频测试仅为PASS_CAPTURE_ONLY。新版实体水平跟随的用户确认、完整物理急停／拔插／长时运行仍待验收，旧失败和旧版用户反馈未改写。当前状态见 `PTZ_TRACKING_REPAIR.md` 与 `../runtime/ptz-follow-release-manifest.json`。下方“暂缓”“最新PID”等均为早期历史快照。

最新安排：用户已确认“已唤醒，继续测试”，随后明确要求先完善整个系统。本项现已暂缓，模式off，物理方向/跟随/停止仍未通过。原FAIL、journal、latch与归档审查记录保存在 `runtime/ptz-calibration-history/65e52d1e-e32b-47c1-aaca-772432f8e7ff/`，没有执行新的运动测试。最新应用PID81696，教学系统发布情况见 `PRODUCT_WORKFLOW_REPORT.md`。以下为此前设备阶段的历史记录，不代表当前部署、唤醒状态或活动锁文件位置。

**阶段结论：旧UVC单次1° PAN标定FAIL保留为历史；新官方OBSBOT Center OSC v2-final尝试为FAIL_NO_RETRY，在姿态变化门禁处被拒绝，非零发送0。随后确认设备Sleep及旧图失效；物理方向、跟随与停止未通过，保持off/BLOCKED。没有用数字裁切替代云台。**

## 本轮实际尝试与随后状态

`runtime/ptz-calibration-rust.json`与journal记录13:03:41的`FAIL_NO_RETRY`，错误`osc_motion_pose_changed_reverify`。冻结源代码中的此校验先于目标编码与UDP发送，因此本次非零Set为0；`outbound_possible_send`只是预先意图。同步四方向0提交4项、异步0提交4项均有本地成功回执，但`physical_stop_verified=false`。没有进入restore或tilt阶段，FAIL/journal/latch均保留，不自动重试。

13:08同窗只读采样中，OSC20次均为`[17,84]`、AI20次均0；原生12次均Pan17/Tilt−83。13:10:20的`runtime/osc-device-state-after-samples.json`握手随后确认唯一`Tiny 3`仍connected、selected0，但selected_device_running=false，按官方表0为Sleep。根代理同阶段已记录camera state=error、frame_age340525ms，旧图不能再当实时预览。此处没有为更新文档重新查询设备。

**随后确认Sleep不等于已证实Sleep在失败瞬间触发了门禁。** 失败当时的baseline/current数值未完整记录，不能事后填充；−24/−23边界抖动假说未被当前采样证实。用户Wake确认正在等待，任何新的非零动作均禁止，不清latch或重新校准。完整数字证据见`OSC_READ_ONLY_DIAGNOSTIC_20261101.md`。

最新workspace软件回归为140项PASS、0失败、1项付费Kimi探针忽略（本轮未重跑），见`runtime/rust-ptz-workspace-tests.json`。包含陈旧预览后端2项与Rust UI8项；这些软件/fixture结果不能把物理跟随或停止升级为PASS。

## 新官方OSC通路：稳定证据快照

以下记录截至2026-11-01的只读与自动测试阶段；后续低幅实体结果需独立追加，不能覆盖早期失败，也不能把代码/fixture通过写成物理PASS。

- 官方OBSBOT Center **2.0.15.186**已从签名安装包提取到`runtime/vendor/center/app/bin/OBSBOT_Main.exe`。未执行安装器、驱动或固件任务。`runtime/vendor/setup-evidence.json`记录签名`Valid`、发布者`REMO TECH Co., Ltd.`，安装包SHA-256为`fe0a97e9ebfc2d09dfc20e1f6a1cde99a46ad3c301da781c4df3f9d466f40617`。
- UDP接收端口**16284**来自实际Center界面与监听记录。Center的OS监听为`0.0.0.0:16284`；本应用仅向`127.0.0.1:16284`发送，并严格检查回复IP和来源端口。原生OSC客户端的本地回复端口显式配置，0表示系统分配临时端口，没有猜厂商默认端口。
- `runtime/osc-read-rust-handshake.json`与`runtime/osc-read-rust-profile.json`已实测官方`Connected(1)`软件注册，回复的唯一connected槽为0、精确名称**`Tiny 3`**、selected=0、running=true、类型码18。握手明确标为`DeviceInfoViaHandshake`，不冒充无回复的GetDeviceInfo成功；ConnectedResp单独诊断。
- 当前Center的云台读取实际为两个int。显式`Tiny3Center2015PanPitch`配置将`[17,-24]`解释为yaw17/pitch-24、unused roll=0；默认`DocumentedRollPitchYaw`仍只接受旧官方三int。同步原生Pan17/Tilt+24与OSC `[17,-24]`只读交叉核验见`runtime/osc-read-rust-correlate.json`、`runtime/ptz-read-rust-correlate.json`。这不是Set方向或物理停止证明。
- 用户已明确确认“两个总开关均已关闭”，`source_type=user_confirmation`，时间`2026-11-01T12:35:10.5567586+08:00`，保存在`runtime/ptz-osc-config.json`。OSC单int0只解释为unlocked，不能替代两个总开关的用户确认，也不代表永久关闭状态。

Rust门禁要求精确设备、唯一槽/selected0、两次unlocked及稳定pose；每次非零命令最多每轴1°、固定speed1、一次性短时权限，发送前重查来源和末端取消/帧时效guard。停止逐项保留四方向0的数值结果；不完整停止、超时或异常清理会持久锁定，不能通过重新启用或重启清除。actor绑定camera epoch、track key和target_generation，网络等待期间监测取消；服务关闭不可逆。上述为已实现、自动测试及只读审查事实，**不表示已经完成真实物理跟随**。细节见`OBSBOT_OSC_READ_REPORT.md`、`OBSBOT_MOTION_GUARD_REPORT.md`。

## 光学预检与部署版本

v1固定中心/边缘mask未选到足够独立背景信息；12:45:23读取时`runtime/ptz-visual-preflight.json`为`BLOCKED_NO_MOTION`，歧义峰不能用于方向，命令0、无latch。该路径按设计是可更新的最新无运动预检快照，后已被13:03:40的v2两帧PASS替换，不是v1原始存档；v1结果在此保留为历史观察。这不影响独立FAIL/journal/latch的保留，也不表示完成过新的实体方向测试。

动态方案改为两张不同帧、当前人脸框扩展并集排除、至少两块不重叠且有二维纹理的固定背景ROI，逐块检查NCC峰margin≥0.035并要求位移共识；人脸进入固定ROI、平坦/单边/周期背景或反向共识均拒绝，不为运动后的失败重新挑ROI。`runtime/ptz-dynamic-preflight.json`在`2026-11-01T12:57:54.400901400+08:00`记录frame3873→3881的`PASS_NO_MOTION_ONLY`，命令0、latch=false。这只是无运动图像质量通过，新的方向标定、跟随与停止仍为NOT_RUN/BLOCKED；标定发送前的有界视觉许可另由校准程序检查。

当前release为PID87904，`bin/ai-tutor.exe` SHA-256为`f493b96779e03f27ce0c468fd3670bc34620eaf3958d08582f5a786c1ea935fc`，已由根代理部署。为尊重Sleep状态，未自动打开相机；成人档案与工作台已恢复。根代理实测GET video/frame为jpeg=null、tracks0，modeoff/gateblocked，原FAIL/latch未清；用户Wake反馈仍待确认，不允许新的非零动作。

C368／PID90324是先前阶段部署，不能继续作为当前程序标识。既有30分钟实测只对应SHA-256 `469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c`，详见`RUST_STABILITY_REPORT.md`；该历史报告保持不变，新F493构建不继承其整机持续运行验收。

## 早期UVC调查与失败试验（历史保留）

下列安装发现、脚本与原生控制状态属于早期快照。当前厂商软件与官方OSC能力以上述新记录为准；早期失败不被新代码或只读结果改写。

| 层次 | 本次证据 |
|---|---|
| 文档声称支持 | [官方 SDK](https://www.obsbot.com/sdk) 明确列出 Tiny 3，列出 Gimbal Control、Tracking On/Off、PTZ Reset。|
| 可获取接口 | SDK 页面要求先登录，填写用途申请，SDK 发至账户邮箱；本次未代用户申请或提交个人信息。|
| 另一条官方接口 | [官方 OSC 说明](https://www.obsbot.com/explore/obsbot-center/osc) 明确支持 Tiny 3 Series，通过 OBSBOT Center；要求软件运行。|
| 厂商接口本机可调用 | 在 Program Files、Program Files (x86)、LocalAppData/Programs 目录未发现 OBSBOT/Remo 安装；进程列表未发现 OBSBOT，未发现已提供的 SDK。未扫描其他私人文件。|
| 原生接口本机可调用 | 已通过 Windows DirectShow `IAMCameraControl` 的 `GetRange` / `Get` 读取真实设备 Pan/Tilt；两项均 S_OK，结果如下。此原生路径不依赖厂商 SDK 或 OBSBOT Center。|
| 实测边界 | 视频采集、原生范围读取通过；执行一次 PAN 7→8→7 度的低幅测试，返回值和场景测量如下。固件版本、厂商 AI 跟踪状态与可靠停止行为未知；自动跟随、目标丢失停止和 USB 断线停止未验收。|

## Windows 原生只读探测结果

`python scripts/probe_uvc.py`：PASS_READ_ONLY_QUERY。直接使用 Windows COM（ctypes，无额外安装），枚举 SystemDeviceEnum 视频 moniker，通过 `IPropertyBag.DevicePath` 与应用保存的稳定 USB 设备路径精确匹配，然后仅将 moniker 绑定为 IBaseFilter 并查询 IAMCameraControl。未创建 FilterGraph、未启动采集流、未调用 `Set`、未操作任何驱动/全局配置；子进程20秒时限。证据 `runtime/uvc_probe.json`。

| 属性 | GetRange | 最小值 | 最大值 | 步长 | 默认值 | 当前值 | 支持/当前flags |
|---|---|---:|---:|---:|---:|---:|---|
| Pan (属性0) | S_OK | -130 | 130 | 1 | 0 | 7 | 2 / 2：Manual，非 Relative |
| Tilt (属性1) | S_OK | -90 | 90 | 1 | 0 | 10 | 2 / 2：Manual，非 Relative |

这表明当前驱动有可读取的标准绝对角度控制接口；不是 physical-follow PASS。Windows 文档规定绝对 Pan/Tilt 以度为单位，但设备的实际方向、准确角度、可用运动行为尚未标定。Manual flag 是该属性的控制模式，**不证明 OBSBOT 自带 AI 跟踪已关闭**。

## 单次1度 PAN 标定：FAIL

通过 `scripts/probe_ptz_motion.py` 进行一次低幅测试：只发送 PAN 绝对目标8度，再在 finally 中发送 PAN 绝对目标7度恢复；共两个有限位置命令，没有连续速度、没有 TILT Set、没有厂商跟踪开关命令，没有重试运动。

| 时刻 | PAN 读数 | TILT 读数 | 应用动作 |
|---|---:|---:|---|
| 起始 | 7 | 10 | 只读 |
| 设 PAN=8 后约150ms | 7 | 9 | PAN Set 返回 S_OK；即时读数尚未到目标 |
| 恢复前 | 8 | 3 | PAN 已到8；TILT 继续变化，应用从未设置它 |
| 恢复 PAN=7 后最终读取 | 7 | 1 | PAN 恢复原值；TILT 没有保持原值 |

应用持有的实时画面也出现位移（仅在内存中计算，不保存 JPEG）：PAN 测试前后640×360图的相位相关位移 dx=9.498px、dy=-44.853px、相关响应0.8308；恢复后的相对位移 dx=6.956px、dy=-73.237px、响应0.7539。画面来自同一 camera_session_id，frame_id 从19556推进至19628，未打开第二路采集。此测量支持“场景发生变化”，不能作为物理方向标定或自动跟随成功的证据。

完整证据：`runtime/ptz_motion_probe.json`，结果 `FAIL`。PAN 往返的延迟读数与目标一致，但 TILT 在没有 TILT 命令的情况下10→1，属于意外/不受控运动。可能存在厂商跟踪竞争或其他尚未核验的设备行为，不能仅凭此确认根因。

测试后立即停止应用控制并保持 off；没有尝试用其他命令修复 TILT，没有再次运动测试。所有物理调用均以失败证据 fail closed：`enable_manual`、`manual_move` 和原生 worker 的 move 分支都拒绝执行；只读 probe 仍可用。`stop`/`close` 只取消应用事务，不发送角度，不声称已关闭厂商 AI 跟踪或具备可靠的硬件急停。一次已被设备接受的有限位置目标可能自行完成。

最小后续步骤：先在 OBSBOT 官方控制界面中关闭厂商 AI 跟踪并取得可核验状态，再重新安排方向与停止测试。可以从 [官方 SDK 入口](https://www.obsbot.com/sdk) 申请 Windows SDK，或安装官方 OBSBOT Center 并开启 OSC；原生接口本身已可调用，当前阻塞是控制互斥与意外运动，不能再描述为“没有控制接口”。只有后续明确完成这些验证，才能解除物理命令冻结；不能自动重跑标定脚本。

应用能力应显示 `hardware_read_capable=true`、`low_amplitude_calibration=FAIL`、`external_follow=BLOCKED`、`vendor_auto_state=unknown`、`mode=off`。应用没有发出持续速度，应用停止不等于厂商自身自动跟踪停止。

验收 C01 原生接口、单位文档、范围和读能力部分 PASS；C02 低幅标定 FAIL；C03–C07 条件受阻/NOT_RUN。未进行真实外部跟随，不能交付 PTZ_VERIFIED。12个 PTZ 自动测试通过，覆盖只读构造、角度/范围/步长限制、自动模式拒绝、停止代次隔离，以及失败标定下所有物理入口拒绝；这些不替代真实硬件验收。

## Microsoft 官方依据

- [Selecting a Capture Device](https://learn.microsoft.com/en-us/windows/win32/directshow/selecting-a-capture-device)：SystemDeviceEnum、moniker、DevicePath 唯一标识、BindToStorage 与 BindToObject。
- [IAMCameraControl](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nn-strmif-iamcameracontrol)：从摄像头 filter 查询该接口，读取相机属性。
- [GetRange](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-iamcameracontrol-getrange) 与 [Get](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-iamcameracontrol-get)：读取范围、步长、默认值、能力flags及当前值；成功返回 S_OK。
- [Set](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-iamcameracontrol-set)：以指定 CameraControlProperty、值和 Manual flag 写入有限绝对位置；本次只有两次 PAN 写入，未调用速度接口。
- [CameraControlProperty](https://learn.microsoft.com/en-us/windows/win32/api/strmif/ne-strmif-cameracontrolproperty)：Pan=0、Tilt=1，绝对角度单位为度；设备可实现文档范围的子集。
- [CameraControlFlags](https://learn.microsoft.com/en-us/windows/win32/api/strmif/ne-strmif-cameracontrolflags)：Auto=1、Manual=2、Relative=0x10；不将它解释成厂商目标跟踪状态。
