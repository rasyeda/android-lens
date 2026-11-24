import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { AppearanceManager, THEMES, ThemeId } from "./appearance";
import { BreakpointMark, SourceViewer } from "./editor";
import { LayoutManager } from "./layout";

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

type PinKind = "stack_frame" | "variable" | "logcat_entry" | "crash" | "ui_node" | "storage_entry";

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

interface UiBounds {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

interface UiNode {
  id: string;
  class_name: string;
  package_name: string;
  resource_id: string;
  text: string;
  content_desc: string;
  clickable: boolean;
  enabled: boolean;
  focused: boolean;
  scrollable: boolean;
  selected: boolean;
  bounds: UiBounds;
  children: UiNode[];
}

interface LayoutSnapshot {
  serial: string;
  screen_width: number;
  screen_height: number;
  screenshot_data_url: string;
  roots: UiNode[];
  node_count: number;
}

interface AppStorageIndex {
  serial: string;
  package: string;
  shared_prefs: string[];
  datastores: string[];
  databases: string[];
}

interface PrefEntry {
  key: string;
  value_type: string;
  value: string;
}

interface SqliteTableData {
  database: string;
  table: string;
  columns: string[];
  rows: string[][];
  total_rows: number;
}

interface CommandItem {
  id: string;
  label: string;
  shortcut?: string;
  run: () => void | Promise<void>;
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
const SOURCE_REF_RE = /\b([A-Za-z0-9_$]+\.(?:kt|java)):(\d+)\b/g;

const CHEVRON_RIGHT = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l4 4-4 4"/></svg>`;
const CHEVRON_DOWN = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6l4 4 4-4"/></svg>`;
const PIN_ICON = `<svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9.8 2.2 13.8 6.2l-1.5 1.5-1-.3-2.5 2.5.4 1.6-1.2 1.2-2.7-2.7-2.6 2.6-.7-.7 2.6-2.6-2.7-2.7 1.2-1.2 1.6.4 2.5-2.5-.3-1 1.5-1.5z"/></svg>`;

// DOM refs
const appEl = document.getElementById("app") as HTMLDivElement;
const deviceSelect = document.getElementById("device-select") as HTMLSelectElement;
const refreshDevicesBtn = document.getElementById("refresh-devices-btn") as HTMLButtonElement;
const scrcpyBtn = document.getElementById("scrcpy-btn") as HTMLButtonElement;
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

const cmdPaletteBtn = document.getElementById("cmd-palette-btn") as HTMLButtonElement;
const toggleLeftBtn = document.getElementById("toggle-left-btn") as HTMLButtonElement;
const toggleDockBtn = document.getElementById("toggle-dock-btn") as HTMLButtonElement;
const toggleRightBtn = document.getElementById("toggle-right-btn") as HTMLButtonElement;

const appearanceBtn = document.getElementById("appearance-btn") as HTMLButtonElement;
const appearancePopover = document.getElementById("appearance-popover") as HTMLDivElement;
const themeListEl = document.getElementById("theme-list") as HTMLDivElement;
const fontDecBtn = document.getElementById("font-dec-btn") as HTMLButtonElement;
const fontIncBtn = document.getElementById("font-inc-btn") as HTMLButtonElement;
const fontResetBtn = document.getElementById("font-reset-btn") as HTMLButtonElement;
const fontSizeLabel = document.getElementById("font-size-label") as HTMLSpanElement;
const resetLayoutBtn = document.getElementById("reset-layout-btn") as HTMLButtonElement;
const shortcutsOpenBtn = document.getElementById("shortcuts-open-btn") as HTMLButtonElement;

const banner = document.getElementById("banner") as HTMLDivElement;
const toastContainer = document.getElementById("toast-container") as HTMLDivElement;

const workspaceEl = document.getElementById("workspace") as HTMLElement;
const dockEl = document.getElementById("dock") as HTMLElement;
const splitDockEl = document.getElementById("split-dock") as HTMLDivElement;

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
const findBarEl = document.getElementById("editor-find-bar") as HTMLDivElement;
const findInput = document.getElementById("find-input") as HTMLInputElement;
const findCountEl = document.getElementById("find-count") as HTMLSpanElement;
const findCaseBtn = document.getElementById("find-case-btn") as HTMLButtonElement;
const findPrevBtn = document.getElementById("find-prev-btn") as HTMLButtonElement;
const findNextBtn = document.getElementById("find-next-btn") as HTMLButtonElement;
const findCloseBtn = document.getElementById("find-close-btn") as HTMLButtonElement;

const threadSelect = document.getElementById("thread-select") as HTMLSelectElement;
const dimSystemBtn = document.getElementById("dim-system-btn") as HTMLButtonElement;
const stackListEl = document.getElementById("stack-list") as HTMLDivElement;
const varsFilterInput = document.getElementById("vars-filter-input") as HTMLInputElement;
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

const dockExpandBtn = document.getElementById("dock-expand-btn") as HTMLButtonElement;
const layoutCountEl = document.getElementById("layout-count") as HTMLSpanElement;
const layoutFilterInput = document.getElementById("layout-filter-input") as HTMLInputElement;
const layoutCaptureBtn = document.getElementById("layout-capture-btn") as HTMLButtonElement;
const layoutStageEl = document.getElementById("layout-stage") as HTMLDivElement;
const layoutTreeEl = document.getElementById("layout-tree") as HTMLDivElement;
const layoutPropsEl = document.getElementById("layout-props") as HTMLDivElement;

const storageFilterInput = document.getElementById("storage-filter-input") as HTMLInputElement;
const storageRefreshBtn = document.getElementById("storage-refresh-btn") as HTMLButtonElement;
const storageSidebarEl = document.getElementById("storage-sidebar") as HTMLDivElement;
const storageSqlBar = document.getElementById("storage-sql-bar") as HTMLDivElement;
const storageSqlInput = document.getElementById("storage-sql-input") as HTMLInputElement;
const storageSqlRunBtn = document.getElementById("storage-sql-run-btn") as HTMLButtonElement;
const storageContentEl = document.getElementById("storage-content") as HTMLDivElement;

const quickOpenBackdrop = document.getElementById("quick-open-backdrop") as HTMLDivElement;
const quickOpenInput = document.getElementById("quick-open-input") as HTMLInputElement;
const quickOpenResults = document.getElementById("quick-open-results") as HTMLDivElement;

const cmdPaletteBackdrop = document.getElementById("cmd-palette-backdrop") as HTMLDivElement;
const cmdPaletteInput = document.getElementById("cmd-palette-input") as HTMLInputElement;
const cmdPaletteResults = document.getElementById("cmd-palette-results") as HTMLDivElement;

const gotoLineBackdrop = document.getElementById("goto-line-backdrop") as HTMLDivElement;
const gotoLineInput = document.getElementById("goto-line-input") as HTMLInputElement;

const shortcutsBackdrop = document.getElementById("shortcuts-backdrop") as HTMLDivElement;
const shortcutsCloseBtn = document.getElementById("shortcuts-close-btn") as HTMLButtonElement;

// Managers & State
const layout = new LayoutManager(appEl);
const appearance = new AppearanceManager(() => renderAppearancePopover());

let projectRoot: string | null = localStorage.getItem("android-lens:projectRoot");
let projectFiles: ProjectFile[] = [];
const expandedDirs = new Set<string>();
const dirCache = new Map<string, FsEntry[]>();

const openFiles = new Map<string, SourceFileContent>();
let activeFilePath: string | null = null;
let findCaseSensitive = false;

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

let lastDevicesSignature = "";
let breakpoints: BreakpointDto[] = [];
let selectedThreadId: string | null = null;
let selectedFrameIndex = 0;
let cachedFrames: StackFrameDto[] = [];
let cachedLocals: VariableDto[] = [];
let dimSystemFrames = true;
let execHighlight: { fileName: string; line: number } | null = null;

let logcatRunning = false;
let logEntries: LogEntry[] = [];
let crashGroups: CrashGroup[] = [];
let pinnedItems: PinnedItem[] = [];

let layoutSnapshot: LayoutSnapshot | null = null;
let flatUiNodes: UiNode[] = [];
let selectedUiNodeId: string | null = null;
let hoveredUiNodeId: string | null = null;

let storageIndex: AppStorageIndex | null = null;
const sqliteTablesCache = new Map<string, string[]>();
let activeStorageSelection:
  | { kind: "shared_prefs"; file: string; entries: PrefEntry[] }
  | { kind: "datastore"; file: string; entries: PrefEntry[] }
  | { kind: "sqlite"; db: string; table: string; data: SqliteTableData }
  | null = null;

let paletteMatches: ProjectFile[] = [];
let paletteIndex = 0;
let cmdMatches: CommandItem[] = [];
let cmdIndex = 0;

const viewer = new SourceViewer(editorContainer, async (fileName, line) => {
  try {
    const updated = await invoke<BreakpointDto[]>("debug_toggle_breakpoint", {
      file: fileName,
      line,
    });
    updateBreakpoints(updated);
    persistProjectBreakpoints();
  } catch (err) {
    showBanner(`Breakpoint error: ${String(err)}`);
  }
});

// --- Toasts & Banners ---

function showToast(text: string, variant: "default" | "ok" | "error" = "default") {
  const el = document.createElement("div");
  el.className = `toast ${variant}`;
  el.textContent = text;
  toastContainer.appendChild(el);
  setTimeout(() => {
    el.remove();
  }, 2400);
}

function showBanner(text: string) {
  banner.innerHTML = "";
  const msg = document.createElement("span");
  msg.textContent = text;
  const close = document.createElement("button");
  close.className = "btn btn-ghost-xs";
  close.textContent = "Dismiss";
  close.onclick = hideBanner;
  banner.className = "banner";
  banner.append(msg, close);
}

function hideBanner() {
  banner.classList.add("hidden");
}

// --- Appearance & Panel Toggles ---

function renderAppearancePopover() {
  const activeId = appearance.getTheme();
  themeListEl.innerHTML = "";
  for (const t of THEMES) {
    const btn = document.createElement("button");
    btn.className = "theme-option" + (t.id === activeId ? " active" : "");
    const swatches = t.swatches
      .map((c) => `<span class="theme-swatch" style="background:${c}"></span>`)
      .join("");
    btn.innerHTML = `<span>${escapeHtml(t.label)}</span><span class="theme-swatches">${swatches}</span>`;
    btn.onclick = () => {
      appearance.setTheme(t.id);
      showToast(`Theme: ${t.label}`);
    };
    themeListEl.appendChild(btn);
  }
  fontSizeLabel.textContent = `${appearance.getFontSize()}px`;
}

function toggleLeftPane(force?: boolean) {
  const collapsed = force ?? !workspaceEl.classList.contains("left-collapsed");
  workspaceEl.classList.toggle("left-collapsed", collapsed);
  toggleLeftBtn.classList.toggle("active", !collapsed);
  localStorage.setItem("android-lens:leftCollapsed", collapsed ? "1" : "0");
}

function toggleRightPane(force?: boolean) {
  const collapsed = force ?? !workspaceEl.classList.contains("right-collapsed");
  workspaceEl.classList.toggle("right-collapsed", collapsed);
  toggleRightBtn.classList.toggle("active", !collapsed);
  localStorage.setItem("android-lens:rightCollapsed", collapsed ? "1" : "0");
}

function toggleDock(force?: boolean) {
  const collapsed = force ?? !dockEl.classList.contains("collapsed");
  dockEl.classList.toggle("collapsed", collapsed);
  splitDockEl.classList.toggle("hidden", collapsed);
  toggleDockBtn.classList.toggle("active", !collapsed);
  localStorage.setItem("android-lens:dockCollapsed", collapsed ? "1" : "0");
}

function selectDockTab(tab: "logcat" | "crashes" | "breakpoints" | "pinned" | "layout" | "storage") {
  if (dockEl.classList.contains("collapsed")) {
    toggleDock(false);
  }
  document.querySelectorAll<HTMLButtonElement>(".dock-tab").forEach((b) => {
    b.classList.toggle("active", b.dataset.tab === tab);
  });
  document.querySelectorAll(".dock-panel").forEach((p) => p.classList.remove("active"));
  document.getElementById(`tab-${tab}`)?.classList.add("active");
  document.getElementById("logcat-controls")!.classList.toggle("hidden", tab !== "logcat");
  document.getElementById("pinned-controls")!.classList.toggle("hidden", tab !== "pinned");
  document.getElementById("layout-controls")!.classList.toggle("hidden", tab !== "layout");
  document.getElementById("storage-controls")!.classList.toggle("hidden", tab !== "storage");

  if (tab === "storage" && !storageIndex && packageInput.value.trim()) {
    void refreshAppStorage();
  }
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

// --- ADB Devices & Auto-Detection ---

async function refreshDevices(silent = false) {
  try {
    const devices = await invoke<AdbDevice[]>("adb_list_devices");
    const sig = devices.map((d) => `${d.serial}:${d.state}:${d.model ?? ""}`).join("|");
    if (silent && sig === lastDevicesSignature) return;
    lastDevicesSignature = sig;

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
    if (!silent) showBanner(`ADB error: ${String(err)}`);
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

// --- Project Folder, Persistent Breakpoints & File Tree ---

function bpStorageKey(): string | null {
  return projectRoot ? `android-lens:bps:${projectRoot}` : null;
}

function persistProjectBreakpoints() {
  const key = bpStorageKey();
  if (!key) return;
  const saved = breakpoints.map((b) => ({ file: b.file, line: b.line }));
  localStorage.setItem(key, JSON.stringify(saved));
}

async function restoreProjectBreakpoints() {
  const key = bpStorageKey();
  if (!key) return;
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return;
    const saved = JSON.parse(raw) as { file: string; line: number }[];
    for (const item of saved) {
      const exists = breakpoints.some(
        (b) => b.file.toLowerCase() === item.file.toLowerCase() && b.line === item.line,
      );
      if (!exists) {
        const updated = await invoke<BreakpointDto[]>("debug_toggle_breakpoint", {
          file: item.file,
          line: item.line,
        });
        updateBreakpoints(updated);
      }
    }
  } catch {
    // Ignore invalid saved breakpoints
  }
}

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
  await restoreProjectBreakpoints();

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
      showToast("Project folder opened", "ok");
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

// --- ⌘P Quick Open, ⌘K Command Palette, ⌃G Go to Line, ⌘/ Shortcuts ---

function closeAllModals() {
  quickOpenBackdrop.classList.add("hidden");
  cmdPaletteBackdrop.classList.add("hidden");
  gotoLineBackdrop.classList.add("hidden");
  shortcutsBackdrop.classList.add("hidden");
}

function openQuickOpen() {
  if (!projectRoot) {
    void pickFolder();
    return;
  }
  closeAllModals();
  quickOpenBackdrop.classList.remove("hidden");
  quickOpenInput.value = "";
  paletteIndex = 0;
  renderPaletteResults();
  quickOpenInput.focus();
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
      closeAllModals();
      void openSourceFile(f.abs_path);
    };
    quickOpenResults.appendChild(item);
  });
}

function getCommands(): CommandItem[] {
  const cmds: CommandItem[] = [
    { id: "open-folder", label: "Open Project Folder…", run: () => pickFolder() },
    { id: "quick-open", label: "Quick Open File…", shortcut: "⌘P", run: () => openQuickOpen() },
    { id: "find", label: "Find in Current File", shortcut: "⌘F", run: () => openFindBar() },
    { id: "goto-line", label: "Go to Line…", shortcut: "⌃G", run: () => openGotoLine() },
    { id: "debug-auto", label: "Debug App (Auto Attach / Launch)", run: () => startSession("auto") },
    { id: "debug-launch", label: "Fresh Launch & Wait for Debugger", run: () => startSession("launch") },
    { id: "refresh-adb", label: "Refresh ADB Devices & Packages", run: () => refreshDevices() },
    { id: "scrcpy", label: "Mirror Device with scrcpy", run: () => launchScrcpyMirror() },
    {
      id: "capture-layout",
      label: "Capture Screen & View Hierarchy",
      run: () => {
        selectDockTab("layout");
        return captureLayout();
      },
    },
    {
      id: "refresh-storage",
      label: "Inspect App Storage (Prefs / DataStore / SQLite)",
      run: () => {
        selectDockTab("storage");
        return refreshAppStorage();
      },
    },
    { id: "toggle-left", label: "Toggle Files Rail", shortcut: "⌘B", run: () => toggleLeftPane() },
    { id: "toggle-dock", label: "Toggle Bottom Dock", shortcut: "⌘J", run: () => toggleDock() },
    { id: "toggle-right", label: "Toggle Right Inspector", shortcut: "⌘⌥B", run: () => toggleRightPane() },
    {
      id: "reset-layout",
      label: "Reset Pane Sizes to Default",
      run: () => {
        layout.reset();
        showToast("Pane sizes reset", "ok");
      },
    },
    {
      id: "font-inc",
      label: "Increase Code Font Size",
      shortcut: "⌘+",
      run: () => appearance.adjustFontSize(1),
    },
    {
      id: "font-dec",
      label: "Decrease Code Font Size",
      shortcut: "⌘−",
      run: () => appearance.adjustFontSize(-1),
    },
    {
      id: "copy-mcp",
      label: "Copy MCP Config for Claude Code / Antigravity",
      run: () => copyMcpConfig(),
    },
    { id: "shortcuts", label: "Show Keyboard Shortcuts", shortcut: "⌘/", run: () => openShortcuts() },
  ];

  for (const t of THEMES) {
    cmds.push({
      id: `theme-${t.id}`,
      label: `Theme: ${t.label}`,
      run: () => {
        appearance.setTheme(t.id as ThemeId);
        showToast(`Theme: ${t.label}`);
      },
    });
  }
  return cmds;
}

function openCommandPalette() {
  closeAllModals();
  cmdPaletteBackdrop.classList.remove("hidden");
  cmdPaletteInput.value = "";
  cmdIndex = 0;
  renderCommandResults();
  cmdPaletteInput.focus();
}

function renderCommandResults() {
  const q = cmdPaletteInput.value.trim().toLowerCase();
  const all = getCommands();
  cmdMatches = q ? all.filter((c) => c.label.toLowerCase().includes(q)) : all;
  if (cmdIndex >= cmdMatches.length) cmdIndex = 0;
  cmdPaletteResults.innerHTML = "";

  if (cmdMatches.length === 0) {
    cmdPaletteResults.innerHTML = `<div class="empty-hint">No matching commands</div>`;
    return;
  }

  cmdMatches.forEach((c, idx) => {
    const row = document.createElement("div");
    row.className = "palette-item" + (idx === cmdIndex ? " selected" : "");
    row.innerHTML = `<span class="palette-name">${escapeHtml(c.label)}</span>${c.shortcut ? `<kbd class="kbd">${escapeHtml(c.shortcut)}</kbd>` : ""}`;
    row.onclick = () => {
      closeAllModals();
      void c.run();
    };
    cmdPaletteResults.appendChild(row);
  });
}

function openGotoLine() {
  if (!viewer.hasDocument()) {
    showToast("Open a source file first");
    return;
  }
  closeAllModals();
  gotoLineBackdrop.classList.remove("hidden");
  gotoLineInput.max = String(viewer.getLineCount());
  gotoLineInput.placeholder = `Line 1 – ${viewer.getLineCount()}…`;
  gotoLineInput.value = "";
  gotoLineInput.focus();
}

function openShortcuts() {
  closeAllModals();
  shortcutsBackdrop.classList.remove("hidden");
}

// --- In-File Find (⌘F) ---

function openFindBar() {
  if (!viewer.hasDocument()) return;
  findBarEl.classList.remove("hidden");
  findInput.focus();
  findInput.select();
  runFind();
}

function closeFindBar() {
  findBarEl.classList.add("hidden");
  viewer.clearFind();
  findCountEl.textContent = "0 / 0";
}

function runFind() {
  const st = viewer.find(findInput.value, findCaseSensitive);
  findCountEl.textContent = st.count > 0 ? `${st.index + 1} / ${st.count}` : "0 / 0";
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
    if (!findBarEl.classList.contains("hidden") && findInput.value) {
      runFind();
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
  if (!projectRoot) {
    showToast(`Open project folder to jump to ${fileName}:${line}`);
    return;
  }
  const found = await invoke<string | null>("fs_find_source_file", {
    projectRoot,
    fileName,
    classHint: classHint ?? null,
  });
  if (found) {
    await openSourceFile(found, line);
  } else {
    showToast(`${fileName} not found in project folder`);
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
          closeFindBar();
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
    cachedLocals = [];
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

function renderVariablesList() {
  varsTreeEl.innerHTML = "";
  const filter = varsFilterInput.value.trim().toLowerCase();
  const visible = filter
    ? cachedLocals.filter(
        (v) =>
          v.name.toLowerCase().includes(filter) ||
          v.value.toLowerCase().includes(filter) ||
          v.type_name.toLowerCase().includes(filter),
      )
    : cachedLocals;

  if (visible.length === 0) {
    varsTreeEl.innerHTML = `<div class="empty-hint">${cachedLocals.length === 0 ? "No local variables in scope" : "No variables match filter"}</div>`;
    return;
  }
  for (const v of visible) {
    varsTreeEl.appendChild(createVarNode(v, 0));
  }
}

async function refreshLocals(
  threadIdHex: string | undefined,
  frameIndex: number,
  frame?: StackFrameDto,
) {
  varsFrameLabelEl.textContent = frame ? `${frame.method_name}()` : `#${frameIndex}`;
  try {
    cachedLocals = await invoke<VariableDto[]>("debug_get_locals", {
      threadIdHex: threadIdHex ?? null,
      frameIndex,
    });
    renderVariablesList();
  } catch (err) {
    varsTreeEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

function formatReadableValue(v: VariableDto): string {
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

  const actions = document.createElement("span");
  actions.className = "var-actions";

  const copyBtn = document.createElement("button");
  copyBtn.className = "copy-btn";
  copyBtn.title = "Copy raw value";
  copyBtn.textContent = "Copy";
  copyBtn.onclick = async (e) => {
    e.stopPropagation();
    const raw =
      v.value.startsWith('"') && v.value.endsWith('"') ? v.value.slice(1, -1) : v.value;
    try {
      await navigator.clipboard.writeText(raw);
      showToast(`Copied ${v.name}`, "ok");
    } catch {
      showToast("Copy failed", "error");
    }
  };

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
  actions.append(copyBtn, pinBtn);
  row.appendChild(actions);

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
      persistProjectBreakpoints();
    };

    row.append(left, rm);
    breakpointsListEl.appendChild(row);
  }
}

// --- Logcat & Crashes (with clickable Source.kt:line links!) ---

function linkifySourceRefs(text: string): DocumentFragment {
  const frag = document.createDocumentFragment();
  let lastIdx = 0;
  SOURCE_REF_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = SOURCE_REF_RE.exec(text)) !== null) {
    if (m.index > lastIdx) {
      frag.appendChild(document.createTextNode(text.slice(lastIdx, m.index)));
    }
    const fileName = m[1];
    const lineNo = Number(m[2]);
    const a = document.createElement("a");
    a.className = "stack-link";
    a.textContent = m[0];
    a.title = `Jump to ${fileName}:${lineNo}`;
    a.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      void openBySourceName(fileName, lineNo);
    };
    frag.appendChild(a);
    lastIdx = m.index + m[0].length;
  }
  if (lastIdx < text.length) {
    frag.appendChild(document.createTextNode(text.slice(lastIdx)));
  }
  return frag;
}

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

  const ts = document.createElement("span");
  ts.className = "log-ts";
  ts.textContent = entry.timestamp;

  const lvl = document.createElement("span");
  lvl.className = `log-lvl ${entry.level}`;
  lvl.textContent = letter;

  const tag = document.createElement("span");
  tag.className = "log-tag";
  tag.title = entry.tag;
  tag.textContent = entry.tag;

  const msg = document.createElement("span");
  msg.appendChild(linkifySourceRefs(entry.message));

  const pin = makePinButton("Pin logcat entry to AI agent", () =>
    pinContext({
      kind: "logcat_entry",
      title: `${letter}/${entry.tag || "log"}`,
      summary: `[${entry.timestamp}] ${entry.message}`,
      payload: entry,
    }),
  );

  row.append(ts, lvl, tag, msg, pin);
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
    body.appendChild(linkifySourceRefs(crash.lines.join("\n")));

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
    showToast(`Pinned "${req.title}" to Agent`, "ok");
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
    ui_node: "UI",
    storage_entry: "Data",
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

async function copyMcpConfig() {
  await refreshMcpStatus();
  try {
    await navigator.clipboard.writeText(mcpSnippet.textContent ?? "");
    showToast("MCP config copied", "ok");
  } catch {
    showToast("Select and copy config manually", "error");
  }
}

// --- Phase 3: scrcpy, Visual Layout Inspector, and Storage Browser ---

async function launchScrcpyMirror() {
  hideBanner();
  try {
    const msg = await invoke<string>("inspector_launch_scrcpy", {
      serial: deviceSelect.value || null,
    });
    showToast(msg, "ok");
  } catch (err) {
    showBanner(String(err));
  }
}

function flattenUiNodes(nodes: UiNode[], out: UiNode[] = []): UiNode[] {
  for (const n of nodes) {
    out.push(n);
    flattenUiNodes(n.children, out);
  }
  return out;
}

async function captureLayout() {
  hideBanner();
  layoutCaptureBtn.disabled = true;
  layoutCaptureBtn.textContent = "Capturing…";
  try {
    const snap = await invoke<LayoutSnapshot>("inspector_capture_layout", {
      serial: deviceSelect.value || null,
    });
    layoutSnapshot = snap;
    flatUiNodes = flattenUiNodes(snap.roots);
    layoutCountEl.textContent = String(snap.node_count);
    selectedUiNodeId = flatUiNodes[0]?.id ?? null;
    hoveredUiNodeId = null;
    renderLayoutInspector();
  } catch (err) {
    showBanner(`Layout capture failed: ${String(err)}`);
  } finally {
    layoutCaptureBtn.disabled = false;
    layoutCaptureBtn.textContent = "Capture Screen";
  }
}

function renderLayoutInspector() {
  if (!layoutSnapshot) return;
  const snap = layoutSnapshot;

  layoutStageEl.innerHTML = "";
  const wrap = document.createElement("div");
  wrap.className = "layout-shot-wrap";

  const img = document.createElement("img");
  img.className = "layout-shot-img";
  const dockHeight = dockEl.getBoundingClientRect().height || 224;
  img.style.maxHeight = `${Math.max(120, Math.round(dockHeight - 54))}px`;
  img.src = snap.screenshot_data_url;

  const svgNS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNS, "svg");
  svg.setAttribute("class", "layout-svg-overlay");
  svg.setAttribute("viewBox", `0 0 ${snap.screen_width} ${snap.screen_height}`);

  for (const node of flatUiNodes) {
    const w = node.bounds.right - node.bounds.left;
    const h = node.bounds.bottom - node.bounds.top;
    if (w <= 0 || h <= 0) continue;
    const rect = document.createElementNS(svgNS, "rect");
    rect.setAttribute("x", String(node.bounds.left));
    rect.setAttribute("y", String(node.bounds.top));
    rect.setAttribute("width", String(w));
    rect.setAttribute("height", String(h));
    rect.setAttribute("data-id", node.id);
    let cls = "ui-rect";
    if (node.id === selectedUiNodeId) cls += " selected";
    else if (node.id === hoveredUiNodeId) cls += " hovered";
    rect.setAttribute("class", cls);
    svg.appendChild(rect);
  }

  const hitTest = (e: MouseEvent): UiNode | null => {
    const rect = svg.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    const x = ((e.clientX - rect.left) / rect.width) * snap.screen_width;
    const y = ((e.clientY - rect.top) / rect.height) * snap.screen_height;
    let best: UiNode | null = null;
    let bestArea = Infinity;
    for (const n of flatUiNodes) {
      if (x >= n.bounds.left && x <= n.bounds.right && y >= n.bounds.top && y <= n.bounds.bottom) {
        const area = Math.max(1, (n.bounds.right - n.bounds.left) * (n.bounds.bottom - n.bounds.top));
        if (area <= bestArea) {
          bestArea = area;
          best = n;
        }
      }
    }
    return best;
  };

  svg.onmousemove = (e) => {
    const hit = hitTest(e);
    const nextHover = hit?.id ?? null;
    if (nextHover !== hoveredUiNodeId) {
      hoveredUiNodeId = nextHover;
      updateOverlayHighlights(svg);
    }
  };
  svg.onmouseleave = () => {
    if (hoveredUiNodeId !== null) {
      hoveredUiNodeId = null;
      updateOverlayHighlights(svg);
    }
  };
  svg.onclick = (e) => {
    const hit = hitTest(e);
    if (hit) {
      selectedUiNodeId = hit.id;
      updateOverlayHighlights(svg);
      renderLayoutTree();
      renderLayoutProps(hit);
    }
  };

  wrap.append(img, svg);
  layoutStageEl.appendChild(wrap);

  renderLayoutTree();
  const sel = flatUiNodes.find((n) => n.id === selectedUiNodeId) ?? null;
  if (sel) renderLayoutProps(sel);
}

function updateOverlayHighlights(svg: SVGSVGElement) {
  svg.querySelectorAll<SVGRectElement>(".ui-rect").forEach((r) => {
    const id = r.getAttribute("data-id");
    let cls = "ui-rect";
    if (id === selectedUiNodeId) cls += " selected";
    else if (id === hoveredUiNodeId) cls += " hovered";
    r.setAttribute("class", cls);
  });
}

function renderLayoutTree() {
  layoutTreeEl.innerHTML = "";
  if (!layoutSnapshot) return;
  const filter = layoutFilterInput.value.trim().toLowerCase();

  const walk = (nodes: UiNode[], depth: number) => {
    for (const n of nodes) {
      const shortCls = n.class_name.split(".").pop() ?? n.class_name;
      const shortRes = n.resource_id.includes("/")
        ? n.resource_id.split("/").pop() ?? ""
        : n.resource_id;
      const matches =
        !filter ||
        shortCls.toLowerCase().includes(filter) ||
        shortRes.toLowerCase().includes(filter) ||
        n.text.toLowerCase().includes(filter) ||
        n.content_desc.toLowerCase().includes(filter);

      if (matches) {
        const row = document.createElement("div");
        row.className = "ui-tree-row" + (n.id === selectedUiNodeId ? " selected" : "");
        row.style.paddingLeft = `${8 + depth * 12}px`;
        let html = `<span class="ui-cls">${escapeHtml(shortCls)}</span>`;
        if (shortRes) html += ` <span class="ui-res">#${escapeHtml(shortRes)}</span>`;
        if (n.text) html += ` <span class="ui-txt">"${escapeHtml(n.text.slice(0, 32))}"</span>`;
        row.innerHTML = html;
        row.onclick = () => {
          selectedUiNodeId = n.id;
          renderLayoutTree();
          renderLayoutProps(n);
          const svg = layoutStageEl.querySelector<SVGSVGElement>(".layout-svg-overlay");
          if (svg) updateOverlayHighlights(svg);
        };
        layoutTreeEl.appendChild(row);
      }
      walk(n.children, depth + 1);
    }
  };
  walk(layoutSnapshot.roots, 0);
}

function renderLayoutProps(node: UiNode) {
  layoutPropsEl.innerHTML = "";
  const shortCls = node.class_name.split(".").pop() ?? node.class_name;
  const head = document.createElement("div");
  head.className = "prop-head";
  const title = document.createElement("span");
  title.textContent = shortCls;
  const pin = makePinButton("Pin UI node to AI agent", () =>
    pinContext({
      kind: "ui_node",
      title: node.resource_id || shortCls,
      summary: `${shortCls} [${node.bounds.left},${node.bounds.top}][${node.bounds.right},${node.bounds.bottom}] ${node.text ? `"${node.text}"` : ""}`.trim(),
      payload: { ...node, children: undefined },
    }),
  );
  pin.style.opacity = "1";
  head.append(title, pin);
  layoutPropsEl.appendChild(head);

  const w = node.bounds.right - node.bounds.left;
  const h = node.bounds.bottom - node.bounds.top;
  const rows: [string, string][] = [
    ["class", node.class_name],
    ["resource-id", node.resource_id || "—"],
    ["text", node.text || "—"],
    ["content-desc", node.content_desc || "—"],
    ["bounds", `[${node.bounds.left}, ${node.bounds.top}] → [${node.bounds.right}, ${node.bounds.bottom}]`],
    ["size", `${w} × ${h} px`],
    [
      "flags",
      [
        node.clickable && "clickable",
        node.focused && "focused",
        node.scrollable && "scrollable",
        node.selected && "selected",
        !node.enabled && "disabled",
      ]
        .filter(Boolean)
        .join(", ") || "—",
    ],
  ];

  for (const [k, v] of rows) {
    const r = document.createElement("div");
    r.className = "prop-row";
    r.innerHTML = `<span class="prop-k">${escapeHtml(k)}</span><span class="prop-v">${escapeHtml(v)}</span>`;
    layoutPropsEl.appendChild(r);
  }
}

async function refreshAppStorage() {
  const pkg = packageInput.value.trim();
  if (!pkg) {
    showToast("Select a debuggable package first", "error");
    return;
  }
  hideBanner();
  storageSidebarEl.innerHTML = `<div class="empty-hint">Loading storage…</div>`;
  try {
    storageIndex = await invoke<AppStorageIndex>("inspector_list_storage", {
      serial: deviceSelect.value || null,
      package: pkg,
    });
    sqliteTablesCache.clear();
    renderStorageSidebar();
  } catch (err) {
    storageSidebarEl.innerHTML = `<div class="empty-hint">${escapeHtml(String(err))}</div>`;
  }
}

function renderStorageSidebar() {
  storageSidebarEl.innerHTML = "";
  if (!storageIndex) return;
  const idx = storageIndex;
  const total = idx.shared_prefs.length + idx.datastores.length + idx.databases.length;
  if (total === 0) {
    storageSidebarEl.innerHTML = `<div class="empty-hint">No SharedPreferences, DataStore, or SQLite files found for ${escapeHtml(idx.package)}</div>`;
    return;
  }

  if (idx.shared_prefs.length > 0) {
    const lbl = document.createElement("div");
    lbl.className = "storage-group-label";
    lbl.textContent = "SharedPreferences";
    storageSidebarEl.appendChild(lbl);
    for (const file of idx.shared_prefs) {
      const row = document.createElement("div");
      row.className =
        "storage-item" +
        (activeStorageSelection?.kind === "shared_prefs" && activeStorageSelection.file === file
          ? " active"
          : "");
      row.textContent = file;
      row.onclick = () => void openSharedPrefs(file);
      storageSidebarEl.appendChild(row);
    }
  }

  if (idx.datastores.length > 0) {
    const lbl = document.createElement("div");
    lbl.className = "storage-group-label";
    lbl.textContent = "DataStore";
    storageSidebarEl.appendChild(lbl);
    for (const file of idx.datastores) {
      const row = document.createElement("div");
      row.className =
        "storage-item" +
        (activeStorageSelection?.kind === "datastore" && activeStorageSelection.file === file
          ? " active"
          : "");
      row.textContent = file;
      row.onclick = () => void openDatastore(file);
      storageSidebarEl.appendChild(row);
    }
  }

  if (idx.databases.length > 0) {
    const lbl = document.createElement("div");
    lbl.className = "storage-group-label";
    lbl.textContent = "SQLite Databases";
    storageSidebarEl.appendChild(lbl);
    for (const db of idx.databases) {
      const dbRow = document.createElement("div");
      dbRow.className = "storage-item";
      const expanded = sqliteTablesCache.has(db);
      dbRow.innerHTML = `${expanded ? CHEVRON_DOWN : CHEVRON_RIGHT}<span>${escapeHtml(db)}</span>`;
      dbRow.onclick = async () => {
        if (sqliteTablesCache.has(db)) {
          sqliteTablesCache.delete(db);
          renderStorageSidebar();
        } else {
          try {
            const tables = await invoke<string[]>("inspector_list_sqlite_tables", {
              serial: deviceSelect.value || null,
              package: idx.package,
              dbName: db,
            });
            sqliteTablesCache.set(db, tables);
            renderStorageSidebar();
          } catch (err) {
            showBanner(String(err));
          }
        }
      };
      storageSidebarEl.appendChild(dbRow);

      const tables = sqliteTablesCache.get(db);
      if (tables) {
        for (const tbl of tables) {
          const tRow = document.createElement("div");
          tRow.className =
            "storage-item storage-table-item" +
            (activeStorageSelection?.kind === "sqlite" &&
            activeStorageSelection.db === db &&
            activeStorageSelection.table === tbl
              ? " active"
              : "");
          tRow.textContent = tbl;
          tRow.onclick = () => void openSqliteTable(db, tbl, false);
          storageSidebarEl.appendChild(tRow);
        }
      }
    }
  }
}

async function openSharedPrefs(file: string) {
  if (!storageIndex) return;
  try {
    const entries = await invoke<PrefEntry[]>("inspector_read_shared_prefs", {
      serial: deviceSelect.value || null,
      package: storageIndex.package,
      fileName: file,
    });
    activeStorageSelection = { kind: "shared_prefs", file, entries };
    renderStorageSidebar();
    renderStorageContent();
  } catch (err) {
    showBanner(String(err));
  }
}

async function openDatastore(file: string) {
  if (!storageIndex) return;
  try {
    const entries = await invoke<PrefEntry[]>("inspector_read_datastore", {
      serial: deviceSelect.value || null,
      package: storageIndex.package,
      fileName: file,
    });
    activeStorageSelection = { kind: "datastore", file, entries };
    renderStorageSidebar();
    renderStorageContent();
  } catch (err) {
    showBanner(String(err));
  }
}

async function openSqliteTable(db: string, tableOrSql: string, isCustomSql: boolean) {
  if (!storageIndex) return;
  try {
    const data = await invoke<SqliteTableData>("inspector_query_sqlite", {
      serial: deviceSelect.value || null,
      package: storageIndex.package,
      dbName: db,
      tableOrSql,
      isCustomSql,
      limit: 100,
    });
    activeStorageSelection = { kind: "sqlite", db, table: data.table, data };
    if (!isCustomSql) {
      storageSqlInput.value = `SELECT * FROM "${tableOrSql}" LIMIT 100;`;
    }
    renderStorageSidebar();
    renderStorageContent();
  } catch (err) {
    showBanner(String(err));
  }
}

function renderStorageContent() {
  storageContentEl.innerHTML = "";
  const sel = activeStorageSelection;
  if (!sel) {
    storageSqlBar.classList.add("hidden");
    storageContentEl.innerHTML = `<div class="empty-hint">Select a preferences file or SQLite table</div>`;
    return;
  }

  const filter = storageFilterInput.value.trim().toLowerCase();

  if (sel.kind === "shared_prefs" || sel.kind === "datastore") {
    storageSqlBar.classList.add("hidden");
    const rows = filter
      ? sel.entries.filter(
          (e) =>
            e.key.toLowerCase().includes(filter) || e.value.toLowerCase().includes(filter),
        )
      : sel.entries;

    const table = document.createElement("table");
    table.className = "data-table";
    table.innerHTML = `<thead><tr><th>Key</th><th>Type</th><th>Value</th><th></th></tr></thead>`;
    const tbody = document.createElement("tbody");
    for (const e of rows) {
      const tr = document.createElement("tr");
      tr.innerHTML = `<td>${escapeHtml(e.key)}</td><td class="muted">${escapeHtml(e.value_type)}</td><td>${escapeHtml(e.value)}</td>`;
      const tdPin = document.createElement("td");
      tdPin.appendChild(
        makePinButton("Pin preference entry to AI agent", () =>
          pinContext({
            kind: "storage_entry",
            title: `${sel.file}:${e.key}`,
            summary: `${e.key} (${e.value_type}) = ${e.value}`,
            payload: { source: sel.kind, file: sel.file, ...e },
          }),
        ),
      );
      tr.appendChild(tdPin);
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    storageContentEl.appendChild(table);
    return;
  }

  storageSqlBar.classList.remove("hidden");
  const { columns, rows } = sel.data;
  const visible = filter
    ? rows.filter((r) => r.some((cell) => cell.toLowerCase().includes(filter)))
    : rows;

  const table = document.createElement("table");
  table.className = "data-table";
  const thead = document.createElement("thead");
  const headTr = document.createElement("tr");
  for (const col of columns) {
    const th = document.createElement("th");
    th.textContent = col;
    headTr.appendChild(th);
  }
  thead.appendChild(headTr);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  for (const r of visible) {
    const tr = document.createElement("tr");
    for (const cell of r) {
      const td = document.createElement("td");
      td.textContent = cell;
      tr.appendChild(td);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  storageContentEl.appendChild(table);
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
scrcpyBtn.onclick = () => void launchScrcpyMirror();
deviceSelect.onchange = () => void refreshPackages();
packageInput.oninput = () => updateGuideChecklist();

debugAutoBtn.onclick = () => void startSession("auto");
debugMenuBtn.onclick = (e) => {
  e.stopPropagation();
  mcpPopover.classList.add("hidden");
  appearancePopover.classList.add("hidden");
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
  appearancePopover.classList.add("hidden");
});

mcpPillBtn.onclick = async (e) => {
  e.stopPropagation();
  debugMenu.classList.add("hidden");
  appearancePopover.classList.add("hidden");
  await refreshMcpStatus();
  mcpPopover.classList.toggle("hidden");
};
mcpPopover.onclick = (e) => e.stopPropagation();
mcpCopyBtn.onclick = () => void copyMcpConfig();

appearanceBtn.onclick = (e) => {
  e.stopPropagation();
  debugMenu.classList.add("hidden");
  mcpPopover.classList.add("hidden");
  renderAppearancePopover();
  appearancePopover.classList.toggle("hidden");
};
appearancePopover.onclick = (e) => e.stopPropagation();
fontDecBtn.onclick = () => appearance.adjustFontSize(-1);
fontIncBtn.onclick = () => appearance.adjustFontSize(1);
fontResetBtn.onclick = () => appearance.resetFontSize();
resetLayoutBtn.onclick = () => {
  layout.reset();
  appearancePopover.classList.add("hidden");
  showToast("Pane sizes reset", "ok");
};
shortcutsOpenBtn.onclick = () => {
  appearancePopover.classList.add("hidden");
  openShortcuts();
};

cmdPaletteBtn.onclick = () => openCommandPalette();

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

varsFilterInput.oninput = () => renderVariablesList();

openFolderBtn.onclick = () => void pickFolder();
guideOpenBtn.onclick = () => void pickFolder();
quickOpenBtn.onclick = () => openQuickOpen();
treeFilterInput.oninput = () => renderFileTree();

toggleLeftBtn.onclick = () => toggleLeftPane();
toggleRightBtn.onclick = () => toggleRightPane();
toggleDockBtn.onclick = () => toggleDock();

// Find Bar events
findInput.oninput = () => runFind();
findInput.onkeydown = (e) => {
  if (e.key === "Escape") {
    closeFindBar();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const st = viewer.stepFind(e.shiftKey ? -1 : 1);
    findCountEl.textContent = st.count > 0 ? `${st.index + 1} / ${st.count}` : "0 / 0";
  }
};
findCaseBtn.onclick = () => {
  findCaseSensitive = !findCaseSensitive;
  findCaseBtn.classList.toggle("active", findCaseSensitive);
  runFind();
};
findPrevBtn.onclick = () => {
  const st = viewer.stepFind(-1);
  findCountEl.textContent = st.count > 0 ? `${st.index + 1} / ${st.count}` : "0 / 0";
};
findNextBtn.onclick = () => {
  const st = viewer.stepFind(1);
  findCountEl.textContent = st.count > 0 ? `${st.index + 1} / ${st.count}` : "0 / 0";
};
findCloseBtn.onclick = () => closeFindBar();

// Quick Open events
quickOpenBackdrop.onclick = (e) => {
  if (e.target === quickOpenBackdrop) closeAllModals();
};
quickOpenInput.oninput = () => {
  paletteIndex = 0;
  renderPaletteResults();
};
quickOpenInput.onkeydown = (e) => {
  if (e.key === "Escape") {
    closeAllModals();
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
      closeAllModals();
      void openSourceFile(chosen.abs_path);
    }
  }
};

// Command Palette events
cmdPaletteBackdrop.onclick = (e) => {
  if (e.target === cmdPaletteBackdrop) closeAllModals();
};
cmdPaletteInput.oninput = () => {
  cmdIndex = 0;
  renderCommandResults();
};
cmdPaletteInput.onkeydown = (e) => {
  if (e.key === "Escape") {
    closeAllModals();
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    if (cmdMatches.length > 0) {
      cmdIndex = (cmdIndex + 1) % cmdMatches.length;
      renderCommandResults();
    }
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (cmdMatches.length > 0) {
      cmdIndex = (cmdIndex - 1 + cmdMatches.length) % cmdMatches.length;
      renderCommandResults();
    }
  } else if (e.key === "Enter") {
    e.preventDefault();
    const chosen = cmdMatches[cmdIndex];
    if (chosen) {
      closeAllModals();
      void chosen.run();
    }
  }
};

// Go to Line events
gotoLineBackdrop.onclick = (e) => {
  if (e.target === gotoLineBackdrop) closeAllModals();
};
gotoLineInput.onkeydown = (e) => {
  if (e.key === "Escape") {
    closeAllModals();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const ln = Number(gotoLineInput.value);
    if (Number.isFinite(ln) && ln >= 1) {
      viewer.scrollToLine(ln);
    }
    closeAllModals();
  }
};

// Shortcuts Modal events
shortcutsBackdrop.onclick = (e) => {
  if (e.target === shortcutsBackdrop) closeAllModals();
};
shortcutsCloseBtn.onclick = () => closeAllModals();

// Global Keyboard Shortcuts
window.addEventListener("keydown", (e) => {
  const mod = e.metaKey || e.ctrlKey;
  if (e.key === "Escape") {
    closeAllModals();
    return;
  }
  if (mod && e.key.toLowerCase() === "k") {
    e.preventDefault();
    openCommandPalette();
    return;
  }
  if (mod && e.key.toLowerCase() === "p") {
    e.preventDefault();
    openQuickOpen();
    return;
  }
  if (mod && e.key.toLowerCase() === "f") {
    e.preventDefault();
    openFindBar();
    return;
  }
  if (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "g") {
    e.preventDefault();
    openGotoLine();
    return;
  }
  if (mod && e.key === "/") {
    e.preventDefault();
    openShortcuts();
    return;
  }
  if (mod && e.altKey && e.key.toLowerCase() === "b") {
    e.preventDefault();
    toggleRightPane();
    return;
  }
  if (mod && !e.altKey && e.key.toLowerCase() === "b") {
    e.preventDefault();
    toggleLeftPane();
    return;
  }
  if (mod && e.key.toLowerCase() === "j") {
    e.preventDefault();
    toggleDock();
    return;
  }
  if (mod && (e.key === "=" || e.key === "+")) {
    e.preventDefault();
    appearance.adjustFontSize(1);
    return;
  }
  if (mod && e.key === "-") {
    e.preventDefault();
    appearance.adjustFontSize(-1);
    return;
  }
  if (mod && e.key === "0") {
    e.preventDefault();
    appearance.resetFontSize();
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
    const tab = btn.dataset.tab as
      | "logcat"
      | "crashes"
      | "breakpoints"
      | "pinned"
      | "layout"
      | "storage";
    selectDockTab(tab);
  };
});

dockExpandBtn.onclick = () => {
  const expanded = dockEl.classList.toggle("expanded");
  dockExpandBtn.textContent = expanded ? "Restore" : "Expand";
  if (layoutSnapshot) {
    renderLayoutInspector();
  }
};

layoutCaptureBtn.onclick = () => void captureLayout();
layoutFilterInput.oninput = () => renderLayoutTree();
storageRefreshBtn.onclick = () => void refreshAppStorage();
storageFilterInput.oninput = () => renderStorageContent();
storageSqlRunBtn.onclick = () => {
  if (activeStorageSelection?.kind === "sqlite") {
    void openSqliteTable(activeStorageSelection.db, storageSqlInput.value, true);
  }
};
storageSqlInput.onkeydown = (e) => {
  if (e.key === "Enter" && activeStorageSelection?.kind === "sqlite") {
    void openSqliteTable(activeStorageSelection.db, storageSqlInput.value, true);
  }
};

logcatLevelSelect.onchange = () => renderAllLogs();
logcatTagInput.oninput = () => renderAllLogs();
logcatQueryInput.oninput = () => renderAllLogs();

logcatToggleBtn.onclick = async () => {
  if (logcatRunning) {
    await stopLogcat();
  } else {
    const pkg = packageInput.value.trim();
    if (!pkg) {
      showToast("Select a package first to start logcat", "error");
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
  renderAppearancePopover();
  layout.bindAll(() => {
    if (layoutSnapshot) renderLayoutInspector();
  });

  if (localStorage.getItem("android-lens:leftCollapsed") === "1") toggleLeftPane(true);
  if (localStorage.getItem("android-lens:rightCollapsed") === "1") toggleRightPane(true);
  if (localStorage.getItem("android-lens:dockCollapsed") === "1") toggleDock(true);

  await listen<DebugUiEvent>("debug://event", async (ev) => {
    const payload = ev.payload;
    switch (payload.kind) {
      case "status_changed":
        applyStatus(payload.status);
        break;
      case "breakpoints_changed":
        updateBreakpoints(payload.breakpoints);
        persistProjectBreakpoints();
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
        showToast("Target app process exited");
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

  // Gentle ADB device auto-detection while disconnected and window focused
  setInterval(() => {
    if (currentStatus.state === "disconnected" && document.hasFocus()) {
      void refreshDevices(true);
    }
  }, 4000);
}

void init();
