# 视觉目标歧义与阶段计时（2026-11-03）

本轮仅修改视觉软件及固定轨迹测试，没有开启摄像头、操作实体云台、改换检测模型或追踪算法。原件备份：`runtime/goal-backups/20261103/architecture_audit/vision.rs`。

## 目标局部歧义

此前任意两个人脸框重叠都会设置全局 `association_ambiguous`，导致画面另一侧无关的两人交叉也清除已选目标。

现在采集辅助进程仍使用原阈值：交叠面积占较小框面积 **大于 0.25** 时，两个检测框均标记为有歧义。计算发生在未成熟／未分配轨迹的过滤之前；低置信或新出现的第二张脸仍能使与其重叠的已确认目标失效。

辅助进程数据新增：

- `association_ambiguity_scope = "per_track_v1"`。
- `ambiguous_track_keys`：参与交叠的可选轨迹编号。
- `scene_association_ambiguous`：保留全场景诊断。
- 各 `tracks[]` 的 `association_ambiguous`：便于查看具体轨迹。

主进程只按当前已选目标／用户准备选择的目标查询歧义。现有 `status`、`frame`、`latest` 输出中的 `association_ambiguous` 仍保留，但新范围数据按当前目标解释。未选择时不自动绑定任何人。涉及所选目标的歧义立即清除选择，之后即使人物分开也不会自动重新选中。

旧数据包没有新范围字段时保留原全局判断；未知范围、缺失或无效的编号数组按歧义处理。未修改 PTZ 发送门限、400ms 可见性过期、超过 600ms 清除目标、采集 epoch 或重复／倒序帧拒绝逻辑。

## 阶段计时

新增 `timings_ms`，单位毫秒、时钟为 Rust `Instant`：

| 字段 | 测量范围 |
|---|---|
| `preprocess` | letterbox、像素归一化、输入张量构建 |
| `ort_run` | 同步 ONNX Runtime `Session::run` 调用 |
| `output_decode` | 提取输出张量、检查形状、解码与坐标还原 |
| `nms` | 候选排序、裁限和非极大值抑制 |
| `tracking` | 本次真实 ByteTrack `update` |
| `association_metadata` | 局部交叠判定、轨迹标签与输出元数据构建、必要的标签清理 |
| `jpeg_encode` | RGB 路径的预览 JPEG 编码；复用原生 MJPEG 时为 0 |
| `preview_prepare` | 取得预览 JPEG 的步骤，包含复用或编码 |
| `latest_slot_wait` | 帧写入最新帧槽位前的时间戳至处理循环取出后的时间差 |
| `capture_to_process` | `camera.frame()` 返回后至本帧处理开始，包含姿态读取、解码、槽位等候 |

既有 `read_sample_ms`、`decode_ms`、`inference_ms`、`processing_ms` 保留原语义；`inference_ms` 仍为整个检测路径，不改称纯 ORT 时间。`preview_jpeg_reused` 区分复用和重编码。

`frame_age_origin = "after_camera_frame_return"` 明确帧龄原点尚不是传感器曝光或 Media Foundation 的采样时钟。本轮没有新增或宣称传感器到屏幕端到端延迟；字段不包含管道写出、HTTP/Base64传输、浏览器解码与显示。各阶段可能嵌套，不能将全部字段直接相加得到端到端延迟。旧30分钟实机数据不能套用到本次改动。

## 固定轨迹回放与验证边界

`rust/ai-tutor/src/vision_replay_tests.rs` 使用确定的框坐标和时间步，真实调用现有 ByteTrack、生产歧义函数和选择状态 reducer；不访问摄像头、远程模型或运动接口。

新增测试覆盖：

1. 一人、两人、三人缓慢移动时保持显式目标。
2. 无关两人交叉：保留全景歧义诊断，不使所选目标代次改变。
3. 所选人物交叉：清除绑定且不自动重选。
4. 短遮挡保持同一 UUID；600ms边界及超过边界的长丢失。
5. 非线性轨迹，输出框保持实际检测框，不将预测框伪装成观测。
6. 重复／旧帧拒绝、陈旧帧及采集 epoch 更新。
7. 可以选择无关清晰目标，但拒绝选择交叠人物。
8. 旧包、未知或损坏的新范围数据保守拒绝。
9. 未成熟／未分配的重叠检测仍使成熟目标失效。

截至本文件创建：已完成 `rustfmt`；Cargo 回归交由根代理统一执行，尚不能将测试定义写为通过。真实多人遮挡、实际设备重新连接、阶段耗时分布及物理跟随均待独立现场验证。回放只覆盖给定检测轨迹，不证明真实相机检测质量或身份识别能力。
