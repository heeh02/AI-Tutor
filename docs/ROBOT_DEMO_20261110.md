# AI Tutor 3D Demo（2026-11-10）

交付目录：`C:\Users\RBMS\.cache\ai-tutor-3d-demo-20261110`。双击 `AI Tutor 3D.lnk` 打开独立 Edge 应用窗口，`网页版本.lnk` 使用默认浏览器，`停止 AI Tutor 3D.lnk` 正常关闭主程序及其模型服务。关闭页面本身不等于停止服务。

主程序与界面保持 Rust / Yew，3D 部分使用随包提供的 Three.js r180，模型由本地隔离服务运行。学习数据属于新建模拟档案，没有复制原安装的学习数据库或凭证。原安装未替换。当前 Demo 服务仍运行，供用户体验。

## 已落地与实际验证

- 奶白色 3D 机器人、中文/英文界面、桌面与手机布局、教学对话、可折叠的语音/实验/学习记录区域。
- 上一轮真实页面只发送了一次中文 Flash 问题，收到两段已校验教学回复并完成 Qwen/native PCM 播放。当前读回仍为 requests=1、retries=0；播放收据不等于声学首音测量。
- 机器人从 `/api/camera/pose` 接收已有采集 owner 的 Windows 原生控制器读回；不通过 Center / OSC 读取，不使用目标角度或上次运动指令冒充当前角度。控制器读回不等于独立编码器测量。
- “体验动作”只作用于屏幕模型。真实网页已观察向左看 +25°，退出演示后无设备时恢复未知角度。此过程没有设备命令。
- 本轮修复慢响应与帧龄遗漏导致旧姿态重新显示实时的问题：用请求开始的单调时钟计算完整往返及显示时间，同时检查样本龄和帧龄；未经单独计时的通用状态快照不宣称实时。
- 摄像头会话变化立即失效旧显示并清除 JS 姿态缓存；同会话短暂失联可保留最后姿态并标为非实时，跨会话不能恢复旧设备角度。卸载时释放场景网格引用。
- 未保存比较草稿阻止导航时，先展开外层折叠区域再聚焦原表单。实际页面复现并通过：草稿保留、表单获得焦点、活动未切换。测试草稿随后清空，未保存实验记录或新增模型请求。
- 启动器不再把状态探测失败误报成已经停止；并发启动时等待赢得实例锁的服务就绪。

## 本轮检查

| 检查 | 结果 |
| --- | --- |
| tutor-ui | 140 通过，0 失败 |
| 3D 数据与生命周期 | 6 通过，0 失败 |
| 启动器生命周期 | 7 通过，0 失败 |
| WASM release | 成功，22.684 秒 |
| 已交付 UI / 启动器身份 | 12 个 UI 文件及启动器哈希与新构建一致 |

3D 生命周期测试使用真实 Three.js 场景对象、事件/RAF 夹具与 renderer sink；覆盖会话更换、演示退出、隐藏/恢复、上下文恢复和卸载重挂载，不是实际 GPU 重置测试。启动器异常场景为无服务 mock 测试；实际 `--no-open` 已确认复用当前进程。

证据：[manifest](../runtime/goal-baselines/20261110/robot-demo-lifecycle/manifest.json)、[页面与状态复验](../runtime/goal-baselines/20261110/robot-demo-lifecycle/verification.json)、[真实 Demo API 读回](../runtime/goal-baselines/20261110/robot-demo-lifecycle/api-observation.json)。完整日志在 `runtime/goal-baselines/20261104/robot-demo-lifecycle-*`、`robot-demo-renderer-lifecycle*`、`robot-demo-launcher-check*`。原生程序本轮未变，不把此前原生测试写成本轮重新运行。

## 仍待完成

当前摄像头未被 Windows 枚举，真实角度/方向/延迟联动尚未验收。已找到签名厂商 `libdev.dll`，但匹配 Tiny 3 的 Windows SDK ABI 尚未确认，运动控制未替换或开启；见 [直连 SDK 交接](OBSBOT_DIRECT_SDK_20261110.md)。桌面窗口入口已启动，但窗口内 CUA 验收因无法可靠识别网址被安全检查中止；本轮仅复验受支持的网页入口。

完整 P0–P5 goal 保持 ACTIVE。自然麦克风、声学打断/真实首音、当前 Flash 与相机的联合验收、现场运动等原有未验项目仍保留，不能由 3D 演示或软件测试替代。实验性自动语音打断继续关闭。
