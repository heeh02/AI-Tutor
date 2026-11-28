# Rust WebAssembly 工作台

所有界面、网络请求、按键语音、投递确认、记忆操作和过期结果保护均位于 `rust/tutor-ui/src`，使用Rust/Yew并编译为WebAssembly。

运行资产：`frontend/dist`。HTML/CSS及wasm-bindgen自动生成的浏览器桥接文件由本机Rust服务同源提供。没有React、TypeScript、Node或npm运行依赖。

构建：`powershell -NoProfile -ExecutionPolicy Bypass -File frontend/build.ps1`。详情与官方wasm-bindgen工具来源见`rust/tutor-ui/README.md`。

`rust/tutor-ui/src/home.rs` 的 `RobotHome` 只呈现机器人、开始对话与活动入口，不展示活动卡片、聊天历史或大段标题。`navigation.rs` 提供 `#/home`、`#/voice`、`#/activities`、`#/conversation`、`#/records` 五个独立页面及浏览器前进／后退支持。`/#/voice` 为机器人常驻的语音陪伴页，支持按住说话、转写确认、完成后本地朗读和可折叠文字辅助，不把文字聊天框作为默认界面。活动页位于 `companion_activity.rs`，先展开详情再进入对话；选择活动只填充可编辑引导语，不自动发送、不覆盖草稿，也不新增后端接口。进入语音页后保持机器人位于主视觉区，焦点留在说话按钮；引导语默认收起，可由孩子或成人主动展开修改。页面切换保留输入草稿并回到顶部，显式进入文字输入时保留输入框聚焦；录音期间阻止直接切换，非对话页不确认新回复的展示或自动播报。设备开启时保留可见的控制入口。Omni 就绪且摄像头运行时可选择“看看这个”，每次点击只提交当前一张画面并播放内存中的语音结果；停止、切换或新的语音操作会取消对应请求，不改动文字草稿，也不把画面或模型观察写入学习记录。异步自动播放被浏览器拦截时保留同一音频并显示“听回答”，无需重新拍照或重新推理；播放结束才释放音频。

视觉层以 `frontend/style.css` 为基础，并由 `home.css`、`home-environment.css`、`activity-gallery.css`、`parent-center.css` 与最后加载的 `layout.css` 分层组织：机器人直接处于分层光影环境中，玻璃主要用于顶部导航和底部操作区；活动详情、对话与记录分别使用清晰的页面布局。配色参考[港科大官方品牌指南](https://brand.hkust.edu.hk/sites/default/files/bm_folder/HKUST_Brand_guideline.pdf)中的 `#003366`、`#996600`、`#CC9900`，以深蓝区分品牌与对话、金色区分当前任务、灰蓝区分设备、灰绿区分学习记录；不使用校徽图形，也不表示学校官方产品认证。成人管理使用独立设置区，模型与设备技术信息按需展开。摄像头入口在关闭时收起，开启时自动展开，不自动启动设备。移动端不显示重复的儿童底部导航，首页、活动和家长中心提供明确入口，使用低合成成本表面，并为 `prefers-reduced-motion` 关闭动画。伙伴动作体验明确标注为本地演示；轻点机器人或按 Enter 可触发本地挥手，陪伴表情和手臂姿态可反映真实录音、思考与播报状态，目光可跟随指针。呼吸与表情层独立于硬件对齐的头部姿态，不会伪造摄像头反馈；启用减少动态效果后停止自动动作与挥手。颜色、阴影和过渡通过 CSS 变量统一，不改变后端接口、设备安全边界或学习数据。

旧版前端仅作为历史参考保存于`archive/pre-rust-reference/frontend`，不会被构建或提供给浏览器。
