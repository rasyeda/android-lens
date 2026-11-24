#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod files;

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

use adb_bridge::AdbDevice;
use app_inspector::storage::{AppStorageIndex, PrefEntry, SqliteTableData};
use app_inspector::uiautomator::LayoutSnapshot;
use jdwp_client::StepDepth;
use logcat_stream::{CrashGroup, LogEntry, LogLevel};
use mcp_server::http::spawn_http_mcp_server;
use mcp_server::pins::{NewPinRequest, PinnedItem};
use mcp_server::session::{BreakpointDto, SessionStatus, StackFrameDto, ThreadDto, VariableDto};
use mcp_server::{SharedLensState, DEFAULT_MCP_PORT};
use serde::Serialize;
use tauri::{Emitter, State};

use crate::files::{FsEntry, ProjectFile, SourceFileContent};

struct AppState {
    shared: Arc<SharedLensState>,
    mcp_port: Arc<AtomicU16>,
}

#[derive(Debug, Clone, Serialize)]
struct McpStatusDto {
    running: bool,
    port: u16,
    endpoint: String,
    pinned_count: usize,
}

#[tauri::command]
async fn adb_list_devices() -> Result<Vec<AdbDevice>, String> {
    adb_bridge::list_devices().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn adb_list_packages(
    serial: Option<String>,
    filter: Option<String>,
) -> Result<Vec<String>, String> {
    let dev = adb_bridge::resolve_device(serial.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    adb_bridge::list_packages(&dev.serial, filter.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn adb_resolve_launcher(serial: Option<String>, package: String) -> Result<String, String> {
    let dev = adb_bridge::resolve_device(serial.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    adb_bridge::resolve_launcher_activity(&dev.serial, &package)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn debug_attach(
    state: State<'_, AppState>,
    serial: Option<String>,
    package: String,
    mode: Option<String>,
    activity: Option<String>,
    port: Option<u16>,
) -> Result<SessionStatus, String> {
    state
        .shared
        .debug
        .attach(
            serial,
            package,
            mode.as_deref().unwrap_or("auto"),
            activity,
            port.unwrap_or(5005),
        )
        .await
}

#[tauri::command]
async fn debug_detach(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.shared.debug.detach().await
}

#[tauri::command]
async fn debug_get_status(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    Ok(state.shared.debug.status().await)
}

#[tauri::command]
async fn debug_pause(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.shared.debug.pause().await
}

#[tauri::command]
async fn debug_resume(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.shared.debug.resume().await
}

#[tauri::command]
async fn debug_step(state: State<'_, AppState>, depth: String) -> Result<(), String> {
    let d = match depth.as_str() {
        "over" => StepDepth::Over,
        "into" => StepDepth::Into,
        "out" => StepDepth::Out,
        other => return Err(format!("unknown step depth '{other}'")),
    };
    state.shared.debug.step(d).await
}

#[tauri::command]
async fn debug_toggle_breakpoint(
    state: State<'_, AppState>,
    file: String,
    line: i32,
) -> Result<Vec<BreakpointDto>, String> {
    state.shared.debug.toggle_breakpoint(file, line).await
}

#[tauri::command]
async fn debug_list_breakpoints(state: State<'_, AppState>) -> Result<Vec<BreakpointDto>, String> {
    Ok(state.shared.debug.list_breakpoints().await)
}

#[tauri::command]
async fn debug_remove_breakpoint(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<BreakpointDto>, String> {
    state.shared.debug.remove_breakpoint(&id).await
}

#[tauri::command]
async fn debug_list_threads(state: State<'_, AppState>) -> Result<Vec<ThreadDto>, String> {
    state.shared.debug.list_threads().await
}

#[tauri::command]
async fn debug_get_call_stack(
    state: State<'_, AppState>,
    thread_id_hex: Option<String>,
) -> Result<Vec<StackFrameDto>, String> {
    state.shared.debug.get_call_stack(thread_id_hex).await
}

#[tauri::command]
async fn debug_get_locals(
    state: State<'_, AppState>,
    thread_id_hex: Option<String>,
    frame_index: usize,
) -> Result<Vec<VariableDto>, String> {
    state
        .shared
        .debug
        .get_frame_locals(thread_id_hex, frame_index)
        .await
}

#[tauri::command]
async fn debug_inspect_object(
    state: State<'_, AppState>,
    object_id_hex: String,
) -> Result<Vec<VariableDto>, String> {
    state.shared.debug.inspect_object(&object_id_hex).await
}

#[tauri::command]
async fn fs_set_project_root(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<(), String> {
    state.shared.set_project_root(path);
    Ok(())
}

#[tauri::command]
async fn fs_list_dir(path: String) -> Result<Vec<FsEntry>, String> {
    files::list_dir(&path)
}

#[tauri::command]
async fn fs_read_file(path: String) -> Result<SourceFileContent, String> {
    files::read_source_file(&path)
}

#[tauri::command]
async fn fs_list_project_files(project_root: String) -> Result<Vec<ProjectFile>, String> {
    files::list_project_files(&project_root)
}

#[tauri::command]
async fn fs_find_source_file(
    project_root: String,
    file_name: String,
    class_hint: Option<String>,
) -> Result<Option<String>, String> {
    Ok(files::find_source_file(
        &project_root,
        &file_name,
        class_hint.as_deref(),
    ))
}

#[tauri::command]
async fn logcat_start(
    state: State<'_, AppState>,
    serial: Option<String>,
    package: String,
) -> Result<(), String> {
    state.shared.start_logcat(serial, package).await
}

#[tauri::command]
async fn logcat_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.shared.stop_logcat().await;
    Ok(())
}

#[tauri::command]
async fn logcat_clear(state: State<'_, AppState>) -> Result<(), String> {
    state.shared.clear_logcat().await;
    Ok(())
}

#[tauri::command]
async fn logcat_query(
    state: State<'_, AppState>,
    min_level: Option<LogLevel>,
    tag: Option<String>,
    query: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<LogEntry>, String> {
    Ok(state
        .shared
        .query_logcat(min_level, tag, query, limit)
        .await)
}

#[tauri::command]
async fn logcat_crashes(state: State<'_, AppState>) -> Result<Vec<CrashGroup>, String> {
    Ok(state.shared.logcat_crashes().await)
}

#[tauri::command]
async fn pin_add(
    state: State<'_, AppState>,
    req: NewPinRequest,
) -> Result<Vec<PinnedItem>, String> {
    state.shared.pins.add(req);
    Ok(state.shared.pins.list())
}

#[tauri::command]
async fn pin_list(state: State<'_, AppState>) -> Result<Vec<PinnedItem>, String> {
    Ok(state.shared.pins.list())
}

#[tauri::command]
async fn pin_remove(state: State<'_, AppState>, id: String) -> Result<Vec<PinnedItem>, String> {
    state.shared.pins.remove(&id);
    Ok(state.shared.pins.list())
}

#[tauri::command]
async fn pin_clear(state: State<'_, AppState>) -> Result<Vec<PinnedItem>, String> {
    state.shared.pins.clear();
    Ok(Vec::new())
}

#[tauri::command]
async fn mcp_status(state: State<'_, AppState>) -> Result<McpStatusDto, String> {
    let port = state.mcp_port.load(Ordering::Relaxed);
    Ok(McpStatusDto {
        running: port != 0,
        port,
        endpoint: if port != 0 {
            format!("http://127.0.0.1:{port}/mcp")
        } else {
            String::new()
        },
        pinned_count: state.shared.pins.list().len(),
    })
}

#[tauri::command]
async fn inspector_launch_scrcpy(serial: Option<String>) -> Result<String, String> {
    app_inspector::launch_scrcpy(serial.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_capture_layout(serial: Option<String>) -> Result<LayoutSnapshot, String> {
    app_inspector::uiautomator::capture_layout_snapshot(serial.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_list_storage(
    serial: Option<String>,
    package: String,
) -> Result<AppStorageIndex, String> {
    app_inspector::storage::list_app_storage(serial.as_deref(), &package)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_read_shared_prefs(
    serial: Option<String>,
    package: String,
    file_name: String,
) -> Result<Vec<PrefEntry>, String> {
    app_inspector::storage::read_shared_prefs(serial.as_deref(), &package, &file_name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_read_datastore(
    serial: Option<String>,
    package: String,
    file_name: String,
) -> Result<Vec<PrefEntry>, String> {
    app_inspector::storage::read_datastore_prefs(serial.as_deref(), &package, &file_name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_list_sqlite_tables(
    serial: Option<String>,
    package: String,
    db_name: String,
) -> Result<Vec<String>, String> {
    app_inspector::storage::list_sqlite_tables(serial.as_deref(), &package, &db_name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspector_query_sqlite(
    serial: Option<String>,
    package: String,
    db_name: String,
    table_or_sql: String,
    is_custom_sql: bool,
    limit: Option<usize>,
) -> Result<SqliteTableData, String> {
    app_inspector::storage::query_sqlite_table(
        serial.as_deref(),
        &package,
        &db_name,
        &table_or_sql,
        is_custom_sql,
        limit.unwrap_or(100),
    )
    .await
    .map_err(|e| e.to_string())
}

fn main() {
    let shared = SharedLensState::shared();
    let mcp_port = Arc::new(AtomicU16::new(0));

    let shared_setup = Arc::clone(&shared);
    let port_setup = Arc::clone(&mcp_port);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { shared, mcp_port })
        .setup(move |app| {
            // Bridge live DebugSession events (whether triggered from UI or MCP) to the WebView.
            let mut dbg_rx = shared_setup.debug.subscribe();
            let app_dbg = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = dbg_rx.recv().await {
                    let _ = app_dbg.emit("debug://event", ev);
                }
            });

            // Bridge live Logcat events to the WebView.
            let mut lc_rx = shared_setup.subscribe_logcat();
            let app_lc = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = lc_rx.recv().await {
                    let _ = app_lc.emit("logcat://event", ev);
                }
            });

            // Start the embedded localhost MCP server sharing the live DebugSession & pins.
            let shared_mcp = Arc::clone(&shared_setup);
            let port_holder = Arc::clone(&port_setup);
            tauri::async_runtime::spawn(async move {
                match spawn_http_mcp_server(Arc::clone(&shared_mcp), DEFAULT_MCP_PORT).await {
                    Ok((bound, _)) => {
                        port_holder.store(bound, Ordering::Relaxed);
                    }
                    Err(_) => {
                        if let Ok((bound, _)) = spawn_http_mcp_server(shared_mcp, 0).await {
                            port_holder.store(bound, Ordering::Relaxed);
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            adb_list_devices,
            adb_list_packages,
            adb_resolve_launcher,
            debug_attach,
            debug_detach,
            debug_get_status,
            debug_pause,
            debug_resume,
            debug_step,
            debug_toggle_breakpoint,
            debug_list_breakpoints,
            debug_remove_breakpoint,
            debug_list_threads,
            debug_get_call_stack,
            debug_get_locals,
            debug_inspect_object,
            fs_set_project_root,
            fs_list_dir,
            fs_read_file,
            fs_list_project_files,
            fs_find_source_file,
            logcat_start,
            logcat_stop,
            logcat_clear,
            logcat_query,
            logcat_crashes,
            pin_add,
            pin_list,
            pin_remove,
            pin_clear,
            mcp_status,
            inspector_launch_scrcpy,
            inspector_capture_layout,
            inspector_list_storage,
            inspector_read_shared_prefs,
            inspector_read_datastore,
            inspector_list_sqlite_tables,
            inspector_query_sqlite,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Android Lens desktop application");
}
