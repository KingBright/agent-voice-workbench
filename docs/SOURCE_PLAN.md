# 来源、恢复边界与范围

用户要求基于分享对话完善规划、实现代码、创建新 GitHub 仓库并提交：
https://chatgpt.com/share/6a9bad6e-2258-83e8-8fbf-8c443aa74a06?ogimg=plain

本次访问只取得标题“调研本地TTS ASR方案”，没有取得正文。另行上下文检索没有可靠恢复逐条规划。因此不将本文当作原分享对话逐字记录，也不声称全部验收条款已覆盖。

采用的工作方向：面向 Agent 的本地声音工作台、跨平台目标、Rust 唯一核心语言；TTS/ASR/forced alignment/后处理通过共同音频资源组合，而非把多个独立模型合成一个模型。待确认的原始細节包括既定项目名、原规划接口、完整硬件规格、性能与音质阈值、商业授权条件及 GUI 范围。

作为当前工程模型集处理：VoxCPM2、MOSS-TTS v1.5、MOSS-Transcribe-Diarize、Qwen3-ForcedAligner。为形成可移植的 Rust ASR 适配器，新增 Qwen3-ASR。**新增 Qwen3-ASR 不代表交付了 MOSS 的 ASR+diarization。**

交付内调整：用内容寻址资源和小任务作为组合单位；只做进程内 Rust 推理，不引入 Python sidecar；使用固定上游 Rust 提交而不是从接口名称臆造模型实现；未完成模型返回 Unsupported；准确区分 chunk timing 与 forced alignment；GUI、实时录音和 LUFS 不在本次代码实现中。

远端操作边界：实际读取连接账号确认 KingBright；工具支持提交文件/树/commit，但未提供新建仓库函数，插件检索也未补齐此能力。容器没有 gh/已有 GitHub 认证。没有向其他项目写入任何内容；仅创建本地 Git 仓库并打包交付。
