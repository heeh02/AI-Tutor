# AI Tutor Codex 实施交付包

这是产品与工程实施规格，不是已实现的应用。用户电脑上的摄像头和 Kimi 尚未由本次回答远程测试。

## 使用
把本目录放在准备开发的项目工作区内，让本地 Codex 读取 CODEX_EXECUTION_PROMPT.md 并执行。
已有仓库时先读已有代码与规则，不替换已有私有实现。无需把密钥或整个 Kimi 配置发到聊天中。

## 文件
- IMPLEMENTATION_SPEC.md：完整目的、范围、用户旅程、架构、组件、接口、阶段和交付要求。
- CODEX_EXECUTION_PROMPT.md：可直接交给 Codex 的开发任务。
- ACCEPTANCE.md：分项测试与证据要求。
- SOURCES_AND_DECISIONS.md：项目材料、最新要求和外部文档的关系。
- configs/default.example.yaml：待实现应用的建议配置，不是 Kimi CLI 配置。
- schemas/tutor_reply.schema.json：待实现业务输出协议。
- examples/*.json / *.yaml：明确标注的模拟输入与验收场景。
- prompts/tutor_core.example.md：可调整的初始教学指令，不声称是团队原有私有 prompt。
