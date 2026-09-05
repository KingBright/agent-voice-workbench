# Working agreement

The executable and all application/model-integration logic stay in Rust. Do not add Python workers, Node services, shell-based inference, whisper.cpp or ONNX Runtime and call them native Rust implementations of the selected models. Native platform/driver dependencies of Rust crates must be disclosed.

Do not claim an adapter is operational merely because it is registered or feature-enabled. Distinguish source-written, compiled, model-loaded, golden-output-validated, and hardware-benchmarked. MOSS-TTS v1.5 and MOSS-Transcribe-Diarize have no native adapters in this commit; return Unsupported rather than a mock, another model, or invented timestamps. Do not relabel MOSS-TTS-Nano as MOSS-TTS v1.5.

Run `cargo fmt --all`, `cargo xtask check`, then the appropriate native-feature checks. The bootstrap delivery was created without a compiler: generate Cargo.lock, review and commit the first formatting diff and lockfile. After bootstrap, switch CI from formatting to `cargo fmt --all -- --check` and enforce a committed lockfile. Never fabricate verification results.

Keep model weights, voices, recordings, `.avw`, credentials and private manifests out of Git. Use explicit user authorization for repository creation/publication. `xtask publish` creates only a new private repository and refuses an existing origin. No force pushes.

Use artifact IDs, compact job summaries and event cursors. Heavy results belong in assets, never repeated tool payloads. Enforce byte/sample/queue limits. All model loads remain outside the async HTTP reactor. One workspace has one owning worker process; use REST rather than opening a competing process.

Cancellation requested before completion wins. A killed process must not automatically regenerate its interrupted synthesis. Preserve the distinction between input-chunk boundaries and neural alignment. No guessed speaker IDs. Never silently pad/truncate generated speech to match a subtitle duration.

An API change needs typed validation, negative tests, protocol examples and an updated capability matrix. Keep new abstractions tied to a real execution path. Do not add empty crates for future modules.
