# 二进制最新帧预览（2026-11-04）

实现位于 `rust/ai-tutor/src/video_stream.rs`，路由为 `GET /api/video/stream`。它在现有 `server::router` 中，沿用本机 Host、同源 Origin、SameSite/HttpOnly 启动 cookie 认证；没有额外令牌或跨域入口。`/api/video/frame` 保留为界面显式切换的 HTTP 备用通道。

## 协议 v1

每个服务端 WebSocket binary 消息只有一条完整消息：

```
4 字节 big-endian JSON 字节数 | UTF-8 JSON metadata | JPEG 原始字节
```

metadata 沿用 HTTP 帧字段（`frame_id`、`camera_session_id`、`tracks`、`state`、`frame_age_ms`、`captured_at_ms`、选中目标和代次等），移除 `jpeg`，增加：

- `protocol_version: 1`。
- `message_type: "frame" | "state"`。
- `delivery_id`：每连接从 1 开始、每条消息严格递增；新连接重新计数。
- `jpeg_length`：尾部 JPEG 精确字节数；state 为 0。
- `preview_available`：运行状态且当前画面年龄不超过 1000 ms。
- `preview_selectable`：运行状态且当前画面年龄小于 400 ms。

frame 的图像、检测框、epoch、frame_id 和当前目标选择来自同一次持锁快照。state 没有图像；关闭/连接中可能没有 frame_id 和 epoch。state 用于撤销旧画面的点击与尚未完成的解码提交，不代表另一张可点击画面。不需要客户端 ready。

浏览器完成图像解码、叠层更新和绘制调度后，发送一条文本 ACK：

```json
{"protocol_version":1,"type":"ack","delivery_id":3,"frame_id":12,"camera_session_id":"example-epoch"}
```

字段必须完整、没有额外字段，精确对应待确认 frame。state 自身不 ACK。state 到来时，客户端撤销旧解码/画面；如果旧 frame 尚未完成，丢弃处理完成后仍 ACK 旧 frame。**服务端发送 state 不会清除待确认 frame**，所以旧 ACK 不可能错误地释放新 frame 的背压。重复 ACK、错误 delivery_id/epoch/frame_id 或没有待确认 frame 的 ACK 会关闭连接。

## 资源与时效边界

- 每相机最多 4 条视频连接；超限在 HTTP upgrade 前返回 429。
- 每连接最多 1 张 JPEG 未 ACK；只保存最近已发送 stamp、待确认 stamp 和一个状态摘要。相机只有一个最新帧槽，不为客户端积压帧。
- 相机接受新帧、open/close/select/clear 后发布合并的 `watch<()>` 通知，不携带或排队保存像素。新通知与 ACK 后立即检查最新槽，丢弃中间帧；相同 epoch/frame_id 不重复发送 JPEG。
- 每连接 JPEG 发送起点至少间隔 33,333,334 ns（上限 30fps）。若新帧到达过早，使用 `sleep_until` 等待，到期再读取当时最新快照；没有忙轮询、等待帧队列或空闲后突发补发额度。慢 socket 发送本身已消耗的间隔不会重复等待。
- 相机 phase/epoch、选择/目标代次、选中状态、可用/可点状态变化时，发送无 JPEG 状态消息；即使等待 ACK 也能撤销旧画面。通知按最新状态合并，状态消息不受 JPEG 帧间隔约束。保留的 40ms tick 负责年龄变化、没有通知的状态变化与 ACK 超时兜底，不再是新帧的唯一唤醒来源。
- ACK 超时为发送完成后 2 s；每次 socket 发送或关闭等待上限 500 ms。关闭连接释放连接名额。
- metadata 上限 64 KiB、JPEG 上限 5 MiB、ACK/客户端单帧/聚合消息上限 512 B；客户端每秒最多 64 条消息。packet 超限不发送。
- 数值标识限制在 JavaScript 安全整数范围，epoch 最长 128 UTF-8 字节。
- 界面按服务端帧龄加本地经过时间计时，400 ms 禁止点击，1000 ms 清图；生产 metadata 还包含同机 `captured_at_ms`，可计入传输等待。隐藏页面、关闭、断线或连接代次改变必须撤销旧解码和对象 URL。

CameraService 的 JPEG 改为不可变共享 `Arc<Vec<u8>>`：状态查询不复制像素，预览快照只克隆 Arc；每个真正发送的 WS 消息为连续消息组装一次字节拷贝，不做 base64。HTTP 备用路径的 base64 编码已移到相机锁外。尚未迁移相机采样时钟或检测器。

## 验证与边界

`video_stream_tests.rs` 使用临时目录、真实 loopback HTTP/WS 连接和合成的 1×1 JPEG，覆盖：认证/跨域拒绝、连接上限和断线释放、未 ACK 时不发送第二张 JPEG、ACK 后跳过中间帧、epoch 重开、旧 ACK 不释放新帧、选择改变/关闭/过期的无像素状态同步、ACK 超时、恶意及超限 ACK、JPEG 超限、HTTP 备用路径。协议单测检查元数据超限、版本、未知字段和安全整数；现有相机快照测试增加 Arc 指针共享断言。

首轮实际设备报告 `runtime/goal-baselines/20261104/camera-stream-first/report.json` 对应原 40ms 取样版本：正常 60s 接收 22.313fps，捕获返回至诊断客户端 Pillow 解码完成 P95 106.26ms；关闭重开 15s 为 23.271fps/P95 104.89ms。慢 ACK 150ms 为 5.213fps，单帧背压与退出检查通过；三段都是 1280×720 MJPEG、零人脸观察。详见 `docs/CAMERA_STREAM_DIAGNOSTIC_20261104.md`。这不是浏览器呈现或人物跟踪准确率验收。

当前 watch/ACK 唤醒与 30fps pacing 版本已通过统一回归：`workspace-thirty-first.log` 共 710 通过、0 失败、5 忽略，`native-release-eighteenth.log` 构建成功。新增 pacing 边界和 loopback burst 合并测试纳入其中。

同冻结脚本/方法第二次设备报告 `runtime/goal-baselines/20261104/camera-stream-second/report.json`：正常 60s 为 1577 帧、26.256fps、P95 80.65ms；重开 15s 为 393 帧、26.137fps、P95 81.66ms，均较首轮改善。慢 ACK 150ms 为 63 帧、6.246fps，P95 从 105.51ms **升至 115.14ms**，没有隐藏这一退化。两轮逐帧独立审计与报告一致，第二轮进程/worker 清理正常，无强制回收，安装与策略哈希不变。

两轮都是 1280×720 MJPEG、零人脸；这是两次顺序对照，非 ABBA/随机交叉实验，不能推断跟随质量或稳定人物关联。未进行 30 分钟语音视频联合运行，浏览器 decode/paint、长期浏览器内存和实际屏幕呈现仍未验证；合成网络测试及 Pillow 解码不能替代这些指标。完整数字、哈希和审计边界见 `docs/CAMERA_STREAM_DIAGNOSTIC_20261104.md`。
