# android-lens

A lightweight (<150 MB RAM), open-source Android debugging companion designed for developers who use agentic AI coding tools (**Claude Code**, **Antigravity**, etc.) instead of Android Studio.

## Why android-lens?

When an AI coding agent handles code edits, refactoring, and `./gradlew` builds, you no longer need Android Studio's heaviest components (IntelliJ indexing, PSI/AST, 8–14 GB RAM footprint). What you still need is a fast, visual **runtime debugging companion**—and a way for both you and your AI agent to inspect the **same live debug session**.

- **Pure-Rust JDWP engine:** Step-through debugging (breakpoints, step over/into/out, call stack, local variables) over `adb` with zero JVM overhead for the debugger itself.
- **Dual-interface (Human + AI Agent):** Planned built-in **MCP server** so Claude Code / Antigravity can set breakpoints, read call stacks, inspect variables, and receive context you pin from the UI.
- **Low-RAM desktop UI:** Planned **Tauri v2** + read-only CodeMirror 6 source viewer (~100–150 MB total RAM).

---

## Architecture

```text
[Tauri v2 UI]  <──IPC──>  [Rust Core (tokio)]  <──MCP──>  [Claude Code / Antigravity]
                                │
                 ┌──────────────┼──────────────┐
                 ▼              ▼              ▼
           adb-bridge      jdwp-client    logcat / inspector
                 └──────────────┴──────────────┘
                                │ adb forward tcp:PORT jdwp:PID
                                ▼
                    [Android Device / Emulator (ART)]
```

### Workspace Crates
| Crate | Status | Responsibility |
| :--- | :--- | :--- |
| `crates/adb-bridge` | Implemented (Phase 0) | Device discovery, package listing, `pidof`, `am start -D`, auto-cleaning `adb forward` |
| `crates/jdwp-client` | Implemented (Phase 0) | Pure-Rust async JDWP wire protocol, dynamic `IDSizes`, line resolution, breakpoints, stepping, locals |
| `crates/cli-poc` | Implemented (Phase 0) | Interactive CLI debugger REPL for verifying JDWP against live devices/emulators |
| `crates/logcat-stream` | Planned (Phase 1) | Package-scoped logcat ring buffer & crash grouping |
| `crates/mcp-server` | Planned (Phase 2) | Local MCP server exposing the live debug session to AI agents |
| `crates/app-inspector` | Planned (Phase 3) | `uiautomator` layout inspector & `run-as` SQLite/SharedPrefs browser |
| `apps/desktop` | Planned (Phase 1) | Tauri v2 desktop app with CodeMirror 6 read-only viewer |

---

## Quick Start (Phase 0 CLI PoC)

### Prerequisites
- Rust toolchain (`rustc` / `cargo`)
- Android SDK Platform-Tools (`adb` in your `PATH`)
- A connected Android device or emulator running a **debuggable Kotlin/Java app** (`debuggable = true`)

### Build & Run
```bash
# Build the workspace
cargo build

# 1. List connected devices / emulators
cargo run -p cli-poc -- devices

# 2. Find your app's package name
cargo run -p cli-poc -- packages --filter sholat

# 3. Attach via pure-Rust JDWP
cargo run -p cli-poc -- attach --package <your.package.name>
```

### Interactive Debugger Commands
Once attached, the `jdwp>` prompt supports:
- `classes [filter]` — search loaded classes (defaults to your app package)
- `b <File.kt:line>` — resolve and set a line breakpoint
- `threads` — list VM threads and names
- `bt` — print the call stack of the paused thread (class, method, source file, line number)
- `locals` — inspect local variables in the top stack frame (auto-resolves Strings, arrays, and object types)
- `n` / `over` — step over
- `s` / `into` — step into
- `o` / `out` — step out
- `c` / `resume` — resume VM
- `pause` — suspend VM
- `q` / `quit` — resume VM and detach cleanly

---

## Roadmap

See [TASKS.md](./TASKS.md) for the full phased checklist, and [AGENTS.md](./AGENTS.md) for contributor/agent guidelines.

## License

MIT
