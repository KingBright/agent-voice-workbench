# 交付验证记录

日期：2026-09-05。状态：**源码已远端提交 / 本地未编译的 pre-alpha**。

## 远端发布复核

用户完成应用安装授权后，完整源码于 2026-09-05 成功提交到 `KingBright/agent-voice-workbench` 的 `main`，源码导入提交为 `84f0b0e8335b09af23bd6ac38e7d5c5309d5085f`。已回读远端分支确认提交生效。48 个文件对应的 Git tree 为 `bdfc093862395cc14068f6eff77845efb7d3bd32`，与已逐文件比对 ZIP 的原 bundle 源码树完全相同。后续文档状态更新单独提交，不改变这份源码快照。

已确认 GitHub Actions `Rust verification` 因本次 push 启动，运行编号 `33956694903`。启动时状态为 `in_progress`，不据此宣称编译或测试通过；最终状态以该次 Actions 记录为准。下表中的本地验证限制仍然成立。

## 已经实际完成的检查

- 读取并解析 Cargo workspace、crate manifests、Rust toolchain 与 Cargo config TOML。
- 解析 8 个示例 JSON 与 4 行 MCP NDJSON；核对示例任务键名和已编写的 Rust 字段。
- 对 19 个 Rust 文件执行字符串/注释感知的括号配对检查与 `mod xxx;` 文件路径检查。这只是词法结构检查，**不是 Rust parser/type checker**。
- 核对 workspace 路径依赖、两个 Git 模型库的完整固定 rev；检查不存在 `todo!` / `unimplemented!` 实现占位宏。
- 检查交付中没有 Python/JavaScript/TypeScript 运行时代码、模型权重、真实音频、常见 GitHub/OpenAI token 格式。模式扫描不是完整的泄漏检测审计。
- 人工复核资源读接口，修正调用处把 `(AssetMeta, bytes)` 误当作 bytes 的问题；核对上游推理类型；增加 VoxCPM 严格参数加载校验与日志长度反码校验。

机器可读结果：`validation-static.json`。这份静态结果不声称实现了编译器，也不取代下面尚未运行的测试。

## 实际尝试但没有执行成功

`cargo test --workspace` 的命令退出码 **127**，原因：`cargo: command not found`。没有生成测试 pass 数量，也没有伪造 Cargo.lock。

环境搜索未发现 cargo/rustc/rustfmt。尝试访问 Rust 工具链发行站点返回 `Could not resolve host: static.rust-lang.org`，因此没有在此环境安装工具链。Git 存在，可以进行本地 commit、bundle 和恢复检查。

## 本地验证与未验收项

| 验证项 | 本次状态 |
|---|---|
| cargo check / 类型与依赖解析 | 未执行 |
| rustfmt | 未执行；CI bootstrap 配置会先格式化并保存 diff |
| cargo clippy | 未执行 |
| 已编写的 61 个 Rust 测试 | **未运行** |
| 默认版 CLI / REST / MCP 端到端运行 | 未运行 |
| VoxCPM2 CPU/WGPU 加载、合成、参考声音 | 未运行 |
| Qwen3-ASR / ForcedAligner CPU/Metal | 未运行 |
| 真正的模型输出与黄金样本比较 | 未运行 |
| macOS / Windows / Linux 矩阵 | GitHub CI 已启动；未据启动状态判定通过，模型实机验证未完成 |
| 速度、音质、内存、取消延迟和稳定性 | 未测量 |
| MOSS-TTS v1.5 / MOSS-Transcribe-Diarize | 神经推理适配器未实现；明确 Unsupported |
| GitHub 仓库创建与提交 | 用户创建仓库；本次源码提交、main 更新及回读核验已成功 |

## 已编写的测试覆盖面

包含资源 ID / 参数验证、WAV round-trip、下混、静音统计、采样时钟、重采样抗混叠、混音、淡入淡出、取消、幂等键、日志恢复与损坏、终态竞态、工作线程、字幕时间与标记处理、路径逃逸、MCP framing/握手、HTTP Origin/token，以及走真实 DSP Executor 的资源→任务→输出测试。

测试里的 TestExecutor 只在 `cfg(test)` 下编译，不能作为产品推理 backend。正弦波 example 只验证音频处理，不能当作 ASR/TTS 验收结果。

## 首次接手应运行

```sh
cargo generate-lockfile
cargo fmt --all
cargo xtask check
cargo check -p avw --features native
# macOS 上额外执行：
cargo check -p avw --features native,wgpu,metal
```

审核并提交生成的 Cargo.lock 和格式化修改。修正实际编译、测试及平台问题后再将对应状态提升为通过；有真实权重结果后才能称模型跑通。CI 已启动，但尚未在本文记录任何构建通过或 artifact 验收结论。

Git bundle 的可恢复性与源码压缩包完整性在打包阶段单独用 Git/ZIP 工具核验；Git 验证通过也不表示 Rust 或模型通过。
