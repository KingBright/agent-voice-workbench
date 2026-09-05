# Agent Voice Workbench / Rust 声音工作台

面向 Agent 的本地声音作业系统：Rust CLI、MCP stdio、本地 REST、持久化任务、内容寻址音频资源、离线 DSP，以及原生 Rust 神经模型适配器。

**交付状态：pre-alpha 源码实现，未完成编译及真机验收。** 当前交付环境没有 `cargo`、`rustc`、`rustfmt`，且无法解析 Rust 工具链下载域名。不能将下面的构建配置、测试代码或适配器代码当作“已经跑通”的证明。详见 [验证记录](docs/VALIDATION.md)。

**原分享页只成功取得标题，未取得完整正文。** 本实现按已知的声音工作台方向与 Rust 核心约束推进；不能声称逐条覆盖未读取到的原规划。需求恢复边界见 [来源与范围](docs/SOURCE_PLAN.md)。

## 能力边界

| 模块 | 本次交付 | 尚未证明 / 未实现 |
|---|---|---|
| 作业与资源 | 持久化 FIFO、幂等键、取消、截止时间、崩溃恢复、SHA-256 资源、分页事件 | 代码已写；Rust 测试尚未执行；没有多进程分布式调度 |
| 音频处理 | WAV 导入、显式单声道下混、裁剪、窗化 sinc 重采样、峰值归一化、淡入淡出、片段叠混 | 没有 LUFS、时间拉伸、立体声工程、实时录音 |
| 文本与字幕 | 结构化转写资源、SRT/VTT、MOSS 格式文本解析器 | 解析器不是 MOSS 神经推理；不凭文本猜时间或说话人 |
| VoxCPM2 | 调用固定提交的 `voxcpm-rs`；CPU/WGPU；参考声音；严格权重检查；协作取消 | 未编译、未加载权重、未试听、未测 RTF |
| Qwen3-ASR | 调用固定提交的 `qwen3_asr`；CPU/Metal；20 秒分块转写 | 无说话人分离；块边界不是词时间；无边界重叠修复；未实测 |
| Qwen3-ForcedAligner | 调用原生 Rust 对齐 API；输出真实预测时间而非均分时间 | 单次最长 30 秒；未实测；短词无效时间返回错误 |
| MOSS-TTS v1.5 | 保留明确模型 ID，返回 Unsupported | **原生推理图尚未实现**；不使用其他模型冒充 |
| MOSS-Transcribe-Diarize | 保留明确模型 ID，返回 Unsupported | **ASR+diarization 原生推理图尚未实现** |

本仓库的业务逻辑、模型集成与服务端代码都是 Rust，没有 Python worker、Node 服务或以 shell 调用外语模型服务的实现。VoxCPM 使用 Burn，Qwen 使用 Candle：它们是两个进程内 Rust 库，不是两套语言服务。**这不表示整个传递依赖树、操作系统驱动或 GPU SDK 都由 Rust 编写。**

## 构建与首次验证

需要较新的 stable Rust、平台 C/C++ 构建工具及所选 GPU 驱动。可选模型依赖可能需要额外原生构建依赖；Qwen 强制对齐的日语词典也会增加构建成本。首次构建依赖网络，权重另行准备。

```sh
# 首次交付没有伪造 Cargo.lock；首次构建后审核并提交它。
cargo generate-lockfile
cargo fmt --all
cargo xtask check

# 轻量版：任务、DSP、字幕、CLI、REST、MCP；不包含神经推理。
cargo build -p avw --release

# CPU 模型版：包含 VoxCPM2、Qwen3-ASR、Qwen3-ForcedAligner 适配器。
cargo build -p avw --release --features native

# Windows/Linux：VoxCPM2 可选 WGPU；Qwen 在本实现中使用 CPU。
cargo build -p avw --release --features native,wgpu

# macOS：VoxCPM2 可选 WGPU/Metal 路径；Qwen 直接使用 Candle Metal。
cargo build -p avw --release --features native,wgpu,metal

cargo xtask check --native
```

下文 `avw` 指 `target/release/avw`，Windows 为 `target/release/avw.exe`；也可自行加入 PATH。不是建议安装一个未经本仓库构建的同名程序。

```sh
avw doctor
avw --workspace .avw capabilities
avw schema
```

`--device cpu` 是默认值。`--device auto` 是**按编译特性显式路由**：VoxCPM 有 WGPU 则选 WGPU；Qwen 在 macOS Metal 构建上选 Metal，否则 CPU。它不是运行时硬件探测；GPU 初始化失败不会偷偷回退，也不会伪报成功。显式 `--device wgpu`/`metal` 不适合同时请求另一个不支持该设备的模型。

## 不需要模型的第一条链路

```sh
# 生成的是正弦波测试素材，绝不是 TTS。
cargo run -p avw-core --example fixture -- fixture.wav
avw --workspace .avw import fixture.wav
```

导入返回真实 `id`。把 `examples/prepare.json` 中 64 个 `0` 替换成该 ID，然后：

```sh
avw --workspace .avw run --request examples/prepare.json
# 将结果 artifacts[0].id 作为输出资源 ID；导出不会覆盖已有文件。
avw --workspace .avw export OUTPUT_ASSET_ID --output prepared.wav
```

`service::tests::real_dsp_roundtrip_through_durable_worker` 已编写完整的自动化路径：真实 PCM → 资源 → 持久化队列 → 实际 DSP → 输出校验。**测试尚未在本交付环境执行。**

## 准备模型：本地目录，不运行 Python 转换脚本

从可信模型发布方取得完整 checkpoint，保留它的真实 40 位提交 ID。应用不提供自动下载器；可使用浏览器下载原始权重。`--revision` 是**模型权重版本**，不是下方 Rust 适配器源码的提交号。它由操作者提供并记录；本地 SHA-256 证明文件与注册时相同，不证明远端来源真实性。

VoxCPM2 目录需有 `config.json`、`tokenizer.json`，主权重 `model.safetensors` / `model.pth` / `model.pt` 三选一，AudioVAE 权重 `audiovae.safetensors` / `audiovae.pth` 二选一。`.pth` 由上游 Rust 库加载；不要求 Python。缺 AudioVAE 或未加载参数会失败，禁止以随机权重兜底。

Qwen 目录需有 `config.json`、`tokenizer.json` 和 `model.safetensors`，或者完整 `model.safetensors.index.json` 及其全部分片。使用完整、独立的实际文件目录；不接受 Hugging Face cache 中指向其他目录的权重符号链接。注册本身不证明模型类型或所有张量正确，加载时仍可能失败。

```sh
avw --workspace .avw models add --id voxcpm2 \
  --directory /absolute/checkpoints/VoxCPM2 --revision CHECKPOINT_COMMIT_SHA
avw --workspace .avw models add --id qwen3_asr \
  --directory /absolute/checkpoints/Qwen3-ASR --revision CHECKPOINT_COMMIT_SHA
avw --workspace .avw models add --id qwen3_forced_aligner \
  --directory /absolute/checkpoints/Qwen3-ForcedAligner --revision CHECKPOINT_COMMIT_SHA
avw --workspace .avw models verify --id voxcpm2
avw --workspace .avw models list
```

注册不可覆盖。换模型版本使用另一个 workspace；不让正在运行的模型悄悄改版本。冷加载前会重新哈希权重；大权重的校验与加载耗时计入任务截止时间，但校验/加载本身不是可硬中断操作。

```sh
avw --workspace .avw --device cpu run --request examples/tts-voxcpm2.json
```

参考声音通过 `reference` 传已导入音频 ID，同时提供 `reference_rights`，记录你对该素材有权使用的说明。这是留痕字段，不是自动完成法律授权校验。没有种子参数，不承诺位级可重现。只在取得授权的声音上使用参考声音功能。

## Agent 接入

### MCP stdio

`examples/mcp-config.json` 是通用配置示例，必须填写本机绝对路径。客户端启动：

```sh
avw --workspace /absolute/workspace --device auto mcp
```

实现固定 MCP `2025-06-18` 的 stdio 工具子集：initialize、ping、tools/list、tools/call、JSON-RPC 错误及通知不回复。stdout 只输出协议，诊断走 stderr。请求与行长度上限 1 MiB。**没有实现 MCP Streamable HTTP；下面是独立 REST。**

工具包括 `capabilities`、`audio_import`、`assets_get`、`jobs_submit`、`jobs_get`、`jobs_cancel`、`jobs_list`、`jobs_events`、`transcript_read`。作业返回资源引用，默认不塞 base64 音频；查询结果不重复整个输入提示。`audio_import` 只能读取 `workspace/inbox` 下相对路径。完整 schema 由 Rust 类型生成。

### 本地 REST 与 CLI 客户端

```sh
avw --workspace .avw --device auto serve --listen 127.0.0.1:8765
# 在另一个终端调用同一个持久化进程，不再争抢 workspace 锁。
avw --server http://127.0.0.1:8765 import input.wav
avw --server http://127.0.0.1:8765 call capabilities
avw --server http://127.0.0.1:8765 call jobs_get --arguments '{"job_id":"JOB_ID"}'
```

REST 只绑定 loopback；拒绝浏览器 Origin 和不可信 Host。可通过 `AVW_TOKEN` 为服务端/客户端设置相同的 32 字节以上随机 bearer token，不要把 token 写入命令参数或仓库。REST 不提供 TLS、远程用户系统或多租户隔离。远程访问使用已认证的安全隧道。

路由及完整 JSON 请求见 [API 文档](docs/API.md)。任务轮询使用事件 `next_after` 游标，转写阅读使用 `next_offset`；不要每轮重发全历史。

## 目录与持久化

```text
crates/avw-core/   类型、资源、DSP、转写、持久化日志、任务工作线程
crates/avw/        CLI、REST、MCP、权重注册、原生模型适配器
xtask/            Rust 开发/发布辅助命令，不参与模型推理
examples/         JSON 请求、字幕素材、MCP 配置
.avw/             本地数据，默认不进入 Git
  inbox/          Agent 允许导入的素材目录
  objects/        SHA-256 命名的资源
  metadata/       资源元数据
  models/         本地模型 manifest（含本机路径）
  jobs.avwj       checksummed append-only 任务日志
  workbench.lock  单工作进程锁
```

当前日志有容量上限，不提供自动压缩/清理；声音与文本是明文。取消后已有未被作业引用的资源可能保留，不做危险的自动删除。安全与故障边界见 [架构](docs/ARCHITECTURE.md) 和 [安全说明](docs/SECURITY.md)。

## GitHub 发布

**本次只创建了本地 Git 历史；没有宣称远端仓库已经存在。** 当前连接可写现有仓库，但没有创建仓库的操作；没有擅自改动其他仓库。随交付提供 source ZIP 和保留完整提交的 Git bundle。

在有 GitHub CLI 且已登录的机器上，从 bundle 恢复后：

```sh
git clone --origin bundle agent-voice-workbench.bundle agent-voice-workbench
cd agent-voice-workbench
# 推荐先编译、测试，提交 rustfmt 修改和生成的 Cargo.lock，再发布。
gh repo create KingBright/agent-voice-workbench --private --source=. --remote=origin --push
```

也可用 `cargo xtask publish --repo KingBright/agent-voice-workbench`。该命令拒绝脏工作区和已有 origin，只创建新私有仓库；不会推送到已存在的同名仓库，不会 force push。发布结果由实际命令返回确认，不能靠 README 中的名字确认。

## 后续实现的具体缺口

先修完首次编译/测试结果，再做模型黄金样本验收与平台实测。MOSS 两个模型的移植拆解、与原方案的区别及验收标准见 [实施缺口](docs/ROADMAP.md)。这里不是“四个指定模型全部完成”的成品声明。

自有代码 MIT；第三方 Rust 库及模型权重各自许可证独立，见 [第三方来源](docs/SOURCES.md)。
