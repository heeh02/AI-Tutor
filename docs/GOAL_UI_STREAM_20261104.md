# P2：Rust/Yew 二进制最新帧预览客户端

日期：2026-11-04。仅修改 UI 与展示样式，没有运行浏览器、设备、远程模型或读取生产学习数据。修改前备份在 `runtime/goal-backups/20261104/ui_review/`。

## 实现边界

`preview_stream.rs` 处理协议 v1、元数据与 JPEG 长度、每连接单调投递编号、单个待确认画面及本地代次。`preview_browser.rs` 只管理同源 WebSocket、Blob ObjectURL、页面可见性监听与绘制调度的生命周期。`CameraPreview` 仍独立持有全部画面工作，主页面只接收用户动作。未新增 JavaScript 业务逻辑。

协议以 [服务端合同](VIDEO_STREAM_20261104.md) 为准：4字节大端 metadata 长度、UTF-8 JSON、JPEG 原始字节；JSON 至多64KiB、JPEG 至多5MiB。每条 frame/state 有独立 delivery_id。ACK 完整绑定 delivery_id、frame_id、camera_session_id。HTTP 启动登录 cookie 继续用于同源 WebSocket，客户端不另外持有或拼入令牌。

## 解码、显示与失效

收到 frame 后创建一个 ObjectURL，替换图像来源；新的目标叠层暂不显示。实际可见 img 的 load 后调用 `HtmlImageElement.decode()`，确认对应来源与尺寸，再提交该帧叠层。组件 rendered 后安排两次 requestAnimationFrame，在其间获得一次绘制机会，再发送 ACK。它不是实际显示器呈现完成时间；真实 decode/paint 时序尚未在浏览器测量。客户端只保留一个未确认画面，不积压 JPEG 队列。

state 消息没有像素，立即清除图像和点击状态、撤销旧解码票据。如果原 frame 尚未 ACK，则发送一次原 frame 的 ACK，表示已丢弃处理完成；已确认的 frame 不重复 ACK。新连接、关闭、页面隐藏和错误均使旧回调失效。清图同时移除旧 img 的 src，使在途图像请求/解码取消；ObjectURL 在替换、清图或组件销毁时撤销；WebSocket 回调和可见性监听随其所有者释放。

帧龄使用服务端 frame_age_ms，并在 captured_at_ms 有效时计入同机传输等待；收包后用 performance.now 的单调经过时间累加。400ms 起禁止选择/清除该帧目标及启用跟随，1000ms 最迟清图（无像素 state 可更早清图）；点击处理时再次判断当前帧票据和时间，不能仅依赖按钮之前渲染时的状态。停止跟随入口仍独立保留。镜像只改变画面和框坐标，同帧元数据不回退到状态轮询中的旧选择。

## 兼容通道

默认二进制连接失败时显示错误与重试/兼容预览入口，不自动掩盖认证或协议错误。用户选择“使用兼容预览”后关闭 WebSocket，才开始原 HTTP 帧请求；切回 WebSocket 若旧 HTTP 尚未结束，先等待该请求释放单请求槽。兼容通道也经过实际 img decode、同帧点击和过期检查。界面明确标注当前通道。

关闭、connecting/reconnecting/opening、running 均按当前相机状态控制连接；关闭态不启动视频请求，状态仍来自主页面的低频轮询。相机关闭操作不依赖是否已有 JPEG，避免网络错误时误将关闭按钮变成设备重开操作。

## 验证记录

新增4项可运行的纯逻辑回归：二进制像素/元数据与无像素状态；无效/超限/不匹配协议；state 取消解码、单容量与旧 ACK 不释放新帧；重连代次、本地帧龄和传输等待。既有清选中目标测试调整为缺字段也不从旧状态恢复目标。

本子任务执行 rustfmt --check；根任务已完成 UI 39 项逻辑测试通过。随后清图时移除旧 img src 的小修复已完成，最终 Cargo/WASM 编译与统一回归结果由根任务补充。此前 P0/P4 的304项 workspace通过、UI35项及WASM release通过不可代替本阶段验证。

浏览器 Computer Use 上一阶段因无法可靠确认当前网址主动中止，本阶段没有重试或绕过，也没有声称实际页面 decode、渲染耗时、持续内存、带宽或帧率通过。后续获准浏览器验收需覆盖：正常JPEG解码、慢解码期间state、目标切换、隐藏/恢复、断连/重连、失败后HTTP/WS互切与ObjectURL长期释放。真实相机捕获/多人运动/PTZ验收仍为独立事项。

浏览器资源行为依据：[HTML Standard — img.decode](https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-decode) 将图像请求变化列为 decode promise 拒绝条件；规范同时说明 decode 可与 requestAnimationFrame 配合。这支持取消与绘制调度设计，不能代替本机浏览器实测。
