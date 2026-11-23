# TASKS.md

Tracking remaining work across phases for **android-lens**.

---

## Phase 0 — Pure-Rust JDWP Proof of Concept
- [x] Scaffold Rust workspace (`adb-bridge`, `jdwp-client`, `cli-poc`)
- [x] Implement ADB device/package lookup and `JdwpForwardGuard`
- [x] Implement async JDWP wire protocol (handshake, dynamic `IDSizes`, packet demux, event stream)
- [x] Implement `.kt` / `.java` source-line breakpoint resolution, stepping (`over`/`into`/`out`), stack frames, and local variable formatting
- [x] Build interactive `cli-poc` REPL
- [ ] **Live test against Kotlin app (`sholattv`) on TV emulator:**
  - [ ] Verify `cli-poc devices`, `packages`, and `attach` handshake + `IDSizes`
  - [ ] Verify setting a breakpoint in a `.kt` file, hitting it, inspecting `bt` and `locals`, stepping (`n`/`s`/`o`), and resuming (`c`)
- [ ] **Pending breakpoints (`ClassPrepare`):** Automatically install breakpoints when a target class is loaded *after* attach
- [ ] **Launch & Wait for Debugger (`am start -D`):** Add `--launch <pkg/activity>` flag to `cli-poc attach` for debugging app startup
- [ ] **Object field expansion:** Add `inspect <0xobject_id>` command using `ReferenceType.FieldsWithGeneric` + `ObjectReference.GetValues` (including superclass fields)
- [ ] **Unit tests for JDWP wire codec:** Test `Writer`/`Reader` round-trips, 4-byte vs 8-byte `IdSizes`, and `Event.Composite` parsing

---

## Phase 1 — Tauri v2 Desktop MVP (<150 MB RAM)
- [ ] Initialize `apps/desktop` with Tauri v2 + lightweight TypeScript frontend
- [ ] Add read-only **CodeMirror 6** source viewer with project folder picker, clickable breakpoint gutter, and active execution line highlight
- [ ] Build Debug toolbar (Attach / Launch `-D`, Pause, Resume, Step Over/Into/Out)
- [ ] Build Threads, Call Stack, and lazy-expanding Variables tree panels
- [ ] Create `crates/logcat-stream`: package-scoped `adb logcat --pid` with auto-reattach on app restart, level/tag filtering, and crash stack grouping

---

## Phase 2 — Agentic AI Bridge (MCP Server)
- [ ] Create `crates/mcp-server` sharing the live `DebugSession` with the Tauri UI
- [ ] Expose core MCP tools for Claude Code & Antigravity:
  - [ ] `debug_attach` / `debug_set_breakpoint` / `debug_step`
  - [ ] `debug_get_state` (paused thread, call stack, source snippet, local variables)
  - [ ] `debug_inspect_object(object_id, depth)`
  - [ ] `logcat_tail(package, level, limit)`
  - [ ] `get_pinned_context()`
- [ ] Add **"Pin to Agent"** button in the UI on stack frames, variables, and logcat entries

---

## Phase 3 — Visual & App State Inspection
- [ ] One-click `scrcpy` mirror launcher from the device toolbar
- [ ] Create `crates/app-inspector`:
  - [ ] `uiautomator` XML hierarchy dump + visual overlay over device screenshot
  - [ ] `adb shell run-as <pkg>` browser for SQLite databases and SharedPreferences/DataStore
- [ ] (v2+) Kotlin inline-function SMAP mapping & Compose semantics inspection
