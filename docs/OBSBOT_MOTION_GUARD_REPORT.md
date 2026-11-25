# OBSBOT有限校准门槛

日期：2026-11-01，Asia/Shanghai。`rust/ai-tutor/src/obsbot_motion.rs`已编译，11项合成本机UDP fixture测试通过。本代理本轮只做fixture验证，不执行实机运动或停止；实机校准与控制集成由根代理及vision代理单独维护。没有修改其`ptz.rs`、`server.rs`或校准辅助程序，也不增加默认启用入口。

依据为[OBSBOT官方OSC命令表](https://www.obsbot.com/explore/obsbot-center/osc)2026/05/08版本第20—26行。只允许有限绝对角度命令和四方向的零值停止请求；没有连续速度、AI开关、预设、重置或任意发送API。

## 临时权限

调用`OscMotionSession::verify_for_calibration(CalibrationConfig).await`时只做官方软件注册与读取。配置显式提供server_port、reply_port、command_bind_port、device_index、expected_name、query_timeout和gimbal_profile；当前校准构造严格要求expected_name为`Tiny 3`、device_index为0。`query_timeout`必须大于0且不超过1秒。

取得权限需满足：只有一个connected槽，且槽号、请求index与selected_index一致；名称精确相符且running；AI锁定状态连续两次为unlocked；两次云台pitch/yaw相同，采样间隔100ms；首尾身份检查均通过。身份查询明确调用handshake_device_info，proof中保留真实DeviceInfoViaHandshake query及来源。总读取阶段有8秒截止时间。成功后才创建独立的本机运动发送socket。

权限固定10秒，一次验证最多一步。`finite_step(delta_pan, delta_tilt)`要求每轴变化绝对值不超过1°、数值有限且不是双轴全零。调用前重新查询身份、unlocked和当前位置；当前位置必须仍与稳定验证点一致，任何失败都会消耗本次权限。绝对目标必须在官方pan ±129°、tilt ±59°内；实际起点tilt47°被允许，能形成48°的有限目标，不沿用不符设备的较窄假定范围。

唯一非零运动数据包为`SetGimMotorDegreeEx`，参数严格为设备index（int32）、固定speed1.0（float32）、绝对pan与tilt（float32）。发送成功只记录操作系统接纳UDP数据，`physical_motion_verified=false`，不宣称到位。

`finite_step_guarded(delta_pan, delta_tilt, dispatch_guard)`在全部异步只读检查及编码之后、紧贴UDP发送之前，调用一次调用方提供的非阻塞原子guard。返回false会拒绝发送并消耗权限；guard返回后再次检查TTL。它用于调用方最后核对target_generation、camera epoch/track key、帧时效及取消代次；不代替调用方的并发派发协调。`finite_step`委托永真guard。只读`read_pose()`与`verified_pose()`分别返回新读取及稳定基线，不更新移动权限。

`stop()`撤销移动权限，只顺序发送SetGimbalUp/Down/Left/Right四项值0，且保持显式设备index。它可在权限已消耗或到期后发出零值请求，不续期任何移动权限。每项最多50ms，共用250ms总截止；前项失败或超时后，在总截止剩余时间内继续尝试其余项。总截止已到的项也明确保留为未尝试，不因早期错误丢失部分结果。

签名保持`Result<StopReceipt>`，但发送错误返回`Ok(receipt)`以保留四项完整结果；调用者必须检查`all_zero_commands_sent`，不能把`Ok`解释为四项都成功。`zero_commands_sent`仅计完整提交数，`commands_attempted`记录实际尝试数；`command_results`逐项含direction_index、address、attempted、result_code、bytes_sent、expected_bytes、os_error和elapsed_ms，另有total_elapsed_ms。

| result_code | 含义 |
|---|---|
| 0 | 完整UDP提交 |
| 1 | I/O错误，原生数值码在os_error |
| 2 | 单项超时 |
| 3 | 总截止已到，未尝试 |
| 4 | 短提交，保留实际字节数但不计成功 |
| 5 | 编码拒绝 |

`physical_stop_verified`始终为false；无硬件确认，也没有独立急停或进程崩溃看门狗声明。身份可能已变化的限制保留在返回记录中。模块没有隐式Drop运动命令。

## 真实协议与明确边界

设备信息通过显式官方Connected(1)握手已证实可读，名称为`Tiny 3`；默认GetDeviceInfo仍未证实成功。校准入口明确使用握手，不伪装成Get成功；proof中的read_receipts保留实际query种类及来源。

默认`DocumentedRollPitchYaw`保留官方三整数格式，拒绝两整数。显式`Tiny3Center2015PanPitch`才接受本机同步只读核验的两整数 `[yaw,pitch]`，roll设0；原生Pan17/Tilt+24与OSC `[17,-24]`证据存于correlate文件，原生Tilt符号与OSC不同。`runtime/osc-read-rust-profile.json`中显式profile的三项读取通过。该profile不能证明Set命令方向、物理到位或停止；本轮没有在实机调用校准session、finite_step或stop。

AI的unlocked仅代表官方锁定状态，不等于所有厂商AI/声音模式总开关已关闭。UDP没有认证或请求nonce，本门槛不能替代外部安全边界和实际方向、失控停止验收。

## 已执行fixture验证

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test --locked -p ai-tutor --lib obsbot_motion::tests -- --nocapture
```

profile阶段联合运行：13项OSC和8项motion共21项通过、0失败。停止故障路径修复后，11项motion测试全部通过、0失败，测试主体0.64秒。motion的11项覆盖：

1. 验证阶段零写入；多设备、错名称、错selected或错槽位拒绝。
2. 任一AI采样为锁定或两次pose变化时拒绝。
3. tilt47°到48°的正确低速有限命令编码；权限一次性。
4. 官方轴范围、单步长、NaN和零步长拒绝。
5. 移动前重新检查身份、AI及pose；失败消耗权限，不发送命令。
6. 合成过期权限拒绝移动；仅允许四个零值停止请求，不声明物理停止。
7. 读取超时、无期望名称及错误超时配置无法取得权限。
8. 最后dispatch guard仅调用一次；false时没有motor数据包且权限被消耗。
9. 受限私有transport注入首项I/O错误码10061和第三项挂起超时，第二、四项仍在真正localhost UDP fixture收到；结果保留`[1,0,2,0]`和成功2项。
10. 注入短提交，保留实际字节数且不计完整成功；后三项仍实际收包。
11. 合成已过总截止，四项均记录未尝试，实际发送次数为0。

故障由仅供私有零值发送路径的测试trait注入；没有声称真实Center发生过I/O错误、短提交或停止失败，也没有放宽或公开任意发送接口。

测试只向随机绑定的localhost合成UDP fixture发包，不能记为真实摄像头运动、停止或AI关闭PASS。
