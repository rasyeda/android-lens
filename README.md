# android-lens

A lightweight, open-source Android debugging companion designed for developers using agentic AI coding tools (Claude Code, Antigravity) instead of Android Studio.

- **Pure-Rust JDWP engine** (zero JVM overhead for the debugger itself)
- **Built for low-RAM machines**
- **Phase 0 (current):** Interactive CLI proof-of-concept for ADB + JDWP step-through debugging

## Quick Start (Phase 0 CLI PoC)

```bash
# List connected devices/emulators
cargo run -p cli-poc -- devices

# List installed third-party packages
cargo run -p cli-poc -- packages

# Attach to a running debuggable Kotlin/Java Android app
cargo run -p cli-poc -- attach --package com.example.sholattv
```

### Interactive Commands Once Attached
- `classes [filter]` — search loaded classes
- `b <File.kt:line>` — set a line breakpoint
- `threads` — list VM threads
- `bt` — print call stack with source file & line numbers
- `locals` — print local variables for the paused frame
- `n` / `s` / `o` — step over / into / out
- `c` / `pause` — resume / suspend VM
- `q` — resume & quit
