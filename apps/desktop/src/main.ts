import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { BreakpointMark, SourceViewer } from "./editor";

interface AdbDevice {
  serial: string;
  state: string;
  model: string | null;
  device: string | null;
}

interface LocationDto {
  class_name: string;
  method_name: string;
  source_file: string | null;
  line_number: number | null;
}

interface SessionStatus {
  state: "disconnected" | "running" | "paused";
  serial: string | null;
  package: string | null;
  pid: number | null;
  vm_info: string | null;
  paused_thread_id: string | null;
  paused_thread_name: string | null;
  paused_location: LocationDto | null;
}

interface BreakpointDto {
  id: string;
  file: string;
  line: number;
  status: "active" | "pending";
  class_name: string | null;
  method_name: string | null;
  request_ids: number[];
}

interface ThreadDto {
  id_hex: string;
  name: string;
  is_paused: boolean;
}

interface StackFrameDto {
  index: number;
  frame_id_hex: string;
  class_name: string;
  method_name: string;
  source_file: string | null;
  line_number: number | null;
}

interface VariableDto {
  name: string;
  type_name: string;
  value: string;
  object_id_hex: string | null;
  is_expandable: boolean;
  is_static: boolean;
  declaring_class: string | null;
}

interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

interface SourceFileContent {
  path: string;
  file_name: string;
  content: string;
  language: string;
}

type LogLevel = "verbose" | "debug" | "info" | "warn" | "error" | "fatal" | "silent" | "unknown";

interface LogEntry {
  seq: number;
  timestamp: string;
  pid: number;
  tid: number;
  level: LogLevel;
  tag: string;
  message: string;
}

interface CrashGroup {
  id: number;
  timestamp: string;
  pid: number;
  process: string | null;
  summary: string;
  lines: string[];
}

type DebugUiEvent =
  | { kind: "status_changed"; status: SessionStatus }
  | { kind: "breakpoints_changed"; breakpoints: BreakpointDto[] }
  | {
      kind: "paused";
      reason: string;
      thread_id_hex: string;
      thread_name: string;
      location: LocationDto;
    }
  | { kind: "resumed" }
  | { kind: "vm_exited" }
  | { kind: "message"; text: string };

type LogcatEvent =
  | { kind: "entry"; entry: LogEntry }
  | { kind: "crash"; crash: CrashGroup }
  | { kind: "attached"; pid: number }
  | { kind: "waiting_for_process"; package: string }
  | { kind: "error"; message: string };

const LEVEL_RANK: Record<LogLevel, number> = {
  verbose: 0,
  debug: 1,
  info: 2,
  warn: 3,
  error: 4,
  fatal: 5,
  silent: 6,
  unknown: 7,
};

const MAX_RENDERED_LOGS = 1500;

// DOM refs
const deviceSelect = document.getElementById("device-select") as HTMLSelectElement;
const refreshDevicesBtn = document.getElementById("refresh-devices-btn") as HTMLButtonElement;
const packageInput = document.getElementById("package-input") as HTMLInputElement;
const packageDatalist = document.getElementById("package-datalist") as HTMLDataListElement;
const attachBtn = document.getElementById("attach-btn") as HTMLButtonElement;
const launchBtn = document.getElementById("launch-btn") as HTMLButtonElement;
const detachBtn = document.getElementById("detach-btn") as HTMLButtonElement;
const pauseBtn = document.getElementById("pause-btn") as HTMLButtonElement;
const resumeBtn = document.getElementById("resume-btn") as HTMLButtonElement;
const stepOverBtn = document.getElementById("step-over-btn") as HTMLButtonElement;
const stepIntoBtn = document.getElementById("step-into-btn") as HTMLButtonElement;
const stepOutBtn = document.getElementById("step-out-btn") as HTMLButtonElement;
const statusBadge = document.getElementById("status-badge") as HTMLSpanElement;
const banner = document.getElementById("banner") as HTMLDivElement;

const openFolderBtn = document.getElementById("open-folder-btn") as HTMLButtonElement;
const projectPathBar = document.getElementById("project-path-bar") as HTMLDivElement;
const treeFilterInput = document.getElementById("tree-filter-input") as HTMLInputElement;
const fileTreeEl = document.getElementById("file-tree") as HTMLDivElement;

const editorTabsEl = document.getElementById("editor-tabs") as HTMLDivElement;
const editorContainer = document.getElementById("editor-container") as HTMLDivElement;
const editorEmpty = document.getElementById("editor-empty") as HTMLDivElement;

const threadsListEl = document.getElementById("threads-list") as HTMLDivElement;
const threadsCountEl = document.getElementById("threads-count") as HTMLSpanElement;
const stackListEl = document.getElementById("stack-list") as HTMLDivElement;
const stackCountEl = document.getElementById("stack-count") as HTMLSpanElement;
const varsTreeEl = document.getElementById("vars-tree") as HTMLDivElement;
const varsFrameLabelEl = document.getElementById("vars-frame-label") as HTMLSpanElement;

const logcatPidPill = document.getElementById("logcat-pid-pill") as HTMLSpanElement;
const crashesCountEl = document.getElementById("crashes-count") as HTMLSpanElement;
const bpsCountEl = document.getElementById("bps-count") as HTMLSpanElement;
const logcatLevelSelect = document.getElementById("logcat-level") as HTMLSelectElement;
const logcatTagInput = document.getElementById("logcat-tag") as HTMLInputElement;
const logcatQueryInput = document.getElementById("logcat-query") as HTMLInputElement;
const logcatAutoScroll = document.getElementById("logcat-autoscroll") as HTMLInputElement;
const logcatToggleBtn = document.getElementById("logcat-toggle-btn") as HTMLButtonElement;
const logcatClearBtn = document.getElementById("logcat-clear-btn") as HTMLButtonElement;
const logcatListEl = document.getElementById("logcat-list") as HTMLDivElement;
const crashesListEl = document.getElementById("crashes-list") as HTMLDivElement;
const breakpointsListEl = document.getElementById("breakpoints-list") as HTMLDivElement;

// App state
let projectRoot: string | null = localStorage.getItem("android-lens:projectRoot");
const expandedDirs = new Set<string>();
const dirCache = new Map<string, FsEntry[]>();

const openFiles = new Map<string, SourceFileContent>();
let activeFilePath: string | null = null;

let currentStatus: SessionStatus = {
  state: "disconnected",
  serial: null,
  package: null,
  pid: null,
  vm_info: null,
  paused_thread_id: null,
  paused_thread_name: null,
  paused_location: null,
};

let breakpoints: BreakpointDto[] = [];
let selectedThreadId: string | null = null;
let selectedFrameIndex = 0;
let execHighlight: { fileName: string; line: number } | null = null;

let logcatRunning = false;
let logEntries: LogEntry[] = [];
let crashGroups: CrashGroup[] = [];

const viewer = new SourceViewer(editorContainer, async (fileName, line) => {
  try {
    const updated = await invoke<BreakpointDto[]>("debug_toggle_breakpoint", {
      file: fileName,
      line,
    });
    updateBreakpoints(updated);
  } catch (err) {
    showBanner(`Breakpoint error: ${String(err)}`);
  }
});

function showBanner(text: string, isInfo = false) {
  banner.textContent = text;
  banner.className = isInfo ? "banner info" : "banner";
  const close = document.createElement("button");
  close.className = "btn btn-xs";
  close.textContent = "Dismiss";
  close.onclick = () => banner.classList.add("hidden");
  banner.appendChild(close);
}

function hideBanner() {
  banner.classList.add("hidden");
}

// --- ADB Devices & Packages ---

async function refreshDevices() {
  try {
    const devices = await invoke<AdbDevice[]>("adb_list_devices");
    const prev = deviceSelect.value;
    deviceSelect.innerHTML = "";
    if (devices.length === 0) {
      const opt = document.createElement("option");
      opt.value = "";
      opt.textContent = "No ADB devices";
      deviceSelect.appendChild(opt);
      return;
    }
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d.serial;
      opt.textContent = d.model ? `${d.serial} (${d.model})` : `${d.serial} [${d.state}]`;
      deviceSelect.appendChild(opt);
    }
    if (prev && devices.some((d) => d.serial === prev)) {
      deviceSelect.value = prev;
    }
    await refreshPackages();
  } catch (err) {
    showBanner(`ADB error: ${String(err)}`);
  }
}

async function refreshPackages() {
  const serial = deviceSelect.value || null;
  if (!serial) return;
  try {
    const pkgs = await invoke<string[]>("adb_list_packages", { serial, filter: null });
    packageDatalist.innerHTML = "";
    for (const p of pkgs) {
      const opt = document.createElement("option");
      opt.value = p;
      packageDatalist.appendChild(opt);
    }
    const savedPkg = localStorage.getItem("android-lens:package");
    if (!packageInput.value && savedPkg) {
      packageInput.value = savedPkg;
    } else if (!packageInput.value && pkgs.length > 0) {
      packageInput.value = pkgs[0];
    }
  } catch {
    // Ignore transient package lookup failure
  }
}

// --- Project Folder & File Tree ---

async function openProjectFolder(path: string) {
  projectRoot = path;
  localStorage.setItem("android-lens:projectRoot", path);
  projectPathBar.textContent = path;
  projectPathBar.title = path;
  expandedDirs.clear();
  dirCache.clear();
  expandedDirs.add(path);
  await loadDir(path);
  renderFileTree();
}

async function loadDir(dirPath: string): Promise<FsEntry[]> {
  const cached = dirCache.get(dirPath);
  if (cached) return cached;
  const entries = await invoke<FsEntry[]>("fs_list_dir", { path: dirPath });
  dirCache.set(dirPath, entries);
  return entries;
}

function renderFileTree() {
  fileTreeEl.innerHTML = "";
  if (!projectRoot) {
    fileTreeEl.innerHTML = `<div class="empty-hint">Click "Open Folder" to browse source files</div>`;
    return;
  }
  const filter = treeFilterInput.value.trim().toLowerCase();

  const renderLevel = (dirPath: string, depth: number) => {
    const entries = dirCache.get(dirPath) ?? [];
    for (const entry of entries) {
      if (filter && !entry.is_dir && !entry.name.toLowerCase().includes(filter)) {
        continue;
      }
      const row = document.createElement("div");
      row.className = "tree-row" + (entry.path === activeFilePath ? " active" : "");
      row.style.paddingLeft = `${8 + depth * 14}px`;

      const caret = document.createElement("span");
      caret.className = "tree-caret";
      if (entry.is_dir) {
        caret.textContent = expandedDirs.has(entry.path) ? "▾" : "▸";
      }

      const icon = document.createElement("span");
      icon.className = "tree-icon";
      if (entry.is_dir) {
        icon.classList.add("dir");
        icon.textContent = "▣";
      } else if (entry.name.endsWith(".kt") || entry.name.endsWith(".kts")) {
        icon.classList.add("kt");
        icon.textContent = "K";
      } else if (entry.name.endsWith(".java")) {
        icon.classList.add("java");
        icon.textContent = "J";
      } else {
        icon.textContent = "·";
      }

      const label = document.createElement("span");
      label.textContent = entry.name;

      row.append(caret, icon, label);
      row.onclick = async () => {
        if (entry.is_dir) {
          if (expandedDirs.has(entry.path)) {
            expandedDirs.delete(entry.path);
          } else {
            expandedDirs.add(entry.path);
            await loadDir(entry.path);
          }
          renderFileTree();
        } else {
          await openSourceFile(entry.path);
        }
      };

      fileTreeEl.appendChild(row);
      if (entry.is_dir && expandedDirs.has(entry.path)) {
        renderLevel(entry.path, depth + 1);
      }
    }
  };

  renderLevel(projectRoot, 0);
}

// --- Source Viewer & Tabs ---

async function openSourceFile(path: string, focusLine?: number) {
  try {
    let doc = openFiles.get(path);
    if (!doc) {
      doc = await invoke<SourceFileContent>("fs_read_file", { path });
      openFiles.set(path, doc);
    }
    activeFilePath = path;
    editorEmpty.style.display = "none";
    renderEditorTabs();
    renderFileTree();

    const marks = marksForFile(doc.file_name);
    const execLine =
      execHighlight && execHighlight.fileName.toLowerCase() === doc.file_name.toLowerCase()
        ? execHighlight.line
        : null;

    viewer.openDocument(doc.file_name, doc.content, doc.language, marks, execLine);
    if (focusLine !== undefined) {
      viewer.scrollToLine(focusLine);
    }
  } catch (err) {
    showBanner(`Could not open file: ${String(err)}`);
  }
}

async function openBySourceName(fileName: string, line: number, classHint?: string) {
  // Check already-open tabs first
  for (const [path, doc] of openFiles.entries()) {
    if (doc.file_name.toLowerCase() === fileName.toLowerCase()) {
      if (activeFilePath === path) {
        viewer.setExecutionLine(
          execHighlight && execHighlight.fileName.toLowerCase() === fileName.toLowerCase()
            ? execHighlight.line
            : null,
          false,
        );
        viewer.scrollToLine(line);
      } else {
        await openSourceFile(path, line);
      }
      return;
    }
  }
  if (!projectRoot) return;
  const found = await invoke<string | null>("fs_find_source_file", {
    projectRoot,
    fileName,
    classHint: classHint ?? null,
  });
  if (found) {
    await openSourceFile(found, line);
  }
}

function renderEditorTabs() {
  editorTabsEl.innerHTML = "";
  for (const [path, doc] of openFiles.entries()) {
    const tab = document.createElement("div");
    tab.className = "editor-tab" + (path === activeFilePath ? " active" : "");
    tab.title = path;

    const nameSpan = document.createElement("span");
    nameSpan.textContent = doc.file_name;

    const closeBtn = document.createElement("button");
    closeBtn.className = "tab-close";
    closeBtn.textContent = "×";
    closeBtn.onclick = (e) => {
      e.stopPropagation();
      openFiles.delete(path);
      if (activeFilePath === path) {
        const next = openFiles.keys().next().value as string | undefined;
        if (next) {
          void openSourceFile(next);
        } else {
          activeFilePath = null;
          viewer.destroy();
          editorEmpty.style.display = "flex";
          renderEditorTabs();
          renderFileTree();
        }
      } else {
        renderEditorTabs();
      }
    };

    tab.append(nameSpan, closeBtn);
    tab.onclick = () => void openSourceFile(path);
    editorTabsEl.appendChild(tab);
  }
}

function marksForFile(fileName: string): BreakpointMark[] {
  return breakpoints
    .filter((bp) => bp.file.toLowerCase() === fileName.toLowerCase())
    .map((bp) => ({ line: bp.line, status: bp.status }));
}

function syncEditorMarkers() {
  if (!activeFilePath) return;
  const doc = openFiles.get(activeFilePath);
  if (!doc) return;
  viewer.setBreakpoints(marksForFile(doc.file_name));
  const execLine =
    execHighlight && execHighlight.fileName.toLowerCase() === doc.file_name.toLowerCase()
      ? execHighlight.line
      : null;
  viewer.setExecutionLine(execLine, false);
}

// --- Debugger State, Threads, Stack, Lazy Variables ---

function applyStatus(status: SessionStatus) {
  currentStatus = status;
  const connected = status.state !== "disconnected";
  const paused = status.state === "paused";

  attachBtn.disabled = connected;
  launchBtn.disabled = connected;
  detachBtn.disabled = !connected;
  pauseBtn.disabled = !connected || paused;
  resumeBtn.disabled = !paused;
  stepOverBtn.disabled = !paused;
  stepIntoBtn.disabled = !paused;
  stepOutBtn.disabled = !paused;

  statusBadge.className = `status-badge ${status.state}`;
  if (status.state === "disconnected") {
    statusBadge.textContent = "Disconnected";
    execHighlight = null;
    syncEditorMarkers();
    threadsListEl.innerHTML = `<div class="empty-hint">Attach to inspect threads</div>`;
    threadsCountEl.textContent = "0";
    stackListEl.innerHTML = `<div class="empty-hint">Pause or hit a breakpoint</div>`;
    stackCountEl.textContent = "0";
    varsTreeEl.innerHTML = `<div class="empty-hint">No paused frame</div>`;
    varsFrameLabelEl.textContent = "";
  } else if (status.state === "running") {
    statusBadge.textContent = `Running · PID ${status.pid ?? "?"}`;
    statusBadge.title = status.vm_info ?? "";
    execHighlight = null;
    syncEditorMarkers();
  } else {
    const loc = status.paused_location;
    const where = loc?.source_file
      ? `${loc.source_file}:${loc.line_number ?? "?"}`
      : loc?.method_name ?? status.paused_thread_name ?? "paused";
    statusBadge.textContent = `Paused · ${where}`;
    statusBadge.title = `${status.paused_thread_name ?? ""} — ${status.vm_info ?? ""}`;
  }
}

async function refreshThreads() {
  if (currentStatus.state === "disconnected") return;
  try {
    const threads = await invoke<ThreadDto[]>("debug_list_threads");
    threadsCountEl.textContent = String(threads.length);
    threadsListEl.innerHTML = "";
    for (const t of threads) {
      const item = document.createElement("div");
      item.className =
        "list-item" +
        (t.is_paused ? " paused-thread" : "") +
        (t.id_hex === selectedThreadId ? " active" : "");
      item.innerHTML = `<span class="frame-method">${escapeHtml(t.name)}</span><span class="frame-loc">${t.is_paused ? "paused" : t.id_hex}</span>`;
      item.onclick = async () => {
        selectedThreadId = t.id_hex;
        selectedFrameIndex = 0;
        await refreshThreads();
        if (currentStatus.state === "paused") {
          await refreshCallStack(t.id_hex);
        }
      };
      threadsListEl.appendChild(item);
    }
  } catch {
    // Ignore if VM resumed mid-query
  }
}

async function refreshCallStack(threadIdHex?: string) {
  try {
    const frames = await invoke<StackFrameDto[]>("debug_get_call_stack", {
      threadIdHex: threadIdHex ?? selectedThreadId,
    });
    stackCountEl.textContent = String(frames.length);
    stackListEl.innerHTML = "";
    if (frames.length === 0) {
      stackListEl.innerHTML = `<div class="empty-hint">No stack frames</div>`;
      return;
    }

    for (const f of frames) {
      const item = document.createElement("div");
      item.className = "list-item" + (f.index === selectedFrameIndex ? " active" : "");
      const shortCls = f.class_name.split(".").pop() ?? f.class_name;
      const locText = f.source_file
        ? `${f.source_file}:${f.line_number ?? "?"}`
        : "native";
      item.title = `${f.class_name}.${f.method_name} (${locText})`;
      item.innerHTML = `<span class="frame-method">#${f.index} ${escapeHtml(shortCls)}.${escapeHtml(f.method_name)}</span><span class="frame-loc">${escapeHtml(locText)}</span>`;
      item.onclick = async () => {
        selectedFrameIndex = f.index;
        await refreshCallStack(threadIdHex ?? selectedThreadId ?? undefined);
        await refreshLocals(threadIdHex ?? selectedThreadId ?? undefined, f.index, f);
        if (f.source_file && f.line_number) {
          await openBySourceName(f.source_file, f.line_number, f.class_name);
        }
      };
      stackListEl.appendChild(item);
    }

    const activeFrame = frames[selectedFrameIndex] ?? frames[0];
    await refreshLocals(threadIdHex ?? selectedThreadId ?? undefined, activeFrame.index, activeFrame);
  } catch (err) {
    stackListEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

async function refreshLocals(
  threadIdHex: string | undefined,
  frameIndex: number,
  frame?: StackFrameDto,
) {
  varsFrameLabelEl.textContent = frame
    ? `#${frameIndex} ${frame.method_name}`
    : `#${frameIndex}`;
  try {
    const vars = await invoke<VariableDto[]>("debug_get_locals", {
      threadIdHex: threadIdHex ?? null,
      frameIndex,
    });
    varsTreeEl.innerHTML = "";
    if (vars.length === 0) {
      varsTreeEl.innerHTML = `<div class="empty-hint">(no local variables in scope)</div>`;
      return;
    }
    for (const v of vars) {
      varsTreeEl.appendChild(createVarNode(v, 0));
    }
  } catch (err) {
    varsTreeEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

function createVarNode(v: VariableDto, depth: number): HTMLElement {
  const container = document.createElement("div");

  const row = document.createElement("div");
  row.className = "var-row" + (v.is_expandable ? " expandable" : "");
  row.style.paddingLeft = `${8 + depth * 14}px`;

  const caret = document.createElement("span");
  caret.className = "var-caret";
  caret.textContent = v.is_expandable ? "▸" : "";

  row.appendChild(caret);

  if (v.is_static) {
    const st = document.createElement("span");
    st.className = "var-static";
    st.textContent = "static";
    row.appendChild(st);
  }

  const nameEl = document.createElement("span");
  nameEl.className = "var-name";
  nameEl.textContent = v.name;
  row.appendChild(nameEl);

  if (v.type_name) {
    const shortType = v.type_name.split(".").pop() ?? v.type_name;
    const typeEl = document.createElement("span");
    typeEl.className = "var-type";
    typeEl.textContent = `: ${shortType}`;
    typeEl.title = v.type_name;
    row.appendChild(typeEl);
  }

  const eq = document.createElement("span");
  eq.className = "muted";
  eq.textContent = "=";
  row.appendChild(eq);

  const valEl = document.createElement("span");
  valEl.className = "var-val";
  valEl.textContent = v.value;
  row.appendChild(valEl);

  if (v.declaring_class) {
    const owner = document.createElement("span");
    owner.className = "var-owner";
    owner.textContent = `(${v.declaring_class.split(".").pop()})`;
    owner.title = `Inherited from ${v.declaring_class}`;
    row.appendChild(owner);
  }

  container.appendChild(row);

  if (v.is_expandable && v.object_id_hex) {
    const childrenWrap = document.createElement("div");
    childrenWrap.style.display = "none";
    container.appendChild(childrenWrap);

    let loaded = false;
    let expanded = false;
    const objId = v.object_id_hex;

    row.onclick = async () => {
      expanded = !expanded;
      caret.textContent = expanded ? "▾" : "▸";
      childrenWrap.style.display = expanded ? "block" : "none";

      if (expanded && !loaded) {
        loaded = true;
        childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${8 + (depth + 1) * 14}px">Loading…</div>`;
        try {
          const children = await invoke<VariableDto[]>("debug_inspect_object", {
            objectIdHex: objId,
          });
          childrenWrap.innerHTML = "";
          if (children.length === 0) {
            childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${8 + (depth + 1) * 14}px">(empty)</div>`;
          } else {
            for (const child of children) {
              childrenWrap.appendChild(createVarNode(child, depth + 1));
            }
          }
        } catch (err) {
          loaded = false;
          childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${8 + (depth + 1) * 14}px">Error: ${escapeHtml(String(err))}</div>`;
        }
      }
    };
  }

  return container;
}

// --- Breakpoints Panel ---

function updateBreakpoints(next: BreakpointDto[]) {
  breakpoints = next;
  bpsCountEl.textContent = String(breakpoints.length);
  syncEditorMarkers();

  breakpointsListEl.innerHTML = "";
  if (breakpoints.length === 0) {
    breakpointsListEl.innerHTML = `<div class="empty-hint">Click the source editor gutter to add a breakpoint</div>`;
    return;
  }

  for (const bp of breakpoints) {
    const row = document.createElement("div");
    row.className = "bp-row";

    const left = document.createElement("div");
    left.className = "bp-left";
    const dot = document.createElement("span");
    dot.className = `bp-dot ${bp.status}`;
    const desc = document.createElement("span");
    desc.textContent = `${bp.file}:${bp.line}`;
    const detail = document.createElement("span");
    detail.className = "muted";
    detail.textContent = bp.class_name
      ? `${bp.class_name}.${bp.method_name ?? ""}`
      : `[${bp.status}]`;

    left.append(dot, desc, detail);
    left.onclick = () => void openBySourceName(bp.file, bp.line, bp.class_name ?? undefined);

    const rm = document.createElement("button");
    rm.className = "btn btn-xs";
    rm.textContent = "Remove";
    rm.onclick = async (e) => {
      e.stopPropagation();
      const updated = await invoke<BreakpointDto[]>("debug_remove_breakpoint", { id: bp.id });
      updateBreakpoints(updated);
    };

    row.append(left, rm);
    breakpointsListEl.appendChild(row);
  }
}

// --- Logcat & Crashes ---

function matchesLogFilter(entry: LogEntry): boolean {
  const minLevel = logcatLevelSelect.value as LogLevel;
  if (LEVEL_RANK[entry.level] < LEVEL_RANK[minLevel]) return false;

  const tag = logcatTagInput.value.trim().toLowerCase();
  if (tag && !entry.tag.toLowerCase().includes(tag)) return false;

  const q = logcatQueryInput.value.trim().toLowerCase();
  if (
    q &&
    !entry.message.toLowerCase().includes(q) &&
    !entry.tag.toLowerCase().includes(q)
  ) {
    return false;
  }
  return true;
}

function createLogRow(entry: LogEntry): HTMLElement {
  const row = document.createElement("div");
  row.className = `log-row ${entry.level}`;
  const letter = entry.level[0]?.toUpperCase() ?? "?";
  row.innerHTML = `<span class="log-ts">${escapeHtml(entry.timestamp)}</span><span class="log-lvl ${entry.level}">${letter}</span><span class="log-tag" title="${escapeHtml(entry.tag)}">${escapeHtml(entry.tag)}</span><span>${escapeHtml(entry.message)}</span>`;
  return row;
}

function renderAllLogs() {
  logcatListEl.innerHTML = "";
  const filtered = logEntries.filter(matchesLogFilter);
  if (filtered.length === 0) {
    logcatListEl.innerHTML = `<div class="empty-hint">${logcatRunning ? "Waiting for matching logcat lines…" : "Logcat idle"}</div>`;
    return;
  }
  const slice = filtered.slice(-MAX_RENDERED_LOGS);
  const frag = document.createDocumentFragment();
  for (const entry of slice) {
    frag.appendChild(createLogRow(entry));
  }
  logcatListEl.appendChild(frag);
  if (logcatAutoScroll.checked) {
    logcatListEl.parentElement!.scrollTop = logcatListEl.parentElement!.scrollHeight;
  }
}

function appendLogEntry(entry: LogEntry) {
  logEntries.push(entry);
  if (logEntries.length > 5000) {
    logEntries.splice(0, logEntries.length - 5000);
  }
  if (!matchesLogFilter(entry)) return;

  if (logcatListEl.firstElementChild?.classList.contains("empty-hint")) {
    logcatListEl.innerHTML = "";
  }
  logcatListEl.appendChild(createLogRow(entry));
  while (logcatListEl.childElementCount > MAX_RENDERED_LOGS) {
    logcatListEl.firstElementChild?.remove();
  }
  if (logcatAutoScroll.checked) {
    logcatListEl.parentElement!.scrollTop = logcatListEl.parentElement!.scrollHeight;
  }
}

function renderCrashes() {
  crashesCountEl.textContent = String(crashGroups.length);
  crashesCountEl.className = crashGroups.length > 0 ? "count-pill alert" : "count-pill";
  crashesListEl.innerHTML = "";
  if (crashGroups.length === 0) {
    crashesListEl.innerHTML = `<div class="empty-hint">No crashes detected</div>`;
    return;
  }
  for (const crash of [...crashGroups].reverse()) {
    const card = document.createElement("div");
    card.className = "crash-card";
    const head = document.createElement("div");
    head.className = "crash-head";
    head.innerHTML = `<span>${escapeHtml(crash.summary)}</span><span class="muted">${escapeHtml(crash.timestamp)} · PID ${crash.pid}</span>`;

    const body = document.createElement("pre");
    body.className = "crash-stack";
    body.textContent = crash.lines.join("\n");

    head.onclick = () => {
      body.style.display = body.style.display === "none" ? "block" : "none";
    };
    card.append(head, body);
    crashesListEl.appendChild(card);
  }
}

async function startLogcat(pkg: string) {
  const serial = deviceSelect.value || null;
  if (!pkg) return;
  await invoke("logcat_start", { serial, package: pkg });
  logcatRunning = true;
  logcatToggleBtn.textContent = "Stop Logcat";
  logEntries = [];
  crashGroups = [];
  renderAllLogs();
  renderCrashes();
}

async function stopLogcat() {
  await invoke("logcat_stop");
  logcatRunning = false;
  logcatToggleBtn.textContent = "Start Logcat";
  logcatPidPill.textContent = "idle";
}

function escapeHtml(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// --- Event Wiring ---

refreshDevicesBtn.onclick = () => void refreshDevices();
deviceSelect.onchange = () => void refreshPackages();

openFolderBtn.onclick = async () => {
  try {
    const selected = await openDialog({ directory: true, multiple: false });
    if (typeof selected === "string" && selected) {
      await openProjectFolder(selected);
    }
  } catch (err) {
    showBanner(`Folder picker error: ${String(err)}`);
  }
};

treeFilterInput.oninput = () => renderFileTree();

async function startSession(launch: boolean) {
  hideBanner();
  const pkg = packageInput.value.trim();
  if (!pkg) {
    showBanner("Enter or pick an Android package name first.");
    return;
  }
  localStorage.setItem("android-lens:package", pkg);
  const serial = deviceSelect.value || null;

  attachBtn.disabled = true;
  launchBtn.disabled = true;
  statusBadge.textContent = launch ? "Launching -D…" : "Attaching…";

  try {
    const status = await invoke<SessionStatus>("debug_attach", {
      serial,
      package: pkg,
      launch,
      activity: null,
      port: 5005,
    });
    applyStatus(status);
    await refreshThreads();
    // Automatically start package-scoped logcat stream for this app
    await startLogcat(pkg);
  } catch (err) {
    applyStatus(await invoke<SessionStatus>("debug_get_status"));
    showBanner(String(err));
  }
}

attachBtn.onclick = () => void startSession(false);
launchBtn.onclick = () => void startSession(true);
detachBtn.onclick = async () => {
  hideBanner();
  const status = await invoke<SessionStatus>("debug_detach");
  applyStatus(status);
};

pauseBtn.onclick = async () => {
  try {
    const status = await invoke<SessionStatus>("debug_pause");
    applyStatus(status);
    selectedThreadId = status.paused_thread_id;
    selectedFrameIndex = 0;
    await refreshThreads();
    await refreshCallStack(selectedThreadId ?? undefined);
  } catch (err) {
    showBanner(String(err));
  }
};

resumeBtn.onclick = async () => {
  try {
    const status = await invoke<SessionStatus>("debug_resume");
    applyStatus(status);
  } catch (err) {
    showBanner(String(err));
  }
};

async function runStep(depth: "over" | "into" | "out") {
  try {
    await invoke("debug_step", { depth });
  } catch (err) {
    showBanner(String(err));
  }
}

stepOverBtn.onclick = () => void runStep("over");
stepIntoBtn.onclick = () => void runStep("into");
stepOutBtn.onclick = () => void runStep("out");

// Keyboard shortcuts
window.addEventListener("keydown", (e) => {
  if (currentStatus.state !== "paused") return;
  if (e.key === "F5") {
    e.preventDefault();
    resumeBtn.click();
  } else if (e.key === "F10") {
    e.preventDefault();
    void runStep("over");
  } else if (e.key === "F11" && e.shiftKey) {
    e.preventDefault();
    void runStep("out");
  } else if (e.key === "F11") {
    e.preventDefault();
    void runStep("into");
  }
});

// Dock tabs
document.querySelectorAll<HTMLButtonElement>(".dock-tab").forEach((btn) => {
  btn.onclick = () => {
    const tab = btn.dataset.tab!;
    document.querySelectorAll(".dock-tab").forEach((b) => b.classList.remove("active"));
    document.querySelectorAll(".dock-panel").forEach((p) => p.classList.remove("active"));
    btn.classList.add("active");
    document.getElementById(`tab-${tab}`)?.classList.add("active");
    const controls = document.getElementById("logcat-controls")!;
    controls.style.display = tab === "logcat" ? "flex" : "none";
  };
});

logcatLevelSelect.onchange = () => renderAllLogs();
logcatTagInput.oninput = () => renderAllLogs();
logcatQueryInput.oninput = () => renderAllLogs();

logcatToggleBtn.onclick = async () => {
  if (logcatRunning) {
    await stopLogcat();
  } else {
    const pkg = packageInput.value.trim();
    if (!pkg) {
      showBanner("Select a package first to start logcat.");
      return;
    }
    await startLogcat(pkg);
  }
};

logcatClearBtn.onclick = async () => {
  await invoke("logcat_clear");
  logEntries = [];
  crashGroups = [];
  renderAllLogs();
  renderCrashes();
};

// Backend event streams
async function init() {
  await listen<DebugUiEvent>("debug://event", async (ev) => {
    const payload = ev.payload;
    switch (payload.kind) {
      case "status_changed":
        applyStatus(payload.status);
        break;
      case "breakpoints_changed":
        updateBreakpoints(payload.breakpoints);
        break;
      case "paused": {
        selectedThreadId = payload.thread_id_hex;
        selectedFrameIndex = 0;
        const { source_file, line_number, class_name } = payload.location;
        if (source_file && line_number) {
          execHighlight = { fileName: source_file, line: line_number };
          await openBySourceName(source_file, line_number, class_name);
          syncEditorMarkers();
        }
        await refreshThreads();
        await refreshCallStack(payload.thread_id_hex);
        break;
      }
      case "resumed":
        execHighlight = null;
        syncEditorMarkers();
        break;
      case "vm_exited":
        showBanner("Target app process exited (VM_DEATH).", true);
        break;
      case "message":
        showBanner(payload.text, true);
        break;
    }
  });

  await listen<LogcatEvent>("logcat://event", (ev) => {
    const payload = ev.payload;
    switch (payload.kind) {
      case "entry":
        appendLogEntry(payload.entry);
        break;
      case "crash":
        crashGroups.push(payload.crash);
        renderCrashes();
        break;
      case "attached":
        logcatPidPill.textContent = `PID ${payload.pid}`;
        break;
      case "waiting_for_process":
        logcatPidPill.textContent = "waiting…";
        break;
      case "error":
        showBanner(`Logcat error: ${payload.message}`);
        break;
    }
  });

  await refreshDevices();
  updateBreakpoints(await invoke<BreakpointDto[]>("debug_list_breakpoints"));
  applyStatus(await invoke<SessionStatus>("debug_get_status"));

  if (projectRoot) {
    try {
      await openProjectFolder(projectRoot);
    } catch {
      projectRoot = null;
      localStorage.removeItem("android-lens:projectRoot");
    }
  }
}

void init();
