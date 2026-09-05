# 实施缺口与验收顺序

本文件是未完成工作的拆解，不是“已经全部实现”的清单。

## 1. 先消除交付验证债务

在可联网的 Rust 环境生成并固定 Cargo.lock，运行 rustfmt、clippy 和全部 Rust tests；修编译错误和测试失败。矩阵检查 CPU、Windows/Linux WGPU、macOS WGPU+Metal。将首次格式化 diff 和 lockfile 审核提交后，把 CI bootstrap 格式化改成 fmt --check 与 --locked。

从可信发布方准备每个模型的完整固定版本权重。验证 CPU 加载、短中文/英文合成、合法参考声音、ASR、单句对齐；将许可允许的测试 PCM、期望输出和容差存为 golden fixtures。不要用纯正弦波证明 TTS/ASR 正确。

测量 RTF（计算秒数/生成音频秒数）、首个可播放片段延迟（当前非流式适配器不能给此指标）、峰值进程内存、峰值 GPU 内存、空闲内存、冷/热加载、取消延迟、连续 100 次请求后的错误率。至少报告真实机器/OS/驱动/编译 feature/权重 revision，不套用上游作者的机器数据。

## 2. MOSS-TTS v1.5 原生 Rust 图

保留 ModelId::MossTts15，不能替成 Nano、VoxCPM 或系统 TTS。先锁定官方 v1.5 配置、tokenizer、codec/声学解码器与权重布局；列出可从 Candle/Burn 复用的模块和缺失算子。按嵌入→attention/RoPE/cache→音频 token 自回归→codec decode 逐层写 Rust 数值测试，比较合法取得的参考张量，而不是只听最终波形。

然后实现文本/参考音频预处理、采样/stop 条件、声道/采样率、取消点、数值 dtype/内存预算。没有充分验证的组合不加入 capabilities 的已编译支持。这里尚没有这些神经图源码；已有接口和 ID 不计作模型移植完成。

## 3. MOSS-Transcribe-Diarize 原生 Rust 图

锁定官方模型、音频编码器、语言模型和结构化输出语法。完成 log-mel/音频编码、跨模态桥接、自回归生成与长音频策略，分别验收文本错误率、词/段时间误差和说话人分离错误率。结构化解析器目前只能处理已存在文本，不能替代任何这些神经组件。

需要语义明确的 timestamp/speaker/event schema，再实现分块说话人 identity 关联、跨块重叠修复与时间偏移。在未完成身份关联前，禁止把每块 S01 当作全片同一人。

## 4. 生产化闭环

先给 Qwen 分块 ASR 加边界重叠/对齐修复，给长素材对齐加入真实分段与原时间轴偏移；随后实现 PCM 流式背压、Rust worker 子进程隔离、硬取消、GPU OOM 恢复。再考虑工程文件、音轨/立体声、LUFS、低损伤时间伸缩与监听 UI，而非一次生成大量空模块。

GC/日志压缩必须保留幂等键约定并拥有 crash recovery 测试；资源对象不可因某个取消任务被误删。模型更新用版本化 manifest 与显式切换，不篡改已注册权重。整个过程保持 Rust 核心和真实功能验证，不引入异构运行时去掩盖模型移植缺口。
