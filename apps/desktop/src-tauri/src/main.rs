#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod files;
mod session;

use std::sync::Arc;

use adb_bridge::AdbDevice;
use jdwp_client::StepDepth;
use logcat_stream::{
    CrashGroup, LogEntry, LogFilter, LogLevel, LogcatStream, DEFAULT_RING_CAPACITY,
};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

use crate::files::{FsEntry, ProjectFile, SourceFileContent};
use crate::session::{
    BreakpointDto, DebugSession, SessionStatus, StackFrameDto, ThreadDto, VariableDto,
};

struct LogcatState {
    stream: Option<LogcatStream>,
    forward_task: Option<tokio::task::JoinHandle<()>>,
}

struct AppState {
    debug: DebugSession,
    logcat: Arc<Mutex<LogcatState>>,
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
    app: AppHandle,
    state: State<'_, AppState>,
    serial: Option<String>,
    package: String,
    mode: Option<String>,
    activity: Option<String>,
    port: Option<u16>,
) -> Result<SessionStatus, String> {
    state
        .debug
        .attach(
            app,
            serial,
            package,
            mode.as_deref().unwrap_or("auto"),
            activity,
            port.unwrap_or(5005),
        )
        .await
}

#[tauri::command]
async fn debug_detach(app: AppHandle, state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.debug.detach(&app).await
}

#[tauri::command]
async fn debug_get_status(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    Ok(state.debug.status().await)
}

#[tauri::command]
async fn debug_pause(app: AppHandle, state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.debug.pause(&app).await
}

#[tauri::command]
async fn debug_resume(app: AppHandle, state: State<'_, AppState>) -> Result<SessionStatus, String> {
    state.debug.resume(&app).await
}

#[tauri::command]
async fn debug_step(
    app: AppHandle,
    state: State<'_, AppState>,
    depth: String,
) -> Result<(), String> {
    let d = match depth.as_str() {
        "over" => StepDepth::Over,
        "into" => StepDepth::Into,
        "out" => StepDepth::Out,
        other => return Err(format!("unknown step depth '{other}'")),
    };
    state.debug.step(&app, d).await
}

#[tauri::command]
async fn debug_toggle_breakpoint(
    app: AppHandle,
    state: State<'_, AppState>,
    file: String,
    line: i32,
) -> Result<Vec<BreakpointDto>, String> {
    state.debug.toggle_breakpoint(&app, file, line).await
}

#[tauri::command]
async fn debug_list_breakpoints(state: State<'_, AppState>) -> Result<Vec<BreakpointDto>, String> {
    Ok(state.debug.list_breakpoints().await)
}

#[tauri::command]
async fn debug_remove_breakpoint(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<BreakpointDto>, String> {
    state.debug.remove_breakpoint(&app, &id).await
}

#[tauri::command]
async fn debug_list_threads(state: State<'_, AppState>) -> Result<Vec<ThreadDto>, String> {
    state.debug.list_threads().await
}

#[tauri::command]
async fn debug_get_call_stack(
    state: State<'_, AppState>,
    thread_id_hex: Option<String>,
) -> Result<Vec<StackFrameDto>, String> {
    state.debug.get_call_stack(thread_id_hex).await
}

#[tauri::command]
async fn debug_get_locals(
    state: State<'_, AppState>,
    thread_id_hex: Option<String>,
    frame_index: usize,
) -> Result<Vec<VariableDto>, String> {
    state
        .debug
        .get_frame_locals(thread_id_hex, frame_index)
        .await
}

#[tauri::command]
async fn debug_inspect_object(
    state: State<'_, AppState>,
    object_id_hex: String,
) -> Result<Vec<VariableDto>, String> {
    state.debug.inspect_object(&object_id_hex).await
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
    app: AppHandle,
    state: State<'_, AppState>,
    serial: Option<String>,
    package: String,
) -> Result<(), String> {
    let dev = adb_bridge::resolve_device(serial.as_deref())
        .await
        .map_err(|e| e.to_string())?;

    let mut lc = state.logcat.lock().await;
    if let Some(old_task) = lc.forward_task.take() {
        old_task.abort();
    }
    if let Some(old_stream) = lc.stream.take() {
        old_stream.stop();
    }

    let stream = LogcatStream::start(dev.serial, package, DEFAULT_RING_CAPACITY);
    let mut rx = stream.subscribe();
    let app_ev = app.clone();
    let task = tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            let _ = app_ev.emit("logcat://event", ev);
        }
    });

    lc.stream = Some(stream);
    lc.forward_task = Some(task);
    Ok(())
}

#[tauri::command]
async fn logcat_stop(state: State<'_, AppState>) -> Result<(), String> {
    let mut lc = state.logcat.lock().await;
    if let Some(task) = lc.forward_task.take() {
        task.abort();
    }
    if let Some(stream) = lc.stream.take() {
        stream.stop();
    }
    Ok(())
}

#[tauri::command]
async fn logcat_clear(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(ref stream) = state.logcat.lock().await.stream {
        stream.clear();
    }
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
    let lc = state.logcat.lock().await;
    let Some(ref stream) = lc.stream else {
        return Ok(Vec::new());
    };
    Ok(stream.entries(&LogFilter {
        min_level,
        tag,
        query,
        limit,
    }))
}

#[tauri::command]
async fn logcat_crashes(state: State<'_, AppState>) -> Result<Vec<CrashGroup>, String> {
    let lc = state.logcat.lock().await;
    let Some(ref stream) = lc.stream else {
        return Ok(Vec::new());
    };
    Ok(stream.crashes())
}

fn main() {
    let state = AppState {
        debug: DebugSession::new(),
        logcat: Arc::new(Mutex::new(LogcatState {
            stream: None,
            forward_task: None,
        })),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
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
            fs_list_dir,
            fs_read_file,
            fs_list_project_files,
            fs_find_source_file,
            logcat_start,
            logcat_stop,
            logcat_clear,
            logcat_query,
            logcat_crashes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Android Lens desktop application");
}
