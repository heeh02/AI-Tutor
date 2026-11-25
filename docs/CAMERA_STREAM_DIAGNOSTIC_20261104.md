# 原生主程序摄像头与二进制预览诊断

`scripts/validation/camera_stream_smoke.py` 只作独立诊断。冻结方法为相邻的 `camera_stream_method_v1.json`：正常接收 60 秒、每帧解码后延迟 ACK 150 毫秒的慢消费者 10 秒、关闭并重开后 15 秒，分别统计。时长可显式缩短到至少 2 秒；实际参数写入每段报告，不能把短跑写成长跑。

## 运行入口

由根代理在统一构建后明确指定 checkpoint11 或后续待测 release，不能把历史 `bin/ai-tutor.exe` 当作当前源码构建：

```powershell
.venv\Scripts\python.exe scripts\validation\camera_stream_smoke.py --real-camera --root . --exe target\release\ai-tutor.exe --ui frontend\dist --single-camera --output runtime\goal-baselines\20261104\camera-stream-first
```

`--single-camera` 仅在本次原生枚举恰好一项时打开该明确 ID；多个/没有设备退出，不猜默认设备。可改用 `--device-id-file <UTF8文件>` 提供已选设备完整 ID，必须在本次枚举中唯一存在。脚本不打印设备 ID，报告只存其 SHA256，退出后移除新 fixture 的已核对选择文件。输出目录必须不存在。

现有诊断依赖固定为 websockets 15.0.1、Pillow 12.3.0、psutil 7.0.0；缺失或不同则失败，不安装依赖。实际采集、JPEG 解码供检测、YOLO、ByteTrack、IPC 与 WebSocket 服务仍由指定 Rust 主程序及其原生 worker 执行，Python 仅接收服务的 JPEG 并独立解码。

## 隔离与清理

复用 `OwnedWorkstation`，创建全新 fixture、空数据库、隔离环境和关闭的本地 ASR/TTS 策略。只按冻结哈希复制一个约 12MB face ONNX 与两份约 16MB/22KB ONNX Runtime DLL；不复制生产数据库、语音模型、PTZ 标定策略或服务凭证。主程序自行通过 Job Object 启动现有 `--camera-worker`，脚本不直接另开摄像头 worker。

HTTP 白名单只有 state、camera 枚举/open/close 和 shutdown；视频只接 `/api/video/stream`，无 HTTP base64 并行消费者。不存在选择目标、PTZ、麦克风、播音或远程教学接口。每秒核对音频 idle、PTZ mode off/命令计数零、无选中目标、远程请求计数零。

psutil 只读观察本次 Popen 主程序的直接子进程，要求程序绝对路径、`--camera-worker` 和 fixture root 匹配，记录 PID/创建时间防复用。每次 close 后检查已观察 worker 不存活，且无新增匹配 worker；重开必须观察到新 worker 与新 camera epoch。最后正常 shutdown、main exit 0、无强制回收才通过。失败也保留已完成分段/部分数字及清理结果，不按进程名或端口杀别的进程。根目录策略/描述符及源资产只读哈希前后核对；新 fixture 内凭证由既有 helper 精确移除。

根目录已安装的 `bin/ai-tutor.exe` 也在前后哈希保护范围内。这些证据证明本应用没有发 PTZ 控制请求、已观察进程已退出；不证明厂商自主跟随关闭、实体云台完全静止或独立的物理设备释放时间。

## 指标与限制

- 每帧只写白名单数字、固定阶段名与布尔字段：receive elapsed、帧号/交付号、JPEG 大小、消费端解码与 ACK 时间、worker 既有分阶段计时。不保存 JPEG、视频、截图、脸框或 native pose 内容。轨迹只保留 key 哈希对应的首次/末次观察、次数、最大观察间隔和歧义次数。
- 每段记录实际宽高、协商的 pixel_format/requested_fps/driver_reported_fps；`format_observations` 保留首次格式及后续每次变化的帧号与时间。每帧保留实际宽高与 JPEG 是否复用，分段汇总复用/重新编码次数，不能仅凭请求 720p 推定实际分辨率。
- `captured_at_ms` 是 Rust `camera.frame()` 返回后的同机墙钟。`capture_return_to_receive_ms` 及加上本地单调解码时间的 `capture_return_to_consumer_decode_done_ms` 都从此处起算。对墙钟与单调钟累计偏差超过 50ms 或无有效捕获时间的帧不计算该年龄，明确报告分母。
- Pillow 的 `Image.load()` 是该诊断客户端真实 JPEG 解码完成，不是 Yew 的 `HtmlImageElement.decode()`、rAF、浏览器绘制或屏幕实际呈现。所有结果明确 `browser_decode_or_paint_measured=false`。
- `received_unique_fps` 使用同段首末帧之间的接收跨度；分位数为 nearest-rank。worker `capture_fps/fps` 是原模块累计统计，另外保留，不替换成诊断客户端帧率。
- 首轮版本的 WS 每 40ms 取样，单消费者 JPEG 约至多 25fps。frame_id 空洞包含最新槽覆盖、处理抽取和 WS 抽取，只称“本消费者未收到的 capture ID”，不声称网络丢包或把各阶段丢帧强行分解。
- 慢消费者持续读取无像素 state；ACK 前如果出现第二张 JPEG 直接失败。ACK 始终绑定原 frame 的 delivery_id/frame_id/epoch，即使期间收到新的 state。
- 三段均不自动选目标；有脸时记录已确认轨迹统计，无脸则零观察，均不声称人物身份准确率或目标保持验收。无人工真值、儿童/多人遮挡、浏览器表现或物理云台跟随结论。

通过条件仅为协议、解码、归属、隔离及退出不变量；没有为了得到绿色结果设置延迟或帧率合格阈值。速度分布是新基线。

首次纯测试为 3 通过、1 失败，失败来自超预算测试夹具：100000×2×3 仅 600000 字节，未超过冻结的 32MiB 总分配上限。已将该夹具改为 100000×100000，未新增长边限制、未改诊断脚本或方法；首失败日志保留，第二次 4/4 通过。

## 首轮实际基线

根代理运行 `runtime/goal-baselines/20261104/camera-stream-first/report.json`，指定 checkpoint11 release（SHA256 `cb62e4ebbcc7a7000efd89d00aac7057d25aa2d2c57b8e9b87b5b9256545d20a`），三段全部完成：

| 分段 | 配置时长 | 接收帧数 | 接收 fps | 捕获返回至 Pillow 解码完成 P95 |
| --- | ---: | ---: | ---: | ---: |
| 正常 | 60s | 1340 | 22.313 | 106.26ms |
| 慢 ACK 150ms | 10s | 53 | 5.213 | 105.51ms |
| 关闭重开 | 15s | 351 | 23.271 | 104.89ms |

三段实际格式均为 1280×720 MJPEG、请求 30fps，JPEG 全部复用。驱动 `driver_reported_fps=1` 是格式元数据，不是真实采集速度；正常段采集累计速度中位数 29.817fps，处理累计速度中位数 24.553fps；重开段两者为 29.274/26.467fps。所有帧均无脸/轨迹观察，因此没有目标保持或人物识别结论。

主进程 132980 正常 exit 0，观察到的两个归属正确的 camera worker 均在 close 后退出，无强制回收；已安装程序、源资产及策略哈希未变。音频、目标选择、PTZ API 调用均为零。该运行不涉及浏览器或物理呈现，也不证明实体云台完全静止。

基于上述实际采集/处理与接收差距，第二版仅调整预览发送调度：frame watch 通知与 ACK 后立即检查最新槽，40ms tick 继续处理年龄变化和 ACK 超时；每连接最小帧间隔 33,333,334ns（上限 30fps），需等待时 `sleep_until`，到期重新读取最新帧，保持只有一个未确认 JPEG。没有新增帧队列或调整冻结测量方法。新增 pacing 边界与真实 loopback burst 合并回归。

## 第二轮与同方法对照

`runtime/goal-baselines/20261104/camera-stream-second/report.json`（SHA256 `df6154360953966bb783293c821755e409f951849fea2c2bfe2aaad9df732161`）使用 native release 第十八次构建，exe SHA256 为 `105d2c9ce50052de0d87e3aa7a633171a83ac1e678f83621d313a5546ccc07ba`。脚本和冻结方法哈希与第一轮相同，三段时长也保持 60/10/15 秒。

| 分段 | 第二轮帧数 | 第一轮 → 第二轮 fps | 第一轮 → 第二轮解码完成 P95 |
| --- | ---: | ---: | ---: |
| 正常 | 1577 | 22.313 → 26.256 | 106.26 → 80.65ms |
| 慢 ACK 150ms | 63 | 5.213 → 6.246 | 105.51 → 115.14ms |
| 关闭重开 | 393 | 23.271 → 26.137 | 104.89 → 81.66ms |

正常段接收 fps 增加 17.67%、解码完成 P95 降低 25.61ms；重开段分别增加 12.32%、降低 23.23ms。慢 ACK 段吞吐增加，但 P95 **上升 9.63ms**，不能概括为各条件延迟均下降。这是两次顺序运行，未做 ABBA/随机交叉重复，不能排除机器负载等差异，更不能把上限 30fps 写成已达到的实际帧率。

第二轮仍为实际 1280×720 MJPEG、JPEG 全部复用、零人脸/轨迹；主进程 142592 exit 0、两个已观察 worker 在 close 后退出、无强制回收，已安装程序与策略/源资产哈希不变。完整诊断耗时 87.172s。两轮都没有浏览器 paint/屏幕呈现、目标选择或跟随质量结论，也没有进行 30 分钟语音与视频联合负载验收。

独立复算文件为 `runtime/goal-baselines/20261104/camera-stream-audit-first-final.json` 与 `camera-stream-audit-second.json`：逐帧数量、帧率、所有 nearest-rank 分布、P95、宽高变化、JPEG 复用计数及 capture ID 空洞与各自报告一致。慢 ACK 无第二 JPEG 由冻结脚本运行时检查与完成状态支持；未存原始网络包，审计不能重新构造包序列。pixel_format/requested_fps/driver_reported_fps 仅见报告格式观察，未逐帧重复保存，审计明确保留这一证据范围。

统一验证日志 `workspace-thirty-first.log` 汇总 **710 通过、0 失败、5 忽略**；`native-release-eighteenth.log` 构建成功。这些软件回归及两次短时设备运行，不替代尚未完成的真人跟踪、浏览器和长期联合验收。
