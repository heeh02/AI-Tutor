# 同源检测回放：ByteTrack 与 OC-SORT 对照方案

本轮仅完成源码与官方资料核实、测试设计；未替换生产追踪器、未操作摄像头、未运行 Cargo 或下载模型。建议先用官方 Python OC-SORT 做离线基准，再决定是否实现并启用 Rust 候选。论文和上游数据集成绩不能证明本机教育场景获益。

## 已核实实现与许可

当前生产依赖固定为 `edgefirst-tracker=0.38.2`，本机包的 `.cargo_vcs_info.json` 指向 `1a985032bdb21bb89911953d557cf033a8f08ae9`。它只导出 `bytetrack`、`kalman` 和 Tracker 接口，没有 OC-SORT；许可为 Apache-2.0。[同版本源码](https://github.com/EdgeFirstAI/hal/blob/1a985032bdb21bb89911953d557cf033a8f08ae9/crates/tracker/src/lib.rs#L44)

官方 OC-SORT 仓库本次核对固定到 `8462e7e729a93ccd3bd995c0a79a890336cb3a0b`（2026-04-21），仓库 LICENSE 为 MIT；移植须保留相应版权/许可声明。不要从名字相似的第三方简化 IOU tracker 代替它。[官方仓库与许可](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/LICENSE)、[原论文 v3](https://arxiv.org/abs/2203.14360v3)

真实 OC-SORT 候选至少保留：

1. 七维位置/面积/纵横比与速度 Kalman 状态、原始初始化和 predict/update 顺序。[KalmanBoxTracker](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/ocsort.py#L58)
2. OCM：用历史观察估计方向，把方向一致性和检测置信度加入关联成本；仍通过 LAP 求分配并检查 IOU 门限。[associate](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/association.py#L251)
3. OCR：用最后实际观察位置，对未匹配检测/轨迹再次关联。[回配阶段](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/ocsort.py#L280)
4. ORU：首次缺观测时冻结滤波状态；再次观察后，沿两次观察间的虚拟轨迹重放更新，修复遮挡期累积误差。[freeze/unfreeze](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/kalmanfilter.py#L383)

`use_byte` 是上游可选低置信度关联分支，默认 false；对照报告必须分别标识 OC-SORT 原始配置与 OC-SORT+BYTE，不能合并后择优声称同一种算法。[参数及 BYTE 分支](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/ocsort.py#L179)

官方三份核心 Python 文件可直接接收 NumPy Nx5 检测矩阵。该路径依赖 NumPy、FilterPy，分配求解使用 LAP，缺 LAP 时回退 SciPy；不必安装整个 YOLOX/Torch/ONNX 推理栈，也不下载权重。[核心入口](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/ocsort.py#L197)、[LAP/SciPy 分支](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/trackers/ocsort_tracker/association.py#L188)。实现时在独立小环境锁定实际版本和源码 SHA，不能把上游未固定版本的完整 requirements 当作可复现锁文件。

本次不建议先接官方 C++ DLL：该提交 CMake 硬编码 Eigen/OpenCV Windows 路径，仍在全局查找 OpenCV，并引用仓库中未找到的 `test_fixes.cpp`。修补构建、FFI 和数值一致性会增加本阶段工作；它不是已经验证的 Rust 直接替代。[CMake](https://github.com/noahcao/OC_SORT/blob/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/deploy/OCSort/cpp/CMakeLists.txt)、[固定提交目录](https://github.com/noahcao/OC_SORT/tree/8462e7e729a93ccd3bd995c0a79a890336cb3a0b/deploy/OCSort/cpp)

## 可复现输入与缺口

现有 `vision_replay_tests.rs` 已有真实 ByteTrack 的 1/2/3 人、无关交叉、涉及所选目标交叉、短遮挡/600 ms 边界、非线性运动与 epoch/旧帧回放，可抽取为共同轨迹生成器。现有输入多数置信度固定 0.9、按 33 ms 步进；必须增加低置信度下降、误检、框尺寸变化、检测顺序打乱、10/15/30 Hz 与不规则丢帧。低置信度测试使用生产检测器保留范围（当前最低 0.15），并单列 0.4 门限附近的行为。

共同 JSONL 每行只保存 `epoch/frame_id/timestamp_ns/frame_age_ms/width/height/detections[{index,xyxy_px,score,class,fixture_truth_id}]/selection_action`。truth_id 来自合成生成器，绝不作为 tracker 输入。ByteTrack adapter 转回生产归一化坐标；OC-SORT 用像素坐标并使 img_info/img_size 相同，避免额外缩放。两者接收同一几何观察、置信度和顺序，报告各自坐标单位/参数；不强行改变生产 ByteTrack 的数值配置。

检查到的相机历史证据主要是帧龄、耗时、人数和状态摘要：`runtime/rust_camera_probe.json`、`camera_probe.json`、526 条 JPEG soak 统计，没有找到逐帧原始 detection boxes + 可核验目标真值的完整 fixture。不能把这些汇总当作带身份标注的真实轨迹。下一次授权设备验收可仅导出检测框/分数/相对时戳与人工临时身份标签，不保存照片；在此之前，真实场景准确率标为未验证。

对照分两层记录：

- 算法层：每个原始 detection index 对应的 track ID、确认计数和内部关联结果。OC-SORT 返回矩阵并不按输入检测索引排列；应在官方匹配/创建位置加仅观测 trace，记录其输入索引。不能用另一次 IOU 最近邻重新猜映射，那会污染交叉场景的 ID switch 指标。trace 补丁独立记录哈希，不修改关联成本或状态更新。
- 产品层：两种关联结果均经过现有 `ambiguous_detections`、`tracked_faces` 与 State 选择生命周期。统一三次观测确认门槛；记录 OC-SORT 原始 min_hits/hit_streak 输出策略与适配层门槛的区别。始终保持涉及目标歧义清除、无关交叉不清除、400 ms 可见性、超过 600 ms 清除、不自动重新选中、epoch/旧帧拒绝和 250 ms PTZ 发送门限。

官方 OC-SORT 的 max_age、delta_t 是帧数；当前应用选择生命周期按时间，ByteTrack 的轨迹过期也接受时间戳。固定帧率实验可以注明换算，丢帧实验必须保留真实时间并分别报告算法帧龄和产品时间门限，不能用“18 帧”替代所有情况下的 600 ms。

## 决策指标

| 指标 | 计算/解释 | 建议验收条件（尚未实测） |
| --- | --- | --- |
| 选中目标误绑定 | selected key 实际关联到非选中 truth_id 的帧数及事件数 | 所有带真值回放为 0；有一次即不启用候选 |
| 不安全恢复 | 歧义清除、长丢失或 epoch 变化后自动恢复选择的次数 | 0，且原有安全回放全部通过 |
| ID switches / fragmentation | 同一可见 truth_id 的 track ID 变化与轨迹分段数 | 单独报告完整轨迹与短遮挡段，不能只统计未被策略清除的帧 |
| 遮挡恢复 | 100/300/500/600/601 ms 后同 ID 恢复率；重现至恢复的 ms/帧数 | ≤600 ms 的可辨认轨迹不劣于基线；>600 ms 产品层不得自动选回 |
| 无关干扰 | 所选目标未受遮挡时旁人交叉造成的清除次数 | 0，防止通过过度清空选择“消除误绑定” |
| 可用率 | 目标真实可见且新鲜时，正确选中可用的帧占比；不必要重新点选次数 | 与误绑定同时报告；有安全性但长期不可用也不算改进 |
| 耗时 | release 下纯 update 的 p50/p95/p99/max；adapter/元数据另计 | 1–3 人初始预算 p95≤2 ms、p99≤5 ms；这是待测工程预算，不是上游承诺 |
| 长时内存 | 同一轨迹 30 分钟回放后历史状态/分配是否随时长增长 | 稳态有界；不得复制完整历史导致遮挡恢复越来越慢 |

每例先预热，再相同 fixture 重复运行至少 100 次记录耗时分布；算法质量按每个固定轨迹计一次，不把重复回放当作 100 个独立样本。控制检测数量 1/2/3，另做 10 人压力样本；首次不运行图像解码/检测/网络，随后再用原有 `timings_ms.tracking` 单独核对整链阶段比例。官方 700+ FPS 是给定检测的特定 CPU benchmark，不是 RTX5070 主机摄像头端到端帧率。[论文性能口径](https://arxiv.org/abs/2203.14360v3)

## 最小 Rust 接入边界

先建离线 `tracker_compare` runner 和官方 Python oracle，固定输入与 trace 生成 golden 数据，再实现独立 `tracking/ocsort.rs`。只定义本项目需要的“检测索引 → 临时轨迹标识/观测次数”小接口，把现有 ByteTrack 包装一次，不造通用追踪平台。

Rust 候选必须对官方实现逐步校验 bbox↔state、方向成本、LAP 分配、缺观测 freeze、重新观察 ORU、OCR 和轨迹删除。等价成本 tie 情况明确排序策略，使用 f64 校验数值容差；不能仅靠最终 ID 总数相同。官方 Python 保存 observations/history_obs 的方式会随长轨迹增长，离线有限长度可接受；Rust 正式实现需保存足够的方向窗口与最近缺观测恢复窗口并以 golden 回放证明裁剪不改变关联，不能照搬无限历史。

即使 OC-SORT 合成指标更优，也只先作为非默认候选；有授权的同源真实检测回放、稳定性和耗时证据之后，才讨论默认切换。它是纯运动模型，不提供真实身份确认，完全不可辨认的交叉不能承诺始终保持同一人。
