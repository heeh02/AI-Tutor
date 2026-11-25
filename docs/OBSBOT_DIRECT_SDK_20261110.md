# OBSBOT Tiny 3 直连控制交接

日期：2026-11-10。结论：**已找到厂商直连设备库的可靠线索；姿态显示已有独立于 Center/OSC 的原生读回路径；运动控制尚未切换到直连 SDK。**

## 已确认的接口与证据

已解包的 `runtime/vendor/center/app/bin/libdev.dll` 产品名为 **OBSBOT Device**，REMO TECH 签名有效，含 613 个具名 C++ 导出。其中有姿态读取、云台位置与停止相关方法；`OBSBOT_Main.exe` 确实导入该库。其设备 I/O、UVC、HID、`IKsControl` 线索支持它属于设备控制层，但仅凭静态导出不能证明脱离 Center 后的完整初始化、兼容性或停止行为。

机器人现在使用的 `camera.pose` 来自现有视频采集 owner 的 Windows `IAMCameraControl::Get`，无需 OSC 中转，也不需要额外打开摄像头。它提供带时间与采集周期标识的 **控制器报告位置**，不冒充独立编码器测量；过期或未知时不使用历史命令补角度。该路径已经与运动启用分离。

公开仓库存在旧版 `libdev_v2.1.0_7` 头文件，部分方法参数类型与本地 DLL 导出一致，但该头文件没有 Tiny 3 产品定义，不能认定与本机新版 Windows DLL 的完整 ABI 匹配。另有 Tiny 3 Linux 项目的实际调用代码，依赖另行提供的 `libdev_v2.1.0_8`，也不能替代 Windows 接口确认。特别是旧头文件中的 **`cameraSetPanTiltAbsolute` 用于 Meet 系列数字画面定位，范围为 -1 到 +1；它不是已证实的 Tiny 3 云台指令。**

后续静态核对已补齐核心接线表：设备管理器、按序列号选择、Windows 宽字符设备路径、姿态、有限位置、速度及停止均可定位。DLL 是 PE32+ x64，linker 14.44；PDB GUID `{2CA512B4-4286-3796-EAC9-E73CB000B3C2}` / age 1 可用于向厂商匹配构建。新版 `gimbalGetAllInfo` / `gimbalGetInfoR` 所需的 `AiGimbalStatus` / `GimbalInfo` 定义仍缺；旧读取接口也跨 DLL 传递 C++ 回调，不能直接简化成 Rust 函数指针调用。详见 [静态 ABI 对照表](../runtime/goal-baselines/20261110/robot-demo-next/sdk-discovery/STATIC_ABI_AUDIT.md)。

## 替换运动控制的下一步

1. 从 [OBSBOT 官方 SDK 入口](https://www.obsbot.com/sdk)取得**支持 Windows x64、Tiny 3、USB 直连且无需 Center 进程**的匹配开发包。需要头文件、运行库、导入库、版本对应说明、生命周期与线程规则、坐标/单位、回调与超时约定，以及使用和分发条款。官网申请需要账户；本轮没有登录或提交申请。
2. 使用匹配的头文件与工具链实现一个小型 **C ABI shim**：在 C++ 内部管理 `Device`、STL 对象和回调，对 Rust 只暴露不透明句柄、固定宽度数值、明确错误码与显式关闭接口。不要依据导出名称猜结构体布局或把 C++ 对象直接跨语言传递。
3. 先完成只读适配：精确绑定已保存设备，读取身份与姿态，核对坐标、单位、来源与新鲜度；验证超时、断开、释放和采集共存。显示端继续消费同一姿态契约，后台刷新不触发动作。
4. 只读验证完成且恢复实体测试授权后，再安排受限动作验收。复用现有显式启用、有限目标、失踪取消与故障锁定机制，先核对方向、未命令轴稳定和到位，再核验停止；成功返回值不能代替物理验证。

原生 `IAMCameraControl::Set` 接口在现有 Windows 绑定中已经可定位，并不依赖厂商 SDK；但历史原生测试出现未命令的 Tilt 变化，结论仍为 FAIL。新增直连路径必须独立验收，不继承后来 OSC 路径的有限通过结论。[Microsoft Set 接口说明](https://learn.microsoft.com/en-us/windows/win32/api/strmif/nf-strmif-iamcameracontrol-set)只定义属性写入及 HRESULT 返回，不保证物理到位或急停。

## 留存材料

- [签名、摘要、导入导出与设备层发现报告](../runtime/goal-baselines/20261110/robot-demo-next/sdk-discovery/SDK_DISCOVERY.md)
- [公开接口比对、源码位置与原生 Set 接点](../runtime/goal-baselines/20261110/robot-demo-next/sdk-discovery/PUBLIC_INTERFACE_FOLLOWUP.md)
- [公开接口比对的结构化证据](../runtime/goal-baselines/20261110/robot-demo-next/sdk-discovery/public-interface-result.json)
- [核心接口、构建身份及确切缺件](../runtime/goal-baselines/20261110/robot-demo-next/sdk-discovery/STATIC_ABI_AUDIT.md)
- [历史原生 PTZ 调查与失败记录](PTZ_REPORT.md)、[后来 OSC 有限跟随验证及暂缓安排](PTZ_TRACKING_REPAIR.md)

本次调查仅做静态检查和公开源码阅读：未加载或运行厂商组件，未操作设备、驱动或固件，未启用或移动云台。
