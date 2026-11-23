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

interface ProjectFile {
  name: string;
  rel_path: string;
  abs_path: string;
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

type PinKind = "stack_frame" | "variable" | "logcat_entry" | "crash";

interface PinnedItem {
  id: string;
  kind: PinKind;
  title: string;
  summary: string;
  note: string | null;
  payload: unknown;
  pinned_at_ms: number;
}

interface McpStatusDto {
  running: boolean;
  port: number;
  endpoint: string;
  pinned_count: number;
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
  | { kind: "vm_exited" };

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

const SYSTEM_FRAME_PREFIXES = [
  "android.",
  "androidx.",
  "java.",
  "javax.",
  "dalvik.",
  "kotlin.",
  "kotlinx.",
  "com.android.",
  "sun.",
  "libcore.",
];

const MAX_RENDERED_LOGS = 1500;

const CHEVRON_RIGHT = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l4 4-4 4"/></svg>`;
const CHEVRON_DOWN = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6l4 4 4-4"/></svg>`;
const PIN_ICON = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9.8 2.2 13.8 6.2l-1.5 1.5-1-.3-2.5 2.5.4 1.6-1.2 1.2-2.7-2.7-2.6 2.6-.7-.7 2.6-2.6-2.7-2.7 1.2-1.2 1.6.4 2.5-2.5-.3-1 1.5-1.5z"/></svg>`;
const MOON_PATH = `<path d="M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5z" />`;
const SUN_PATH = `<circle cx="8" cy="8" r="3" /><path d="M8 1.75v1.5M8 12.75v1.5M1.75 8h1.5M12.75 8h1.5M3.6 3.6l1.05 1.05M11.35 11.35l1.05 1.05M12.4 3.6l-1.05 1.05M4.65 11.35 3.6 12.4" />`;

// DOM refs
const deviceSelect = document.getElementById("device-select") as HTMLSelectElement;
const refreshDevicesBtn = document.getElementById("refresh-devices-btn") as HTMLButtonElement;
const packageInput = document.getElementById("package-input") as HTMLInputElement;
const packageDatalist = document.getElementById("package-datalist") as HTMLDataListElement;

const controlsDisconnected = document.getElementById("controls-disconnected") as HTMLDivElement;
const controlsRunning = document.getElementById("controls-running") as HTMLDivElement;
const controlsPaused = document.getElementById("controls-paused") as HTMLDivElement;

const debugAutoBtn = document.getElementById("debug-auto-btn") as HTMLButtonElement;
const debugMenuBtn = document.getElementById("debug-menu-btn") as HTMLButtonElement;
const debugMenu = document.getElementById("debug-menu") as HTMLDivElement;
const pauseBtn = document.getElementById("pause-btn") as HTMLButtonElement;
const stopRunningBtn = document.getElementById("stop-running-btn") as HTMLButtonElement;
const stopPausedBtn = document.getElementById("stop-paused-btn") as HTMLButtonElement;
const resumeBtn = document.getElementById("resume-btn") as HTMLButtonElement;
const stepOverBtn = document.getElementById("step-over-btn") as HTMLButtonElement;
const stepIntoBtn = document.getElementById("step-into-btn") as HTMLButtonElement;
const stepOutBtn = document.getElementById("step-out-btn") as HTMLButtonElement;

const statusPill = document.getElementById("status-pill") as HTMLSpanElement;
const statusText = document.getElementById("status-text") as HTMLSpanElement;
const mcpPillBtn = document.getElementById("mcp-pill-btn") as HTMLButtonElement;
const mcpPillText = document.getElementById("mcp-pill-text") as HTMLSpanElement;
const mcpPopover = document.getElementById("mcp-popover") as HTMLDivElement;
const mcpPopEndpoint = document.getElementById("mcp-pop-endpoint") as HTMLSpanElement;
const mcpSnippet = document.getElementById("mcp-snippet") as HTMLPreElement;
const mcpCopyBtn = document.getElementById("mcp-copy-btn") as HTMLButtonElement;
const mcpCopyFeedback = document.getElementById("mcp-copy-feedback") as HTMLSpanElement;
const toggleLeftBtn = document.getElementById("toggle-left-btn") as HTMLButtonElement;
const toggleDockBtn = document.getElementById("toggle-dock-btn") as HTMLButtonElement;
const themeToggleBtn = document.getElementById("theme-toggle-btn") as HTMLButtonElement;
const themeIcon = document.getElementById("theme-icon") as unknown as SVGSVGElement;
const banner = document.getElementById("banner") as HTMLDivElement;

const workspaceEl = document.getElementById("workspace") as HTMLElement;
const dockEl = document.getElementById("dock") as HTMLElement;

const quickOpenBtn = document.getElementById("quick-open-btn") as HTMLButtonElement;
const openFolderBtn = document.getElementById("open-folder-btn") as HTMLButtonElement;
const guideOpenBtn = document.getElementById("guide-open-btn") as HTMLButtonElement;
const projectPathBar = document.getElementById("project-path-bar") as HTMLDivElement;
const treeFilterInput = document.getElementById("tree-filter-input") as HTMLInputElement;
const fileTreeEl = document.getElementById("file-tree") as HTMLDivElement;

const guideDeviceRow = document.getElementById("guide-device") as HTMLDivElement;
const guideDeviceSub = document.getElementById("guide-device-sub") as HTMLDivElement;
const guideAppRow = document.getElementById("guide-app") as HTMLDivElement;
const guideAppSub = document.getElementById("guide-app-sub") as HTMLDivElement;
const guideFolderRow = document.getElementById("guide-folder") as HTMLDivElement;
const guideFolderSub = document.getElementById("guide-folder-sub") as HTMLDivElement;

const editorTabsEl = document.getElementById("editor-tabs") as HTMLDivElement;
const editorContainer = document.getElementById("editor-container") as HTMLDivElement;
const editorEmpty = document.getElementById("editor-empty") as HTMLDivElement;

const threadSelect = document.getElementById("thread-select") as HTMLSelectElement;
const dimSystemBtn = document.getElementById("dim-system-btn") as HTMLButtonElement;
const stackListEl = document.getElementById("stack-list") as HTMLDivElement;
const varsTreeEl = document.getElementById("vars-tree") as HTMLDivElement;
const varsFrameLabelEl = document.getElementById("vars-frame-label") as HTMLSpanElement;

const logcatPidPill = document.getElementById("logcat-pid-pill") as HTMLSpanElement;
const crashesCountEl = document.getElementById("crashes-count") as HTMLSpanElement;
const bpsCountEl = document.getElementById("bps-count") as HTMLSpanElement;
const pinsCountEl = document.getElementById("pins-count") as HTMLSpanElement;
const logcatLevelSelect = document.getElementById("logcat-level") as HTMLSelectElement;
const logcatTagInput = document.getElementById("logcat-tag") as HTMLInputElement;
const logcatQueryInput = document.getElementById("logcat-query") as HTMLInputElement;
const logcatAutoScroll = document.getElementById("logcat-autoscroll") as HTMLInputElement;
const logcatToggleBtn = document.getElementById("logcat-toggle-btn") as HTMLButtonElement;
const logcatClearBtn = document.getElementById("logcat-clear-btn") as HTMLButtonElement;
const pinsClearBtn = document.getElementById("pins-clear-btn") as HTMLButtonElement;
const logcatListEl = document.getElementById("logcat-list") as HTMLDivElement;
const crashesListEl = document.getElementById("crashes-list") as HTMLDivElement;
const breakpointsListEl = document.getElementById("breakpoints-list") as HTMLDivElement;
const pinnedListEl = document.getElementById("pinned-list") as HTMLDivElement;

const quickOpenBackdrop = document.getElementById("quick-open-backdrop") as HTMLDivElement;
const quickOpenInput = document.getElementById("quick-open-input") as HTMLInputElement;
const quickOpenResults = document.getElementById("quick-open-results") as HTMLDivElement;

// State
let projectRoot: string | null = localStorage.getItem("android-lens:projectRoot");
let projectFiles: ProjectFile[] = [];
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
let cachedFrames: StackFrameDto[] = [];
let dimSystemFrames = true;
let execHighlight: { fileName: string; line: number } | null = null;

let logcatRunning = false;
let logEntries: LogEntry[] = [];
let crashGroups: CrashGroup[] = [];
let pinnedItems: PinnedItem[] = [];

let paletteMatches: ProjectFile[] = [];
let paletteIndex = 0;

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

// --- Theme & Layout Persistence ---

function applyTheme(theme: "dark" | "light") {
  document.documentElement.setAttribute("data-theme", theme);
  localStorage.setItem("android-lens:theme", theme);
  themeIcon.innerHTML = theme === "dark" ? SUN_PATH : MOON_PATH;
}

function toggleLeftPane(force?: boolean) {
  const collapsed = force ?? !workspaceEl.classList.contains("left-collapsed");
  workspaceEl.classList.toggle("left-collapsed", collapsed);
  toggleLeftBtn.classList.toggle("active", !collapsed);
  localStorage.setItem("android-lens:leftCollapsed", collapsed ? "1" : "0");
}

function toggleDock(force?: boolean) {
  const collapsed = force ?? !dockEl.classList.contains("collapsed");
  dockEl.classList.toggle("collapsed", collapsed);
  toggleDockBtn.classList.toggle("active", !collapsed);
  localStorage.setItem("android-lens:dockCollapsed", collapsed ? "1" : "0");
}

function showBanner(text: string, isInfo = false) {
  banner.innerHTML = "";
  const msg = document.createElement("span");
  msg.textContent = text;
  const close = document.createElement("button");
  close.className = "btn btn-ghost-xs";
  close.textContent = "Dismiss";
  close.onclick = hideBanner;
  banner.className = isInfo ? "banner info" : "banner";
  banner.append(msg, close);
}

function hideBanner() {
  banner.classList.add("hidden");
}

function updateGuideChecklist() {
  const hasDevice = Boolean(deviceSelect.value);
  guideDeviceRow.classList.toggle("done", hasDevice);
  guideDeviceRow.querySelector(".guide-mark")!.textContent = hasDevice ? "✓" : "1";
  guideDeviceSub.textContent = hasDevice
    ? deviceSelect.options[deviceSelect.selectedIndex]?.textContent ?? deviceSelect.value
    : "Connect a device or start an Android emulator";

  const pkg = packageInput.value.trim();
  const hasPkg = pkg.length > 0;
  guideAppRow.classList.toggle("done", hasPkg);
  guideAppRow.querySelector(".guide-mark")!.textContent = hasPkg ? "✓" : "2";
  guideAppSub.textContent = hasPkg ? pkg : "Pick a debuggable package from the top bar";

  const hasFolder = Boolean(projectRoot);
  guideFolderRow.classList.toggle("done", hasFolder);
  guideFolderRow.querySelector(".guide-mark")!.textContent = hasFolder ? "✓" : "3";
  guideFolderSub.textContent = projectRoot
    ? projectRoot
    : "Open your project to browse .kt / .java files & click gutters for breakpoints";
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
      updateGuideChecklist();
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
    updateGuideChecklist();
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
    updateGuideChecklist();
  } catch {
    // Ignore transient package lookup failure
  }
}

// --- Project Folder & File Tree ---

async function openProjectFolder(path: string) {
  projectRoot = path;
  localStorage.setItem("android-lens:projectRoot", path);
  await invoke("fs_set_project_root", { path });
  const folderName = path.split("/").filter(Boolean).pop() ?? path;
  projectPathBar.textContent = folderName;
  projectPathBar.title = path;
  expandedDirs.clear();
  dirCache.clear();
  expandedDirs.add(path);
  await loadDir(path);
  renderFileTree();
  updateGuideChecklist();

  // Populate ⌘P Quick Open index asynchronously without blocking UI
  invoke<ProjectFile[]>("fs_list_project_files", { projectRoot: path })
    .then((files) => {
      projectFiles = files;
    })
    .catch(() => {
      projectFiles = [];
    });
}

async function pickFolder() {
  try {
    const selected = await openDialog({ directory: true, multiple: false });
    if (typeof selected === "string" && selected) {
      await openProjectFolder(selected);
    }
  } catch (err) {
    showBanner(`Folder picker error: ${String(err)}`);
  }
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
    fileTreeEl.innerHTML = `<div class="empty-hint">Open a project folder to browse source files</div>`;
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
      row.style.paddingLeft = `${10 + depth * 14}px`;

      const caret = document.createElement("span");
      caret.className = "tree-caret";
      if (entry.is_dir) {
        caret.innerHTML = expandedDirs.has(entry.path) ? CHEVRON_DOWN : CHEVRON_RIGHT;
      }

      const badge = document.createElement("span");
      badge.className = "tree-badge";
      if (entry.is_dir) {
        badge.classList.add("dir");
        badge.innerHTML = `<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.3 1.5h4.7A1.5 1.5 0 0 1 14 6v6a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12V4.5z"/></svg>`;
      } else if (entry.name.endsWith(".kt") || entry.name.endsWith(".kts")) {
        badge.classList.add("kt");
        badge.textContent = "K";
      } else if (entry.name.endsWith(".java")) {
        badge.classList.add("java");
        badge.textContent = "J";
      } else {
        badge.textContent = "·";
      }

      const label = document.createElement("span");
      label.textContent = entry.name;

      row.append(caret, badge, label);
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

// --- ⌘P Quick Open Palette ---

function openQuickOpen() {
  if (!projectRoot) {
    void pickFolder();
    return;
  }
  quickOpenBackdrop.classList.remove("hidden");
  quickOpenInput.value = "";
  paletteIndex = 0;
  renderPaletteResults();
  quickOpenInput.focus();
}

function closeQuickOpen() {
  quickOpenBackdrop.classList.add("hidden");
}

function renderPaletteResults() {
  const q = quickOpenInput.value.trim().toLowerCase();
  paletteMatches = (
    q
      ? projectFiles.filter(
          (f) => f.name.toLowerCase().includes(q) || f.rel_path.toLowerCase().includes(q),
        )
      : projectFiles
  ).slice(0, 40);

  if (paletteIndex >= paletteMatches.length) paletteIndex = 0;
  quickOpenResults.innerHTML = "";

  if (paletteMatches.length === 0) {
    quickOpenResults.innerHTML = `<div class="empty-hint">No matching source files</div>`;
    return;
  }

  paletteMatches.forEach((f, idx) => {
    const item = document.createElement("div");
    item.className = "palette-item" + (idx === paletteIndex ? " selected" : "");
    item.innerHTML = `<span class="palette-name">${escapeHtml(f.name)}</span><span class="palette-path">${escapeHtml(f.rel_path)}</span>`;
    item.onclick = () => {
      closeQuickOpen();
      void openSourceFile(f.abs_path);
    };
    quickOpenResults.appendChild(item);
  });
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
    editorEmpty.classList.add("hidden");
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
          editorEmpty.classList.remove("hidden");
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

// --- Progressive Toolbar & Debugger State ---

function applyStatus(status: SessionStatus) {
  currentStatus = status;

  controlsDisconnected.classList.toggle("hidden", status.state !== "disconnected");
  controlsRunning.classList.toggle("hidden", status.state !== "running");
  controlsPaused.classList.toggle("hidden", status.state !== "paused");

  deviceSelect.disabled = status.state !== "disconnected";
  packageInput.disabled = status.state !== "disconnected";

  statusPill.className = `status-pill ${status.state}`;
  if (status.state === "disconnected") {
    statusText.textContent = "Ready";
    statusPill.title = "";
    execHighlight = null;
    syncEditorMarkers();
    threadSelect.innerHTML = `<option value="">No thread</option>`;
    threadSelect.disabled = true;
    stackListEl.innerHTML = `<div class="empty-hint">Pause or hit a breakpoint</div>`;
    varsTreeEl.innerHTML = `<div class="empty-hint">Variables appear here when paused</div>`;
    varsFrameLabelEl.textContent = "";
    cachedFrames = [];
  } else if (status.state === "running") {
    statusText.textContent = `Running · PID ${status.pid ?? "?"}`;
    statusPill.title = status.vm_info ?? "";
    execHighlight = null;
    syncEditorMarkers();
  } else {
    const loc = status.paused_location;
    const where = loc?.source_file
      ? `${loc.source_file}:${loc.line_number ?? "?"}`
      : loc?.method_name ?? status.paused_thread_name ?? "Paused";
    statusText.textContent = `Paused · ${where}`;
    statusPill.title = `${status.paused_thread_name ?? ""} — ${status.vm_info ?? ""}`;
  }
}

async function refreshThreads() {
  if (currentStatus.state === "disconnected") return;
  try {
    const threads = await invoke<ThreadDto[]>("debug_list_threads");
    threadSelect.innerHTML = "";
    threadSelect.disabled = threads.length === 0;
    for (const t of threads) {
      const opt = document.createElement("option");
      opt.value = t.id_hex;
      opt.textContent = t.is_paused ? `${t.name} (paused)` : t.name;
      threadSelect.appendChild(opt);
    }
    if (selectedThreadId && threads.some((t) => t.id_hex === selectedThreadId)) {
      threadSelect.value = selectedThreadId;
    } else if (threads.length > 0) {
      selectedThreadId = threads[0].id_hex;
      threadSelect.value = selectedThreadId;
    }
  } catch {
    // Ignore if VM resumed mid-query
  }
}

function isSystemFrame(className: string): boolean {
  return SYSTEM_FRAME_PREFIXES.some((prefix) => className.startsWith(prefix));
}

function renderCallStack() {
  stackListEl.innerHTML = "";
  if (cachedFrames.length === 0) {
    stackListEl.innerHTML = `<div class="empty-hint">No stack frames</div>`;
    return;
  }

  for (const f of cachedFrames) {
    const sys = isSystemFrame(f.class_name);
    const row = document.createElement("div");
    row.className =
      "frame-row" +
      (f.index === selectedFrameIndex ? " active" : "") +
      (dimSystemFrames && sys ? " system-frame" : "");

    const shortCls = f.class_name.split(".").pop() ?? f.class_name;
    const locText = f.source_file ? `${f.source_file}:${f.line_number ?? "?"}` : "native";
    row.title = `${f.class_name}.${f.method_name} (${locText})`;

    const nameEl = document.createElement("span");
    nameEl.className = "frame-name";
    nameEl.textContent = `${shortCls}.${f.method_name}`;

    const right = document.createElement("span");
    right.className = "frame-right";
    const locEl = document.createElement("span");
    locEl.className = "frame-loc";
    locEl.textContent = locText;

    const pinBtn = makePinButton("Pin stack frame to AI agent", () =>
      pinContext({
        kind: "stack_frame",
        title: `${shortCls}.${f.method_name}`,
        summary: `${f.class_name}.${f.method_name} (${locText})`,
        payload: {
          frame_index: f.index,
          class_name: f.class_name,
          method_name: f.method_name,
          source_file: f.source_file,
          line_number: f.line_number,
          thread_id: selectedThreadId,
        },
      }),
    );

    right.append(locEl, pinBtn);
    row.append(nameEl, right);

    row.onclick = async () => {
      selectedFrameIndex = f.index;
      renderCallStack();
      await refreshLocals(selectedThreadId ?? undefined, f.index, f);
      if (f.source_file && f.line_number) {
        await openBySourceName(f.source_file, f.line_number, f.class_name);
      }
    };
    stackListEl.appendChild(row);
  }
}

async function refreshCallStack(threadIdHex?: string) {
  try {
    cachedFrames = await invoke<StackFrameDto[]>("debug_get_call_stack", {
      threadIdHex: threadIdHex ?? selectedThreadId,
    });
    renderCallStack();
    if (cachedFrames.length > 0) {
      const activeFrame = cachedFrames[selectedFrameIndex] ?? cachedFrames[0];
      await refreshLocals(threadIdHex ?? selectedThreadId ?? undefined, activeFrame.index, activeFrame);
    }
  } catch (err) {
    stackListEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

async function refreshLocals(
  threadIdHex: string | undefined,
  frameIndex: number,
  frame?: StackFrameDto,
) {
  varsFrameLabelEl.textContent = frame ? `${frame.method_name}()` : `#${frameIndex}`;
  try {
    const vars = await invoke<VariableDto[]>("debug_get_locals", {
      threadIdHex: threadIdHex ?? null,
      frameIndex,
    });
    varsTreeEl.innerHTML = "";
    if (vars.length === 0) {
      varsTreeEl.innerHTML = `<div class="empty-hint">No local variables in scope</div>`;
      return;
    }
    for (const v of vars) {
      varsTreeEl.appendChild(createVarNode(v, 0));
    }
  } catch (err) {
    varsTreeEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

function formatReadableValue(v: VariableDto): string {
  // Tuck raw `@0x...` address into tooltip; show short class name inline for objects
  if (v.object_id_hex && v.value.includes("@0x")) {
    const clsPart = v.value.split("@0x")[0];
    return clsPart.split(".").pop() ?? clsPart;
  }
  return v.value;
}

function createVarNode(v: VariableDto, depth: number): HTMLElement {
  const container = document.createElement("div");

  const row = document.createElement("div");
  row.className = "var-row" + (v.is_expandable ? " expandable" : "");
  row.style.paddingLeft = `${10 + depth * 14}px`;
  if (v.object_id_hex) {
    row.title = `${v.value} (${v.type_name})`;
  }

  const caret = document.createElement("span");
  caret.className = "var-caret";
  if (v.is_expandable) {
    caret.innerHTML = CHEVRON_RIGHT;
  }
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

  const eq = document.createElement("span");
  eq.className = "muted";
  eq.textContent = "=";
  row.appendChild(eq);

  const valEl = document.createElement("span");
  valEl.className = "var-val";
  valEl.textContent = formatReadableValue(v);
  row.appendChild(valEl);

  if (v.type_name && !v.is_expandable) {
    const shortType = v.type_name.split(".").pop() ?? v.type_name;
    const typeEl = document.createElement("span");
    typeEl.className = "var-type";
    typeEl.textContent = shortType;
    row.appendChild(typeEl);
  }

  if (v.declaring_class) {
    const owner = document.createElement("span");
    owner.className = "var-owner";
    owner.textContent = v.declaring_class.split(".").pop() ?? v.declaring_class;
    owner.title = `Inherited from ${v.declaring_class}`;
    row.appendChild(owner);
  }

  const pinBtn = makePinButton("Pin variable to AI agent", () =>
    pinContext({
      kind: "variable",
      title: v.name,
      summary: `${v.name} : ${v.type_name || "value"} = ${v.value}`,
      payload: {
        name: v.name,
        type_name: v.type_name,
        value: v.value,
        object_id_hex: v.object_id_hex,
        is_static: v.is_static,
        declaring_class: v.declaring_class,
        frame_index: selectedFrameIndex,
      },
    }),
  );
  row.appendChild(pinBtn);

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
      caret.innerHTML = expanded ? CHEVRON_DOWN : CHEVRON_RIGHT;
      childrenWrap.style.display = expanded ? "block" : "none";

      if (expanded && !loaded) {
        loaded = true;
        childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${10 + (depth + 1) * 14}px">Loading…</div>`;
        try {
          const children = await invoke<VariableDto[]>("debug_inspect_object", {
            objectIdHex: objId,
          });
          childrenWrap.innerHTML = "";
          if (children.length === 0) {
            childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${10 + (depth + 1) * 14}px">(empty)</div>`;
          } else {
            for (const child of children) {
              childrenWrap.appendChild(createVarNode(child, depth + 1));
            }
          }
        } catch (err) {
          loaded = false;
          childrenWrap.innerHTML = `<div class="var-row muted" style="padding-left:${10 + (depth + 1) * 14}px">Error: ${escapeHtml(String(err))}</div>`;
        }
      }
    };
  }

  return container;
}

// --- Breakpoints ---

function updateBreakpoints(next: BreakpointDto[]) {
  breakpoints = next;
  bpsCountEl.textContent = String(breakpoints.length);
  syncEditorMarkers();

  breakpointsListEl.innerHTML = "";
  if (breakpoints.length === 0) {
    breakpointsListEl.innerHTML = `<div class="empty-hint">Click the gutter next to any source line to add a breakpoint</div>`;
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
      ? `${bp.class_name.split(".").pop()}.${bp.method_name ?? ""}`
      : "Waiting for class to load…";

    left.append(dot, desc, detail);
    left.onclick = () => void openBySourceName(bp.file, bp.line, bp.class_name ?? undefined);

    const rm = document.createElement("button");
    rm.className = "btn btn-ghost-xs";
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
  if (q && !entry.message.toLowerCase().includes(q) && !entry.tag.toLowerCase().includes(q)) {
    return false;
  }
  return true;
}

function createLogRow(entry: LogEntry): HTMLElement {
  const row = document.createElement("div");
  row.className = `log-row ${entry.level}`;
  const letter = entry.level[0]?.toUpperCase() ?? "?";
  row.innerHTML = `<span class="log-ts">${escapeHtml(entry.timestamp)}</span><span class="log-lvl ${entry.level}">${letter}</span><span class="log-tag" title="${escapeHtml(entry.tag)}">${escapeHtml(entry.tag)}</span><span>${escapeHtml(entry.message)}</span>`;
  row.appendChild(
    makePinButton("Pin logcat entry to AI agent", () =>
      pinContext({
        kind: "logcat_entry",
        title: `${letter}/${entry.tag || "log"}`,
        summary: `[${entry.timestamp}] ${entry.message}`,
        payload: entry,
      }),
    ),
  );
  return row;
}

function renderAllLogs() {
  logcatListEl.innerHTML = "";
  const filtered = logEntries.filter(matchesLogFilter);
  if (filtered.length === 0) {
    logcatListEl.innerHTML = `<div class="empty-hint">${logcatRunning ? "Waiting for matching logcat lines…" : "Logcat streams automatically when debugging starts"}</div>`;
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
  crashesCountEl.className = crashGroups.length > 0 ? "pill alert" : "pill";
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
    const left = document.createElement("span");
    left.textContent = crash.summary;
    const right = document.createElement("span");
    right.className = "frame-right";
    const meta = document.createElement("span");
    meta.className = "muted";
    meta.textContent = `${crash.timestamp} · PID ${crash.pid}`;
    const pinBtn = makePinButton("Pin crash stack to AI agent", () =>
      pinContext({
        kind: "crash",
        title: crash.summary,
        summary: `${crash.process ?? "app"} (PID ${crash.pid}) · ${crash.lines.length} lines`,
        payload: crash,
      }),
    );
    right.append(meta, pinBtn);
    head.append(left, right);

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
  logcatToggleBtn.textContent = "Stop";
  logEntries = [];
  crashGroups = [];
  renderAllLogs();
  renderCrashes();
}

async function stopLogcat() {
  await invoke("logcat_stop");
  logcatRunning = false;
  logcatToggleBtn.textContent = "Start";
  logcatPidPill.textContent = "idle";
}

function escapeHtml(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// --- Pinned Context & MCP Bridge ---

function makePinButton(title: string, onPin: () => Promise<void>): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.className = "pin-btn";
  btn.title = title;
  btn.innerHTML = `${PIN_ICON}<span>Pin</span>`;
  btn.onclick = async (e) => {
    e.stopPropagation();
    await onPin();
    btn.classList.add("pinned");
    btn.innerHTML = `${PIN_ICON}<span>Pinned</span>`;
    setTimeout(() => {
      btn.classList.remove("pinned");
      btn.innerHTML = `${PIN_ICON}<span>Pin</span>`;
    }, 1200);
  };
  return btn;
}

async function pinContext(req: {
  kind: PinKind;
  title: string;
  summary: string;
  payload: unknown;
}) {
  try {
    const items = await invoke<PinnedItem[]>("pin_add", {
      req: { ...req, note: null },
    });
    updatePinnedItems(items);
  } catch (err) {
    showBanner(`Pin error: ${String(err)}`);
  }
}

function updatePinnedItems(items: PinnedItem[]) {
  pinnedItems = items;
  pinsCountEl.textContent = String(pinnedItems.length);
  pinnedListEl.innerHTML = "";

  if (pinnedItems.length === 0) {
    pinnedListEl.innerHTML = `<div class="empty-hint">Hover any variable, stack frame, logcat line, or crash and click Pin to share it with your AI agent</div>`;
    return;
  }

  const kindLabel: Record<PinKind, string> = {
    stack_frame: "Frame",
    variable: "Var",
    logcat_entry: "Log",
    crash: "Crash",
  };

  for (const item of [...pinnedItems].reverse()) {
    const row = document.createElement("div");
    row.className = "pin-row";

    const left = document.createElement("div");
    left.className = "pin-left";
    const badge = document.createElement("span");
    badge.className = "pin-kind";
    badge.textContent = kindLabel[item.kind];
    const title = document.createElement("span");
    title.className = "pin-title";
    title.textContent = item.title;
    const summary = document.createElement("span");
    summary.className = "pin-summary";
    summary.textContent = item.summary;
    summary.title = item.summary;
    left.append(badge, title, summary);

    const rm = document.createElement("button");
    rm.className = "btn btn-ghost-xs";
    rm.textContent = "Remove";
    rm.onclick = async () => {
      const updated = await invoke<PinnedItem[]>("pin_remove", { id: item.id });
      updatePinnedItems(updated);
    };

    row.append(left, rm);
    pinnedListEl.appendChild(row);
  }
}

async function refreshMcpStatus() {
  try {
    const st = await invoke<McpStatusDto>("mcp_status");
    if (st.running) {
      mcpPillText.textContent = `MCP :${st.port}`;
      mcpPopEndpoint.textContent = st.endpoint;
      mcpSnippet.textContent = JSON.stringify(
        {
          mcpServers: {
            "android-lens": {
              command: "android-lens-mcp",
              args: ["--desktop-port", String(st.port)],
            },
          },
        },
        null,
        2,
      );
    } else {
      mcpPillText.textContent = "MCP starting…";
    }
  } catch {
    mcpPillText.textContent = "MCP";
  }
}

// --- Session Actions ---

async function startSession(mode: "auto" | "attach" | "launch") {
  debugMenu.classList.add("hidden");
  hideBanner();
  const pkg = packageInput.value.trim();
  if (!pkg) {
    showBanner("Select or enter an Android package name first.");
    return;
  }
  localStorage.setItem("android-lens:package", pkg);
  const serial = deviceSelect.value || null;

  debugAutoBtn.disabled = true;
  debugMenuBtn.disabled = true;
  statusText.textContent = mode === "launch" ? "Launching…" : "Connecting…";

  try {
    const status = await invoke<SessionStatus>("debug_attach", {
      serial,
      package: pkg,
      mode,
      activity: null,
      port: 5005,
    });
    applyStatus(status);
    await refreshThreads();
    await startLogcat(pkg);
  } catch (err) {
    applyStatus(await invoke<SessionStatus>("debug_get_status"));
    showBanner(String(err));
  } finally {
    debugAutoBtn.disabled = false;
    debugMenuBtn.disabled = false;
  }
}

async function detachSession() {
  hideBanner();
  const status = await invoke<SessionStatus>("debug_detach");
  applyStatus(status);
}

async function runStep(depth: "over" | "into" | "out") {
  try {
    await invoke("debug_step", { depth });
  } catch (err) {
    showBanner(String(err));
  }
}

// --- Event Wiring ---

refreshDevicesBtn.onclick = () => void refreshDevices();
deviceSelect.onchange = () => void refreshPackages();
packageInput.oninput = () => updateGuideChecklist();

debugAutoBtn.onclick = () => void startSession("auto");
debugMenuBtn.onclick = (e) => {
  e.stopPropagation();
  debugMenu.classList.toggle("hidden");
};
debugMenu.querySelectorAll<HTMLButtonElement>(".popover-item").forEach((item) => {
  item.onclick = () => {
    const mode = (item.dataset.mode as "auto" | "attach" | "launch") ?? "auto";
    void startSession(mode);
  };
});
document.addEventListener("click", () => {
  debugMenu.classList.add("hidden");
  mcpPopover.classList.add("hidden");
});

mcpPillBtn.onclick = async (e) => {
  e.stopPropagation();
  debugMenu.classList.add("hidden");
  await refreshMcpStatus();
  mcpPopover.classList.toggle("hidden");
};
mcpPopover.onclick = (e) => e.stopPropagation();
mcpCopyBtn.onclick = async () => {
  try {
    await navigator.clipboard.writeText(mcpSnippet.textContent ?? "");
    mcpCopyFeedback.textContent = "Copied";
    setTimeout(() => {
      mcpCopyFeedback.textContent = "";
    }, 1500);
  } catch {
    mcpCopyFeedback.textContent = "Select & copy above";
  }
};

pinsClearBtn.onclick = async () => {
  const updated = await invoke<PinnedItem[]>("pin_clear");
  updatePinnedItems(updated);
};

stopRunningBtn.onclick = () => void detachSession();
stopPausedBtn.onclick = () => void detachSession();

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

stepOverBtn.onclick = () => void runStep("over");
stepIntoBtn.onclick = () => void runStep("into");
stepOutBtn.onclick = () => void runStep("out");

threadSelect.onchange = async () => {
  selectedThreadId = threadSelect.value || null;
  selectedFrameIndex = 0;
  if (currentStatus.state === "paused" && selectedThreadId) {
    await refreshCallStack(selectedThreadId);
  }
};

dimSystemBtn.onclick = () => {
  dimSystemFrames = !dimSystemFrames;
  dimSystemBtn.classList.toggle("active", dimSystemFrames);
  renderCallStack();
};

openFolderBtn.onclick = () => void pickFolder();
guideOpenBtn.onclick = () => void pickFolder();
quickOpenBtn.onclick = () => openQuickOpen();
treeFilterInput.oninput = () => renderFileTree();

toggleLeftBtn.onclick = () => toggleLeftPane();
toggleDockBtn.onclick = () => toggleDock();
themeToggleBtn.onclick = () => {
  const cur = document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
  applyTheme(cur === "dark" ? "light" : "dark");
};

// Quick Open events
quickOpenBackdrop.onclick = (e) => {
  if (e.target === quickOpenBackdrop) closeQuickOpen();
};
quickOpenInput.oninput = () => {
  paletteIndex = 0;
  renderPaletteResults();
};
quickOpenInput.onkeydown = (e) => {
  if (e.key === "Escape") {
    closeQuickOpen();
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    if (paletteMatches.length > 0) {
      paletteIndex = (paletteIndex + 1) % paletteMatches.length;
      renderPaletteResults();
    }
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (paletteMatches.length > 0) {
      paletteIndex = (paletteIndex - 1 + paletteMatches.length) % paletteMatches.length;
      renderPaletteResults();
    }
  } else if (e.key === "Enter") {
    e.preventDefault();
    const chosen = paletteMatches[paletteIndex];
    if (chosen) {
      closeQuickOpen();
      void openSourceFile(chosen.abs_path);
    }
  }
};

// Global Keyboard Shortcuts
window.addEventListener("keydown", (e) => {
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.key.toLowerCase() === "p") {
    e.preventDefault();
    openQuickOpen();
    return;
  }
  if (mod && e.key.toLowerCase() === "b") {
    e.preventDefault();
    toggleLeftPane();
    return;
  }
  if (mod && e.key.toLowerCase() === "j") {
    e.preventDefault();
    toggleDock();
    return;
  }
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
    const pinnedControls = document.getElementById("pinned-controls")!;
    controls.classList.toggle("hidden", tab !== "logcat");
    pinnedControls.classList.toggle("hidden", tab !== "pinned");
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

// Backend Event Streams & Init
async function init() {
  const savedTheme = localStorage.getItem("android-lens:theme") === "light" ? "light" : "dark";
  applyTheme(savedTheme);
  if (localStorage.getItem("android-lens:leftCollapsed") === "1") toggleLeftPane(true);
  if (localStorage.getItem("android-lens:dockCollapsed") === "1") toggleDock(true);

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
        showBanner("Target app process exited.", true);
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
        logcatPidPill.textContent = "waiting";
        break;
      case "error":
        showBanner(`Logcat error: ${payload.message}`);
        break;
    }
  });

  await refreshDevices();
  updateBreakpoints(await invoke<BreakpointDto[]>("debug_list_breakpoints"));
  updatePinnedItems(await invoke<PinnedItem[]>("pin_list"));
  applyStatus(await invoke<SessionStatus>("debug_get_status"));
  await refreshMcpStatus();
  // Retry once shortly after boot in case the background HTTP MCP server just bound its port
  setTimeout(() => void refreshMcpStatus(), 600);

  if (projectRoot) {
    try {
      await openProjectFolder(projectRoot);
    } catch {
      projectRoot = null;
      localStorage.removeItem("android-lens:projectRoot");
    }
  }
  updateGuideChecklist();
}

void init();
