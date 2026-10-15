# Omni MD

Omni MD is the statically linked Rust/AppKit Markdown plugin for [Omni Bar](https://github.com/nilo-ggyt/omni-bar). Its source is maintained independently from the host and [Plugin SDK](https://github.com/nilo-ggyt/omni-plugin-sdk). It is bundled into the host application and DMG; it does not install or launch a separate plugin process.

The workspace contains:

| Crate | Responsibility |
| --- | --- |
| `omni-md-plugin` | Static SDK registration, foreground session, AppKit workspace controls and piece-backed TextKit editor. |
| `omni-markdown-core` | Confined vault operations, atomic revision-checked saves, bounded search, canonical piece-tree document and viewport Markdown presentation. |

SDK package 2.0.0 / protocol 2 must match the host exactly; the manifest pins canonical SDK commit
[`3e17eaf4bf7def814c3e3bdd08155f6ca330be3b`](https://github.com/nilo-ggyt/omni-plugin-sdk/commit/3e17eaf4bf7def814c3e3bdd08155f6ca330be3b). `REGISTRATION` exposes immutable descriptor data; only explicit activation opens a workspace or allocates native objects. The host must integrate the SDK from the same immutable Git revision used here so Cargo resolves one SDK package identity.

## Build and checks

Install Xcode command-line tools on macOS and Rust through rustup. The committed toolchain selects Rust 1.99.0, edition 2024. The deployment target remains macOS 14. Use the committed lockfile:

```sh
export MACOSX_DEPLOYMENT_TARGET=14.0
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --workspace --no-deps --locked
cargo test -p omni-md-plugin --test native_textkit --features native-markdown-runtime-probe --locked
```

The native test has `harness = false`: AppKit runs on the real process main thread and the actual `NSApplication` event loop. It mounts, exercises and closes the editor in separate native event transactions. Public deallocation markers retain no observed graph object; after the final actual native release, one verification event consumes the full weak-only evidence. A single probe-only 2 s failure deadline terminates a missing signal; it never retries or accepts a surviving graph. Production uses no release timer. The test contains no skip, survivor allowance, sleep or polling acceptance mode. Run it on an unlocked interactive desktop; a locked display is not visible-user acceptance evidence. Portable core tests and native runtime evidence cover different contracts. See [validation status](docs/validation.md) for the distinction between implemented behavior and executed acceptance.

## Host integration

Depend on `omni-md-plugin` with its canonical repository and an immutable full `rev`, then add `omni_md_plugin::REGISTRATION` to the host's static `PluginCatalog`. Discovery, query matching and activation remain generic SDK operations. Never branch in host business logic on this plugin's ID or activation code.

The host supplies the workspace root, application-data root and main-thread lifecycle callbacks. Mounting copies a borrowed native-view descriptor; it does not transfer or extend ownership. The close protocol saves first, preserves the editor on failure, observes the native close callback, unmounts borrowed views, releases the document/TextKit graph, and consumes one destruction barrier before creating a successor surface.

Ordinary `ForegroundTransition` requests still require full native graph deallocation before any
successor. Explicit `ApplicationTermination` requests require actual complete project-payload
release and a typed proof of an empty, closed/detached panel that AppKit may still lease in its
termination caller pool. That terminal receipt is not window deallocation and cannot reopen a
surface. Scope promotion requires Host authorization, preserves original intent, and never repeats
save/close; see [terminal ownership](docs/architecture.md#application-termination-scope).

## Native workspace

The sidebar submits title/body search with Return. Each query owns one cancellable foreground worker; replacing a query, refreshing or closing cancels and joins it. Tree navigation owns bounded metadata pages rather than an entire vault catalog. Click a directory to collapse or expand its subtree, use **More notes** to advance, and **Refresh** or the root row to start again. Pagination keeps later notes reachable while bounding each filesystem scan and native control page.

The **Document** menu provides explicit save, Markdown and attachment import through the native file chooser, confirmed disk reload, exclusive save of a conflicted draft as a new note, document counts, find/replace, and nineteen Markdown formatting commands. File paste/drop provisionally imports assets into the confined `attachments` tree and commits each asset only after checked canonical link insertion succeeds. Source presentation includes literal syntax highlighting, line numbers and fold controls. Source/preview presentation and the reusable table-cell editor share the same canonical document. Physical Escape first leaves table editing or find UI; native marked-text cancellation remains owned by AppKit. Exact Command-W and explicit close requests follow save-before-close.

Accepted edits reset a single foreground 850 ms autosave deadline. Save preserves structural undo/redo and selection; a conflict keeps the live draft and external file and stops automatic retry until a new edit or explicit save. The detailed module boundaries, native ownership graph and handoff protocol are described in [architecture](docs/architecture.md).

## Data and ownership

The vault owns Markdown notes and a reserved `attachments` tree. Paths are validated relative UTF-8 paths; traversal, symlink components, nonregular targets and overwriting destination operations are rejected. Saving streams piece-tree bytes into a same-directory temporary file, rechecks the disk revision, and atomically replaces the original only when the expected revision still matches. A conflict preserves both external disk changes and the live draft; explicit reload requires confirmation, while saving the draft as a new note uses exclusive creation.

The piece tree is the only canonical mutable text model. AppKit receives a virtual UTF-16 string and bounded viewport attributes. The editor retains no complete `String`, `NSString` or attributed-string mirror. Copy-all may allocate the one transient system pasteboard value required by the explicit action. Main-thread native objects, document mappings, undo roots, menu targets, scan workers and presentation caches end with the foreground session.

## Validation status

Formal acceptance is scoped to Apple M5 Max, 36 GiB, arm64, macOS 27.0.1. macOS 14 and M1/8 GiB runtime validation remain untested until actually performed. Successful compilation or core tests do not establish VoiceOver/IME behavior, native object release, full-product performance parity or the host baseline-retirement gate. Recorded observations belong in maintained validation reports and must distinguish executed, failed and unexecuted checks.

Project-owned code is licensed under [MIT](LICENSE), copyright nilo vance. Third-party dependencies retain their own licenses; see [third-party notices](docs/third-party-notices.md).

Optional actual-input instrumentation is isolated behind the nondefault `native-input-trace` feature.
Its explicit arm/drain API stores bounded scalar callback/canonical evidence without output or
observers; it does not certify semantic paint or real IME performance by itself. See
[input trace contracts and limits](docs/input-trace.md).
