# 已核对的主要来源

核对日期：2026-09-05。第三方库是依赖，不将其全部实现冒称为本仓库新编写代码。下列 source revisions 是适配器源码版本，不是模型权重 revision。

| 来源 | 固定版本 / 用途 |
|---|---|
| https://github.com/mii-nipah/voxcpm-rs | 006ae18fba7d6712e6212c8dcb4d05d723917f6e；VoxCPM2 Rust/Burn；库声明 Apache-2.0 |
| https://github.com/lumosimmo/qwen3-asr-rs | 7984967e8701cc90bb17d06453901036e36ca578；Qwen3 ASR/forced-aligner Rust/Candle；workspace 声明 MIT OR Apache-2.0 |
| https://github.com/OpenBMB/VoxCPM | 官方 VoxCPM 系列项目背景 |
| https://huggingface.co/openbmb/VoxCPM2 | 模型权重发布来源；使用前另核权重许可证与 revision |
| https://github.com/OpenMOSS/MOSS-TTS | MOSS-TTS 目标模型系列；未作为已完成的 Rust adapter |
| https://github.com/OpenMOSS/MOSS-Transcribe-Diarize | MOSS 转写/说话人分离目标；格式解析不等于模型实现 |
| https://modelcontextprotocol.io/specification/2025-06-18/basic/transports | 固定版本 stdio framing、日志与传输边界 |
| https://modelcontextprotocol.io/specification/2025-06-18/server/tools | 工具 schema、tools/list/tools/call、错误返回 |

实际查阅的 VoxCPM 文件：Cargo.toml、src/lib.rs、src/voxcpm2/wrapper.rs、src/weights.rs。核对了公共类型、GenerateOptions/CancelToken、PCM reference、load_pretrained receipt 及官方 `.pth` 布局兼容性。工作台改用严格 receipt 检查，拒绝缺参数/缺 AudioVAE 的容错随机权重路径。

实际查阅的 Qwen 文件：qwen3_asr/Cargo.toml、src/lib.rs、src/inference/types.rs、src/audio/input.rs、src/model/weights.rs、src/forced_aligner/mod.rs、src/forced_aligner/model.rs。核对了 AudioInput::Waveform、TranscribeOptions、LoadOptions、align 参数和浮点秒时间返回。没有从不明 API 猜测调用签名。

已知上游限制：VoxCPM 原项目 bf16 Vulkan 有依赖 patch 说明，本工程没有移植或宣称验证该分支，只提供 f32 CPU/WGPU。Qwen optional Metal 仅目标 macOS；没有声称本仓库已经提供 AMD 上的 Qwen GPU 推理。Burn/Candle 与驱动仍需实机构建验收。

来源核对和源码调用签名核对并不能替代编译、模型数值验证或音质验收。没有复制模型权重或参考音频到本仓库。
