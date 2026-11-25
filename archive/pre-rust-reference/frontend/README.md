# AI Tutor 工作台界面

使用 React 19、TypeScript 5.8 和 Vite 6。所有版本与解析依赖均在 `package.json` 和 `pnpm-lock.yaml` 中锁定。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\frontend\build.ps1
```

该脚本优先使用现有 Node.js，并在缺少系统包管理器时使用本机 Codex 已捆绑的 pnpm。它不安装全局工具。构建结果为 `frontend/dist`，由本机 FastAPI 服务提供。

界面依赖后端的真实接口，不提供演示数据后端。摄像头通过 `/api/video/frame` 获取同时包含 JPEG、框坐标和 frame_id 的单帧响应；请求按顺序轮询，不堆积旧帧。图像镜像与框坐标在同一显示层转换，原始坐标保留在后端。浏览器不会另行打开摄像头或麦克风。

状态每秒轮询 `/api/state`。生成回复状态为 `generated` 时，界面渲染后提交 `shown` 确认，成功后才请求可选的本地语音播报。后端仍需验证播放请求的学习者、会话和 generation，避免过期语音播放。

语音按钮支持按住鼠标、触摸或空格。按下时停止旧播报与生成；松开后请求本地转写，结果进入可修改的输入框，不会自动提交。录音上限为 20 秒，后端必须再次执行时长限制。

匿名档案与视觉目标独立。模拟档案使用 `namespace: simulator`，仅允许 `simulated` 模块事件。真实档案只显示人工或教师报告来源，界面不会宣称摄像头识别了积木。

对话和记忆均用 React 文本节点显示，不解释 HTML。没有远程字体、分析脚本或远程图像依赖。

音频输入与自动播报带有操作代次校验，切档案或停止后，过期转写不再写入输入框。接口显示确认完成前不会自动播报。前端纯逻辑测试可通过 `node --test frontend/src/audioEpoch.test.ts` 执行。

已验证：TypeScript 检查、生产构建、操作代次测试以及当前真实服务的只读接口联调。真实设备、自动播报及多浏览器操作仍由整体应用验收覆盖。
