# 本机架构与接口

本文件记录前期Python原型，已归档其生产代码；当前交付全面使用Rust，权威架构为`RUST_ARCHITECTURE.md`，当前验收为根目录`ACCEPTANCE_REPORT.md`。下文不代表当前运行语言或语音引擎。

FastAPI(127.0.0.1)服务同源React工作台。只有后端拥有摄像头和麦克风。摄像头采集为可终止子进程，最新帧队列上限1；CPU推理与追踪线程发布同一frame_id的JPEG和原图归一化脸框。显示镜像由前端同时映射框，原始坐标不变。

教学由明确文字提交触发，每轮独立Kimi进程。应用提供模块知识、学习者设置、有限近期已投递对话、当前活动和最多5条相关慢记忆。Kimi只返回严格JSON。模型没有shell/文件/MCP/设备工具，输出字段不能包含执行命令。

每轮绑定learner_id/session_id/request_id/generation与删除epoch。新输入、换档案、录音、任务证据更新和停止会取消旧生成/播报。候选状态/记忆要等浏览器确认shown才应用；未显示的提示不算已投递。

SQLite保存匿名档案、会话检查点、事件、投递状态、慢记忆、删除tombstone与请求元数据。真实与模拟分别保存到runtime/data/real.sqlite3和simulator.sqlite3。记忆引用必须存在、同档案、来源不提升；会话偏好不自动成为长期偏好。中文采用相邻双字匹配加范围筛选，不依赖英文分词。

纠正/删除会增加档案epoch，取消旧任务，清除相关来源及派生记忆、该档案暂态对话与快状态；避免旧上下文重新引入删除内容。未受影响的记忆保留原始来源。CLI请求结束即清除本应用隔离home内的会话日志；原有Kimi历史与凭证不删除。SQLite secure_delete和WAL checkpoint用于清理应用数据库旧页。

录音按住开始、松开转写；20秒硬缓冲，模型本地加载，空白/低置信度不自动提交。转写需用户编辑确认后发送。播报仅使用已校验且当前有效的显示回复；新输入先终止旧音频，半双工避免自问自答。

本地页面采用随机HttpOnly/SameSite严格cookie，启动URL只用于bootstrap后立即跳转去除token。Host/Origin/Sec-Fetch-Site检查同样覆盖视频与API，WebSocket也校验cookie和Origin。无公共部署、无任意静态目录，Prompt不会进入前端包。

## 未来硬件接入

当前事件：`POST /api/activities/{activity_id}/events`，字段modules、source_type、report；硬件验证固定false。真实硬件adapter需要小组提供device_epoch、timestamp、module_id/type/connection、传感器数据及独立安全验证结果。不得仅将source_type改名就当作硬件接入。当前API不接受real_hardware事件，不提供执行电机入口。

情绪模型接口本轮关闭；未来需真实模型、可靠输出及来源协议，不能从脸框生成替代情绪数值。
