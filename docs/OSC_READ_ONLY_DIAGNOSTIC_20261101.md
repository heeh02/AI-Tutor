# 标定失败后的只读数值对照

日期：2026-11-01，Asia/Shanghai。保留实际`FAIL_NO_RETRY`与标定latch，不放宽门禁、不清记录、不重试运动。本报告不是物理方向或停止验收通过。

## 13:03标定记录核对

`runtime/ptz-calibration-rust.json`结果为`FAIL_NO_RETRY`，错误`osc_motion_pose_changed_reverify`。journal记录了预先的`finite_send_intent/outbound_possible_send`，随后为停止结果；没有非零步长回执、restore或tilt阶段。

对照该失败分支的冻结源代码，pose与baseline比较位于目标编码及UDP发送之前。因此本次**非零Set为0**，这是记录与控制流共同支持的结论，不把“没有收到回执”单独当作没有发送的证明。原报告没有保存失败当时的确切baseline/current数值，不能事后猜填。

停止记录为同步四方向0提交4项、随后异步四方向0提交4项；两组均返回完整本地UDP提交，长度依次48/52/52/52字节。**共8项零值提交不证明实体已经停止**，原`physical_stop_verified=false`保持不变。后续本报告的采样没有再发送zero或其他写命令。

## 同步只读采样

共同计划开始时间为Unix毫秒`1793509723164`。新Rust辅助程序`osc-read-sample`只发送GetGimbalPosInfo和GetAiTrackingInfo，不注册客户端、不调用任何Set/stop，也不操作latch。20轮按250ms节拍进行，40次查询均通过精确来源`127.0.0.1:16284`校验。

| 证据 | 实际数值 |
|---|---|
| `runtime/osc-read-only-samples-20261101-01.json` | 20/20 pose原始参数均为`[17,84]`；显式Tiny3Center2015PanPitch解释为yaw17/pitch84；相邻变化0次 |
| 同一OSC样本的AI查询 | 20/20均为单int0/unlocked；不能单凭此证明所有厂商AI模式已关闭 |
| `runtime/ptz-read-after-predispatch-fail.json` | 既有严格只读原生探针12次、间隔250ms，Pan均17、Tilt均−83；Set调用0，不创建采集流 |

OSC实际pose接收窗口为13:08:43.2504123—13:08:47.9865596（+08），原生探针按共同启动时间运行，覆盖其中约前三秒。采用已验证原生探针的12样本上限，没有把它描述成最初期望的30样本。

两种接口当前都稳定，但与先前原生Pan17/Tilt+24、OSC`[17,-24]`的只读记录明显不同。当前原生Tilt−83与OSC pitch84也有1°差异。**这些数据不支持把本次问题直接归因为−24/−23边界截断**；不从整数读数推断未提供的底层浮点值、固件取整规则或变化原因。

vision代理同窗的被动背景采样在首帧过期（超过250ms）时拒绝，未形成新的有效场景位移证据；不能宣称画面稳定或相机已/未发生实体移动。没有图像落盘或远程图像调用。

## 随后独立确认Sleep

`runtime/osc-device-state-after-samples.json`在13:10:20（+08）通过显式官方握手读到：唯一槽0仍connected、名称`Tiny 3`、selected0，但selected_device_running=false。官方表的运行状态0为Sleep、1为Run；同期pose仍为`[17,84]`。这是随后状态证据，**不能断言Sleep在13:03失败瞬间已经存在或导致了本次失败**。

根代理同阶段的既有应用状态观察为camera state=error、frame_age340525ms。旧预览已失效，不可继续画框/选目标或据它派发；本文档更新未重新调用设备或服务。用户Wake反馈尚待确认，保持全部新非零动作禁止，原FAIL、journal与latch不清除，不自动恢复或重跑。

## 保持的边界与诊断改进

当前OSC pitch84已超出有限绝对指令允许的±59°范围。保持off/BLOCKED和原latch；不能通过放宽稳定度容差、取反原生Tilt或重发目标来补偿。先独立核对当前实体姿态、设备/视频占用及厂商控制状态，原因未确认前不增加运动。

新增`PoseChangedBeforeDispatch`结构化错误及`motion_error_details(&anyhow::Error)`，保留稳定错误代码，同时为未来拒绝保存baseline_pose、current_pose、两次baseline原始OSC回复、当前原始回复与来源、profile和nonzero_commands_sent=0。合成localhost fixture验证这些数值完整、没有motor包，1项测试通过。**此改进不回写旧失败报告，也没有为获取诊断再调用实机finite_step。**

本轮读取结果是数字诊断证据；原校准仍FAIL_NO_RETRY，实体跟随、方向和停止均未据此升级为PASS。

最新软件workspace回归140项PASS、0失败、1项付费Kimi探针忽略，来源`runtime/rust-ptz-workspace-tests.json`；本轮没有重复付费模型调用。它验证软件与合成fixture契约，不证明物理跟随通过。

当前release SHA为`f493b96779e03f27ce0c468fd3670bc34620eaf3958d08582f5a786c1ea935fc`、PID87904，根代理已部署并恢复成人工作台。Sleep状态下没有auto-open相机；根代理GET video/frame验证jpeg=null、tracks0、modeoff/gateblocked，保留旧latch，等待用户Wake。C368/PID90324改记为阶段历史。旧30分钟测试SHA `469960ec7c441cb69fc254215e126457b9d814547a7008fe95f9e434f80b990c`保持原记录，不用于新release验收。
