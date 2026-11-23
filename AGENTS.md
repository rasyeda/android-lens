# AGENTS.md

Instructions and engineering guidelines for AI coding agents (Claude Code, Antigravity, Codex, etc.) contributing to **android-lens**.

## Project Vision
**android-lens** is a lightweight (<150 MB RAM), open-source Android debugging companion built for developers who use agentic AI coding tools instead of Android Studio.
- **No heavy IDE indexing, PSI/AST, or embedded JVM.**
- **Dual-interface by design:** The same Rust core drives both the Tauri v2 desktop UI (for humans) and a local MCP server (for AI agents).

## Repository Layout
```text
android-lens/
├── Cargo.toml                 # Rust workspace root
├── crates/
│   ├── adb-bridge/            # ADB device discovery, pidof, port forwarding, am start -D
│   ├── jdwp-client/           # Pure-Rust async JDWP wire protocol & high-level debugger API
│   └── cli-poc/               # Phase 0 interactive CLI debugger
├── README.md
├── TASKS.md                   # Active backlog & phased roadmap
└── AGENTS.md
```

Planned crates/apps (see `TASKS.md`):
- `crates/logcat-stream/` — Package-scoped logcat ring buffer & crash detector
- `crates/app-inspector/` — `uiautomator` XML hierarchy & `run-as` DB/Prefs reader
- `crates/mcp-server/` — Local MCP server exposing live debug state to AI agents
- `apps/desktop/` — Tauri v2 + lightweight Web UI (CodeMirror 6 read-only viewer)

## Architecture & Engineering Rules
1. **Stay lightweight:** Do not introduce a JVM dependency for the debugger core. Keep dependencies minimal and compile times fast.
2. **Protocol correctness (`jdwp-client`):**
   - Always use dynamic `IdSizes` returned by `VirtualMachine.IDSizes` (Command Set `1`, Command `7`) rather than hardcoding 4 or 8 bytes.
   - All JDWP integers/IDs are **big-endian**.
   - Incoming VM events arrive as Command Set `64`, Command `100` (`Event.Composite`) and are demultiplexed in the background reader task.
3. **Clean resource cleanup (`adb-bridge`):**
   - Always clean up `adb forward` rules via RAII (`JdwpForwardGuard`) so stale ports are not left behind.
4. **Target separation:**
   - Kotlin/Java Android apps use **ART / JDWP** (supported).
   - Flutter/Dart apps use the **Dart VM Service protocol** (out of scope for v1 JDWP; do not assume JDWP breakpoints work on Dart code).
5. **Kotlin v1 pragmatism:**
   - Debug APKs emit `.kt` file names and line tables in DEX debug info—standard line breakpoints and stepping work directly.
   - Defer arbitrary Kotlin expression evaluation and cross-file inline-function SMAP mapping until v2.

## Common Commands
```bash
# Build all workspace crates
cargo build

# Run the Phase 0 CLI PoC
cargo run -p cli-poc -- devices
cargo run -p cli-poc -- packages --filter <name>
cargo run -p cli-poc -- attach --package <pkg>

# Check formatting & lints
cargo fmt --check
cargo clippy --workspace --all-targets
```

## Workflow
- Keep `TASKS.md` updated as tasks are completed or newly discovered.
- Commit small, focused changes with conventional commit messages (`feat:`, `fix:`, `docs:`, `refactor:`).
