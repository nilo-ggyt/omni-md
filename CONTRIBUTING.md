# Contributing

Read [AGENTS.md](AGENTS.md) before changing implementation. Use pinned Rust 1.99.0, edition 2024, the committed lockfile and macOS 14 deployment target. Run the commands in [README.md](README.md); native probes must execute on the macOS process main thread without diagnostic skip flags.

Keep core algorithms independent of AppKit and the host. The plugin depends on the public SDK, its own core and declared libraries. Do not import host private code, another plugin's implementation, a duplicate SDK identity, dynamic-loading infrastructure or a second document model.

Document every logical section and branch in accurate English. Explain intent, ownership, bounds, rejection and recovery. Public/private modules, types, functions, callbacks and error exits need local responsibility/contract explanations; a summary over a long function is insufficient. Every unsafe block/implementation needs an adjacent specific `SAFETY` explanation, and unsafe APIs document caller obligations. Generated and upstream third-party code are outside blanket annotation changes.

For changes to text/storage, test scalar-aligned UTF-16 boundaries, emoji and combining marks, giant lines, history and stale viewport invalidation. For filesystem changes, test exclusive creation, collisions, traversal/symlink confinement, stale revisions and byte preservation. Native changes additionally require real input/IME/accessibility and weak graph/mapping release evidence. Record exact commands and limitations; never infer runtime success from source inspection.

Do not commit credentials, local overrides, personal documents, clipboard contents, build output or session traces. Preserve MIT attribution and third-party notices. SDK and host changes require coordinated immutable revision updates and a clean host assembly check proving one SDK Cargo identity.
