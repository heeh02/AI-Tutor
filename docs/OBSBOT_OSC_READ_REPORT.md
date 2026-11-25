# OBSBOT Center OSC 只读通路

日期：2026-11-01，Asia/Shanghai。

已实现原生Rust的本机UDP只读传输、显式的软件客户端注册握手和显式Tiny 3格式配置。真实Center在显式配置下三项读取通过；默认官方三参数配置仍拒绝实际两参数云台回复。本轮没有执行运动、AI跟踪开关或选择设备命令；连接通知仅在单独授权的显式握手模式发送，不隐藏为Get查询。

## 资料与协议范围

- [OBSBOT官方OSC说明](https://www.obsbot.com/explore/obsbot-center/osc)：Tiny 3系列可通过运行中的OBSBOT Center使用OSC。该网页没有证明当前电脑的Center已运行或端口已配置。
- 官方下载表`runtime/vendor/obsbot_center_osc_definition20260508.xlsx`，表内日期2026/05/08；SHA-256：`a9d6cde69e074ee6e3c93c6714924735e06a165c00fd83de295101bd10d50c2e`。通过.NET ZIP/XML只读核对第70—78行。
- 同一官方页面的UDP TouchOSC示例，已保存为`runtime/vendor/sample-udp.xml`。其注册使用`Connected`单int1并处理`DeviceInfo`；本模块只在显式选择握手时发送一次，不运行示例中的定期注册循环。
- [OSC 1.0协议](https://opensoundcontrol.stanford.edu/spec-1_0.html)：本模块采用大端32位数值及四字节补零字符串；只支持消息与i/f/s标签，不接受bundle、blob、数组或地址通配符。设备名接受有界UTF-8扩展，地址及标签保持ASCII。

| 查询 | 请求地址 | 对应回复 |
|---|---|---|
| 设备信息 | `/OBSBOT/WebCam/General/GetDeviceInfo` | `/OBSBOT/WebCam/General/DeviceInfo` |
| AI锁定状态 | `/OBSBOT/WebCam/Tiny/GetAiTrackingInfo` | `/OBSBOT/WebCam/Tiny/AiTrackingInfo` |
| 云台角度 | `/OBSBOT/WebCam/General/GetGimbalPosInfo` | `/OBSBOT/WebCam/General/GetGimbalPosInfoResp` |
| 显式客户端注册以读设备信息 | `/OBSBOT/WebCam/General/Connected`，单int1 | `/OBSBOT/WebCam/General/DeviceInfo`；`ConnectedResp`仅记录诊断 |

三个Get请求显式传入设备索引和保留值0；握手例外，严格使用官方单int1。设备信息按官方四槽位结构解析，更新设备类型保留数值，不用旧类型表猜Tiny 3型号。AI回复中的0/1仅解释为unlock/lock，不能据此宣布厂商AI总开关已关闭。

`GimbalProfile::DocumentedRollPitchYaw`为默认，保持官方roll（当前不用）、pitch、yaw三整数格式，并拒绝两整数。`Tiny3Center2015PanPitch`必须显式选择，仅接受两整数，映射为yaw=参数0、pitch=参数1、roll=0。该扩展来自本机Center 2.0.15与原生只读同步交叉核验：原生Pan17/Tilt+24对应OSC `[17,-24]`，证据为`runtime/osc-read-rust-correlate.json`和`runtime/ptz-read-rust-correlate.json`。不把原生Tilt符号翻转后直接当OSC指令，也不声称Set方向已验证。

官方部分回复没有设备编号；此时`device_binding_verified=false`。带编号时必须等于请求编号。即使编号匹配，UDP仍无认证和请求nonce，迟到的同形回复无法被密码学绑定到本轮查询；这些记录不能授权运动或替代校准。

## 边界与接口

`rust/ai-tutor/src/obsbot_osc.rs`默认只把三个Get查询提供给传输层；额外的`DeviceInfoViaHandshake`必须显式选择。没有任意发送、设备设置或运动接口。

```rust
OscReadClient::bind(server_port: u16, reply_port: u16, timeout: Duration).await
OscReadClient::bind_with_profile(server_port, reply_port, timeout, gimbal_profile).await
client.local_addr()
client.query(query: ReadQuery, device_index: i32).await
client.handshake_device_info(device_index: i32).await
client.diagnostics()
```

仅绑定与发送到`127.0.0.1`；两个端口必须显式提供，`reply_port=0`明确表示请求系统分配临时回复端口。来源IP和端口必须匹配配置的服务端。当前接口独占借用串行查询，发送前有界丢弃排队旧包。若Center需配置固定回复端口，应显式传入其实际配置值，不能猜测。

- 数据报上限4096字节，地址128字节，字符串1024字节，参数32个。
- 单次查询超时必须大于0且不超过5秒；采用固定截止时间，异常数据不会延长等待。
- 错误来源、无关回复及旧队列最多丢弃16包；超过即报错。
- 非法UTF-8、非零补位、截断、额外尾字节、未知类型、非有限浮点数、错误设备编号及不符回复结构均拒绝。
- Windows UDP超大包导致系统接收错误时同样失败关闭，不解析截断数据。
- 设备索引0—255为应用资源边界，不是厂商支持数量声明。
- 仅在精确来源IP/端口匹配后，最多保留4个非预期的有界decoded消息，以及最后一个被拒绝的匹配回复；错误来源只记端点，不记录payload。另记录安全的OS错误码和错误类别。

## 实际执行的自动测试

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-rust.ps1 test --locked -p ai-tutor --lib obsbot_osc::tests -- --nocapture
```

profile阶段联合运行：13项OSC及8项motion测试共21项PASS、0失败，测试主体0.65秒。测试数据均为明确的合成fixture，没有读取真实设备。后续停止故障修复新增3项fixture，11项motion再次执行通过，详见`OBSBOT_MOTION_GUARD_REPORT.md`；不能当作实体运动验收。

| 测试 | 结果 |
|---|---|
| 独立已知字节序列、大端数值、标签与补零、三个只读请求参数 | PASS |
| 数据包、地址、字符串、参数数目边界 | PASS |
| 所有截断位置、bundle/blob、非法UTF-8、非零补位、尾字节、NaN/Infinity | PASS |
| 带/不带设备编号的保守来源记录、布尔值与角度范围 | PASS |
| 真正localhost UDP上的三种查询与回复结构 | PASS |
| 有效但错误来源端口的回复被丢弃，随后仅接收正确端口 | PASS |
| 只有错误端口回复时按50ms测试截止时间超时 | PASS |
| 真正UDP截断包及超大包失败关闭 | PASS |
| 必须显式端口、有界超时和异常包预算 | PASS |
| 仅可信源的最多4条非预期回复及最后被拒绝回复进入诊断 | PASS |
| 显式握手单int1、ConnectedResp诊断及真实query身份保留 | PASS |
| 本机交叉核验的两轴映射、保留负pitch且不套用原生Tilt符号 | PASS |
| 真正localhost UDP两参数仅显式profile接受，默认依旧拒绝 | PASS |

## 实际Center只读证据

端口16284来自用户打开的Center OSC设置与根代理确认的实际监听，没有扫描或猜测端口。以下是不同时间、不同状态的独立记录，不覆盖初次失败。

| 记录 | 实际结果 |
|---|---|
| `runtime/osc-read-rust.json`，12:12:56+08 | 三Get均接收失败；当时尚无可接纳设备状态 |
| `runtime/osc-read-rust-protocol.json`，12:18:22+08 | GetDeviceInfo超时；AI回复单int0；云台匹配地址却只有两个int `[17,-24]`，按表iii拒绝 |
| `runtime/osc-read-rust-handshake.json`，12:21:14+08 | 显式Connected(1)后DeviceInfo通过：唯一connected槽0、名称精确为`Tiny 3`、selected=0、running=true、类型码18；ConnectedResp单int1单独诊断。AI单int0/unlocked；云台仍两int `[17,-24]`，拒绝解析 |
| `runtime/osc-read-rust-profile.json`，12:29:22+08 | 显式握手及Tiny3Center2015PanPitch配置，三项读取均PASS：同一唯一设备/selected0、AI单int0、原始pose `[17,-24]`映射yaw17/pitch-24/roll0；Set、实际方向与停止未测试 |

上述启用OSC后的可接纳/被拒绝回复均来自`127.0.0.1:16284`，没有错误来源payload进入日志。握手还附带过云台 `[17,-23]` 和预设信息，均明确标记非预期消息，不当作本轮pose。显式profile只确认当前环境的读取解释，不证明运动方向、到位或停止；没有执行任何运动或AI开关。

## 只读辅助程序

源文件：`rust/ai-tutor/src/bin/osc-read-probe.rs`。没有设备设置、跟踪或运动命令选项。使用前由厂商软件负责确认真实端口与设备编号；下列PORT和INDEX是占位符，不能照抄当作默认配置。

已实际构建`target/debug/osc-read-probe.exe`；`--help`正常退出，缺少`--reply-port`或传入未知`--move`均在创建socket前拒绝。以上命令检查没有发送任何UDP数据。

```text
osc-read-probe --server-port PORT --reply-port PORT --device-index INDEX [--timeout-ms 1500] [--device-info-handshake] [--tiny3-center-2015]
```

默认只顺序发送三个Get；只有显式`--device-info-handshake`才把第一项换成一次软件注册，只有`--tiny3-center-2015`才接受已交叉核验的两参数读取格式。输出每项结果及来源、绑定范围、profile来源、耗时；任一查询失败则退出码2。全部查询通过只代表状态读取通路，不能升级为运动或自动跟随验收。
