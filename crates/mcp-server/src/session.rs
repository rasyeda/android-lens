use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adb_bridge::JdwpForwardGuard;
use jdwp_client::events::{JdwpEvent, EVENT_BREAKPOINT, EVENT_CLASS_PREPARE, SUSPEND_NONE};
use jdwp_client::protocol::{JdwpError, JdwpValue, Location};
use jdwp_client::{
    pretty_signature, root_package_prefix, InspectedObject, JdwpClient, StepDepth,
    DEFAULT_CLASS_EXCLUDES,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".kotlin",
    "build",
    "node_modules",
    "target",
    ".cxx",
    ".dart_tool",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub state: String, // "disconnected" | "running" | "paused"
    pub serial: Option<String>,
    pub package: Option<String>,
    pub pid: Option<u32>,
    pub vm_info: Option<String>,
    pub paused_thread_id: Option<String>,
    pub paused_thread_name: Option<String>,
    pub paused_location: Option<LocationDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationDto {
    pub class_name: String,
    pub method_name: String,
    pub source_file: Option<String>,
    pub line_number: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakpointDto {
    pub id: String,
    pub file: String,
    pub line: i32,
    pub status: String, // "active" | "pending"
    pub class_name: Option<String>,
    pub method_name: Option<String>,
    pub request_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadDto {
    pub id_hex: String,
    pub name: String,
    pub is_paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrameDto {
    pub index: usize,
    pub frame_id_hex: String,
    pub class_name: String,
    pub method_name: String,
    pub source_file: Option<String>,
    pub line_number: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDto {
    pub name: String,
    pub type_name: String,
    pub value: String,
    pub object_id_hex: Option<String>,
    pub is_expandable: bool,
    pub is_static: bool,
    pub declaring_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectedNode {
    pub name: String,
    pub type_name: String,
    pub value: String,
    pub object_id_hex: Option<String>,
    pub is_static: bool,
    pub declaring_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<InspectedNode>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnippet {
    pub path: String,
    pub file_name: String,
    pub focus_line: i32,
    pub start_line: i32,
    pub end_line: i32,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugCoreEvent {
    StatusChanged {
        status: SessionStatus,
    },
    BreakpointsChanged {
        breakpoints: Vec<BreakpointDto>,
    },
    Paused {
        reason: String,
        thread_id_hex: String,
        thread_name: String,
        location: LocationDto,
    },
    Resumed,
    VmExited,
}

struct ActiveConnection {
    client: Arc<JdwpClient>,
    _guard: JdwpForwardGuard,
    serial: String,
    package: String,
    pid: u32,
    vm_info: String,
    class_prepare_req_id: Option<i32>,
    event_task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct InnerState {
    conn: Option<ActiveConnection>,
    paused_thread_id: Option<u64>,
    paused_thread_name: Option<String>,
    paused_location: Option<LocationDto>,
    next_bp_id: usize,
    breakpoints: Vec<BreakpointDto>,
}

impl InnerState {
    fn status(&self) -> SessionStatus {
        let Some(ref conn) = self.conn else {
            return SessionStatus {
                state: "disconnected".into(),
                serial: None,
                package: None,
                pid: None,
                vm_info: None,
                paused_thread_id: None,
                paused_thread_name: None,
                paused_location: None,
            };
        };

        let state = if self.paused_thread_id.is_some() {
            "paused"
        } else {
            "running"
        };

        SessionStatus {
            state: state.into(),
            serial: Some(conn.serial.clone()),
            package: Some(conn.package.clone()),
            pid: Some(conn.pid),
            vm_info: Some(conn.vm_info.clone()),
            paused_thread_id: self.paused_thread_id.map(|id| format!("0x{id:x}")),
            paused_thread_name: self.paused_thread_name.clone(),
            paused_location: self.paused_location.clone(),
        }
    }
}

/// Shared live JDWP debugger session driving both the Tauri v2 Desktop UI and the local MCP server.
#[derive(Clone)]
pub struct DebugSession {
    inner: Arc<Mutex<InnerState>>,
    events_tx: broadcast::Sender<DebugCoreEvent>,
}

impl Default for DebugSession {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugSession {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(Mutex::new(InnerState::default())),
            events_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DebugCoreEvent> {
        self.events_tx.subscribe()
    }

    fn emit(&self, ev: DebugCoreEvent) {
        let _ = self.events_tx.send(ev);
    }

    pub async fn status(&self) -> SessionStatus {
        self.inner.lock().await.status()
    }

    pub async fn list_breakpoints(&self) -> Vec<BreakpointDto> {
        self.inner.lock().await.breakpoints.clone()
    }

    pub async fn attach(
        &self,
        serial: Option<String>,
        package: String,
        mode: &str,
        activity: Option<String>,
        port: u16,
    ) -> Result<SessionStatus, String> {
        let _ = self.detach().await;

        let dev = adb_bridge::resolve_device(serial.as_deref())
            .await
            .map_err(|e| e.to_string())?;

        let (pid, did_launch) = match mode {
            "attach" => {
                let pid = adb_bridge::find_package_pid(&dev.serial, &package)
                    .await
                    .map_err(|e| e.to_string())?;
                (pid, false)
            }
            "launch" => {
                let pid = launch_and_wait(&dev.serial, &package, activity).await?;
                (pid, true)
            }
            _ => match adb_bridge::find_package_pid(&dev.serial, &package).await {
                Ok(pid) => (pid, false),
                Err(_) => {
                    let pid = launch_and_wait(&dev.serial, &package, activity).await?;
                    (pid, true)
                }
            },
        };

        let guard = adb_bridge::forward_jdwp(&dev.serial, port, pid)
            .await
            .map_err(|e| e.to_string())?;

        let attempts = if did_launch { 20 } else { 3 };
        let client = Arc::new(
            connect_with_retry(&guard.local_addr(), attempts)
                .await
                .map_err(|e| e.to_string())?,
        );

        let ver = client.version().await.map_err(|e| e.to_string())?;
        let sizes = client.id_sizes();
        let vm_info = format!(
            "{} ({}) · JDWP {}.{} · {}B IDs",
            ver.vm_name, ver.vm_version, ver.jdwp_major, ver.jdwp_minor, sizes.object_id
        );

        if did_launch {
            client.suspend_vm().await.map_err(|e| e.to_string())?;
        }

        let mut events_rx = client.subscribe_events();
        let inner_ev = Arc::clone(&self.inner);
        let client_ev = Arc::clone(&client);
        let tx_ev = self.events_tx.clone();

        let event_task = tokio::spawn(async move {
            while let Ok(comp) = events_rx.recv().await {
                let mut should_stay_paused = false;
                let mut saw_class_prepare = false;

                for ev in comp.events {
                    match ev {
                        JdwpEvent::Breakpoint {
                            thread_id,
                            location,
                            ..
                        } => {
                            should_stay_paused = true;
                            handle_pause_event(
                                &tx_ev,
                                &inner_ev,
                                &client_ev,
                                "breakpoint",
                                thread_id,
                                location,
                            )
                            .await;
                        }
                        JdwpEvent::SingleStep {
                            thread_id,
                            location,
                            ..
                        } => {
                            should_stay_paused = true;
                            handle_pause_event(
                                &tx_ev, &inner_ev, &client_ev, "step", thread_id, location,
                            )
                            .await;
                        }
                        JdwpEvent::Exception {
                            thread_id,
                            location,
                            ..
                        } => {
                            should_stay_paused = true;
                            handle_pause_event(
                                &tx_ev,
                                &inner_ev,
                                &client_ev,
                                "exception",
                                thread_id,
                                location,
                            )
                            .await;
                        }
                        JdwpEvent::ClassPrepare {
                            ref_type_tag,
                            type_id,
                            signature,
                            ..
                        } => {
                            saw_class_prepare = true;
                            resolve_pending_for_class(
                                &tx_ev,
                                &inner_ev,
                                &client_ev,
                                ref_type_tag,
                                type_id,
                                &signature,
                            )
                            .await;
                        }
                        JdwpEvent::VmDeath { .. } => {
                            let status = {
                                let mut st = inner_ev.lock().await;
                                st.conn = None;
                                st.paused_thread_id = None;
                                st.paused_thread_name = None;
                                st.paused_location = None;
                                for bp in &mut st.breakpoints {
                                    bp.status = "pending".into();
                                    bp.request_ids.clear();
                                }
                                st.status()
                            };
                            let _ = tx_ev.send(DebugCoreEvent::VmExited);
                            let _ = tx_ev.send(DebugCoreEvent::StatusChanged { status });
                        }
                        _ => {}
                    }
                }

                if saw_class_prepare && !should_stay_paused && comp.suspend_policy != SUSPEND_NONE {
                    let _ = client_ev.resume_vm().await;
                }
            }
        });

        {
            let mut st = self.inner.lock().await;
            st.paused_thread_id = None;
            st.paused_thread_name = None;
            st.paused_location = None;
            st.conn = Some(ActiveConnection {
                client: Arc::clone(&client),
                _guard: guard,
                serial: dev.serial,
                package: package.clone(),
                pid,
                vm_info,
                class_prepare_req_id: None,
                event_task,
            });
        }

        self.install_all_queued_breakpoints().await;

        if did_launch {
            let _ = client.resume_vm().await;
        }

        let status = self.status().await;
        self.emit(DebugCoreEvent::StatusChanged {
            status: status.clone(),
        });
        Ok(status)
    }

    pub async fn detach(&self) -> Result<SessionStatus, String> {
        let conn = {
            let mut st = self.inner.lock().await;
            st.paused_thread_id = None;
            st.paused_thread_name = None;
            st.paused_location = None;
            for bp in &mut st.breakpoints {
                bp.status = "pending".into();
                bp.request_ids.clear();
            }
            st.conn.take()
        };

        if let Some(conn) = conn {
            conn.event_task.abort();
            let _ = conn.client.resume_vm().await;
            drop(conn);
        }

        let status = self.status().await;
        let bps = self.list_breakpoints().await;
        self.emit(DebugCoreEvent::BreakpointsChanged { breakpoints: bps });
        self.emit(DebugCoreEvent::StatusChanged {
            status: status.clone(),
        });
        Ok(status)
    }

    pub async fn set_breakpoint(&self, file: String, line: i32) -> Result<BreakpointDto, String> {
        if let Some(existing) = {
            let st = self.inner.lock().await;
            st.breakpoints
                .iter()
                .find(|bp| bp.file.eq_ignore_ascii_case(&file) && bp.line == line)
                .cloned()
        } {
            return Ok(existing);
        }

        let bp_id = {
            let mut st = self.inner.lock().await;
            st.next_bp_id += 1;
            let id = format!("bp-{}", st.next_bp_id);
            st.breakpoints.push(BreakpointDto {
                id: id.clone(),
                file: file.clone(),
                line,
                status: "pending".into(),
                class_name: None,
                method_name: None,
                request_ids: Vec::new(),
            });
            id
        };

        let conn_info = {
            let st = self.inner.lock().await;
            st.conn
                .as_ref()
                .map(|c| (Arc::clone(&c.client), c.package.clone()))
        };

        if let Some((client, package)) = conn_info {
            self.resolve_and_install_single(&client, &package, &bp_id, &file, line)
                .await?;
        }

        let bps = self.list_breakpoints().await;
        let created = bps
            .iter()
            .find(|b| b.id == bp_id)
            .cloned()
            .ok_or_else(|| "breakpoint disappeared".to_string())?;
        self.emit(DebugCoreEvent::BreakpointsChanged { breakpoints: bps });
        Ok(created)
    }

    pub async fn toggle_breakpoint(
        &self,
        file: String,
        line: i32,
    ) -> Result<Vec<BreakpointDto>, String> {
        let existing_id = {
            let st = self.inner.lock().await;
            st.breakpoints
                .iter()
                .find(|bp| bp.file.eq_ignore_ascii_case(&file) && bp.line == line)
                .map(|bp| bp.id.clone())
        };

        if let Some(id) = existing_id {
            return self.remove_breakpoint(&id).await;
        }

        self.set_breakpoint(file, line).await?;
        Ok(self.list_breakpoints().await)
    }

    pub async fn remove_breakpoint_by_location(
        &self,
        file: &str,
        line: i32,
    ) -> Result<Vec<BreakpointDto>, String> {
        let existing_id = {
            let st = self.inner.lock().await;
            st.breakpoints
                .iter()
                .find(|bp| bp.file.eq_ignore_ascii_case(file) && bp.line == line)
                .map(|bp| bp.id.clone())
        };
        if let Some(id) = existing_id {
            return self.remove_breakpoint(&id).await;
        }
        Ok(self.list_breakpoints().await)
    }

    pub async fn remove_breakpoint(&self, bp_id: &str) -> Result<Vec<BreakpointDto>, String> {
        let (client_opt, req_ids, clear_cp) = {
            let mut st = self.inner.lock().await;
            let mut req_ids = Vec::new();
            st.breakpoints.retain(|bp| {
                if bp.id == bp_id {
                    req_ids.extend_from_slice(&bp.request_ids);
                    false
                } else {
                    true
                }
            });
            let has_pending = st.breakpoints.iter().any(|bp| bp.status == "pending");
            let clear_cp = if !has_pending {
                st.conn.as_mut().and_then(|c| c.class_prepare_req_id.take())
            } else {
                None
            };
            let client_opt = st.conn.as_ref().map(|c| Arc::clone(&c.client));
            (client_opt, req_ids, clear_cp)
        };

        if let Some(client) = client_opt {
            for rid in req_ids {
                let _ = client.clear_event_request(EVENT_BREAKPOINT, rid).await;
            }
            if let Some(cp_id) = clear_cp {
                let _ = client.clear_event_request(EVENT_CLASS_PREPARE, cp_id).await;
            }
        }

        let bps = self.list_breakpoints().await;
        self.emit(DebugCoreEvent::BreakpointsChanged {
            breakpoints: bps.clone(),
        });
        Ok(bps)
    }

    async fn install_all_queued_breakpoints(&self) {
        let (client, package, pending) = {
            let st = self.inner.lock().await;
            let Some(ref c) = st.conn else { return };
            let items: Vec<(String, String, i32)> = st
                .breakpoints
                .iter()
                .map(|b| (b.id.clone(), b.file.clone(), b.line))
                .collect();
            (Arc::clone(&c.client), c.package.clone(), items)
        };

        for (id, file, line) in pending {
            let _ = self
                .resolve_and_install_single(&client, &package, &id, &file, line)
                .await;
        }

        let bps = self.list_breakpoints().await;
        self.emit(DebugCoreEvent::BreakpointsChanged { breakpoints: bps });
    }

    async fn resolve_and_install_single(
        &self,
        client: &JdwpClient,
        package: &str,
        bp_id: &str,
        file: &str,
        line: i32,
    ) -> Result<(), String> {
        let matches = client
            .resolve_source_line(Some(package), file, line)
            .await
            .map_err(|e| e.to_string())?;

        if matches.is_empty() {
            let need_cp = {
                let st = self.inner.lock().await;
                st.conn
                    .as_ref()
                    .map(|c| c.class_prepare_req_id.is_none())
                    .unwrap_or(false)
            };
            if need_cp {
                let pattern = format!("{}.*", root_package_prefix(package));
                let req_id = client
                    .set_class_prepare_filtered(Some(&pattern), DEFAULT_CLASS_EXCLUDES)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut st = self.inner.lock().await;
                if let Some(ref mut c) = st.conn {
                    c.class_prepare_req_id = Some(req_id);
                }
            }
            return Ok(());
        }

        let mut req_ids = Vec::new();
        let mut class_name = None;
        let mut method_name = None;
        for m in matches {
            let rid = client
                .set_breakpoint(&m.location)
                .await
                .map_err(|e| e.to_string())?;
            req_ids.push(rid);
            if class_name.is_none() {
                class_name = Some(pretty_signature(&m.class_signature));
                method_name = Some(m.method_name);
            }
        }

        let mut st = self.inner.lock().await;
        if let Some(bp) = st.breakpoints.iter_mut().find(|b| b.id == bp_id) {
            bp.status = "active".into();
            bp.class_name = class_name;
            bp.method_name = method_name;
            bp.request_ids = req_ids;
        }
        Ok(())
    }

    pub async fn pause(&self) -> Result<SessionStatus, String> {
        let client = self.active_client().await?;
        client.suspend_vm().await.map_err(|e| e.to_string())?;

        let threads = client.all_threads().await.unwrap_or_default();
        let mut chosen = None;
        for tid in &threads {
            if let Ok(name) = client.thread_name(*tid).await {
                if name == "main" {
                    chosen = Some((*tid, name));
                    break;
                }
                if chosen.is_none() {
                    chosen = Some((*tid, name));
                }
            }
        }

        let status = {
            let mut st = self.inner.lock().await;
            if let Some((tid, name)) = chosen {
                st.paused_thread_id = Some(tid);
                st.paused_thread_name = Some(name);
            }
            st.status()
        };
        self.emit(DebugCoreEvent::StatusChanged {
            status: status.clone(),
        });
        Ok(status)
    }

    pub async fn resume(&self) -> Result<SessionStatus, String> {
        let client = self.active_client().await?;
        {
            let mut st = self.inner.lock().await;
            st.paused_thread_id = None;
            st.paused_thread_name = None;
            st.paused_location = None;
        }
        client.resume_vm().await.map_err(|e| e.to_string())?;
        let status = self.status().await;
        self.emit(DebugCoreEvent::Resumed);
        self.emit(DebugCoreEvent::StatusChanged {
            status: status.clone(),
        });
        Ok(status)
    }

    pub async fn step(&self, depth: StepDepth) -> Result<(), String> {
        let (client, tid) = {
            let mut st = self.inner.lock().await;
            let client = st
                .conn
                .as_ref()
                .map(|c| Arc::clone(&c.client))
                .ok_or_else(|| "not attached".to_string())?;
            let tid = st
                .paused_thread_id
                .take()
                .ok_or_else(|| "no paused thread to step".to_string())?;
            st.paused_location = None;
            (client, tid)
        };

        if let Err(e) = client.set_single_step(tid, depth).await {
            self.inner.lock().await.paused_thread_id = Some(tid);
            return Err(e.to_string());
        }
        if let Err(e) = client.resume_vm().await {
            self.inner.lock().await.paused_thread_id = Some(tid);
            return Err(e.to_string());
        }

        let status = self.status().await;
        self.emit(DebugCoreEvent::Resumed);
        self.emit(DebugCoreEvent::StatusChanged { status });
        Ok(())
    }

    pub async fn list_threads(&self) -> Result<Vec<ThreadDto>, String> {
        let (client, paused_id) = {
            let st = self.inner.lock().await;
            let client = st
                .conn
                .as_ref()
                .map(|c| Arc::clone(&c.client))
                .ok_or_else(|| "not attached".to_string())?;
            (client, st.paused_thread_id)
        };

        let ids = client.all_threads().await.map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(ids.len());
        for tid in ids {
            let name = client
                .thread_name(tid)
                .await
                .unwrap_or_else(|_| format!("thread@0x{tid:x}"));
            out.push(ThreadDto {
                id_hex: format!("0x{tid:x}"),
                name,
                is_paused: paused_id == Some(tid),
            });
        }
        out.sort_by_key(|t| (!t.is_paused, t.name != "main", t.name.clone()));
        Ok(out)
    }

    pub async fn get_call_stack(
        &self,
        thread_id_hex: Option<String>,
    ) -> Result<Vec<StackFrameDto>, String> {
        let (client, tid) = self.resolve_thread(thread_id_hex).await?;
        let frames = client
            .thread_frames(tid, 0, -1)
            .await
            .map_err(|e| e.to_string())?;

        let mut out = Vec::with_capacity(frames.len().min(40));
        for (index, f) in frames.into_iter().take(40).enumerate() {
            let loc = describe_location(&client, f.location).await;
            out.push(StackFrameDto {
                index,
                frame_id_hex: format!("0x{:x}", f.frame_id),
                class_name: loc.class_name,
                method_name: loc.method_name,
                source_file: loc.source_file,
                line_number: loc.line_number,
            });
        }
        Ok(out)
    }

    pub async fn get_frame_locals(
        &self,
        thread_id_hex: Option<String>,
        frame_index: usize,
    ) -> Result<Vec<VariableDto>, String> {
        let (client, tid) = self.resolve_thread(thread_id_hex).await?;
        let frames = client
            .thread_frames(tid, 0, -1)
            .await
            .map_err(|e| e.to_string())?;
        let Some(frame) = frames.get(frame_index) else {
            return Ok(Vec::new());
        };

        let vars = match client
            .variable_table_with_generic(frame.location.class_id, frame.location.method_id)
            .await
        {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };

        let active: Vec<_> = vars
            .into_iter()
            .filter(|v| {
                let start = v.code_index;
                let end = start + (v.length.max(0) as u64);
                frame.location.index >= start && frame.location.index <= end
            })
            .collect();

        if active.is_empty() {
            return Ok(Vec::new());
        }

        let slot_queries: Vec<(i32, u8)> = active
            .iter()
            .map(|v| {
                (
                    v.slot,
                    v.signature.as_bytes().first().copied().unwrap_or(b'L'),
                )
            })
            .collect();

        let values = client
            .frame_get_values(tid, frame.frame_id, &slot_queries)
            .await
            .map_err(|e| e.to_string())?;

        let mut out = Vec::with_capacity(active.len());
        for (var, val) in active.into_iter().zip(values) {
            out.push(
                value_to_variable_dto(
                    &client,
                    var.name,
                    pretty_signature(&var.signature),
                    &val,
                    false,
                    None,
                )
                .await,
            );
        }
        Ok(out)
    }

    pub async fn inspect_object(&self, object_id_hex: &str) -> Result<Vec<VariableDto>, String> {
        let client = self.active_client().await?;
        let obj_id = parse_hex_id(object_id_hex)?;

        match client
            .inspect_object(obj_id, 100)
            .await
            .map_err(|e| e.to_string())?
        {
            InspectedObject::Array { elements, .. } => {
                let mut out = Vec::with_capacity(elements.len());
                for (i, val) in elements.iter().enumerate() {
                    out.push(
                        value_to_variable_dto(
                            &client,
                            format!("[{i}]"),
                            String::new(),
                            val,
                            false,
                            None,
                        )
                        .await,
                    );
                }
                Ok(out)
            }
            InspectedObject::Object { signature, fields } => {
                let root = pretty_signature(&signature);
                let mut out = Vec::with_capacity(fields.len());
                for f in fields {
                    let owner = pretty_signature(&f.declaring_class);
                    let declaring = if owner != root { Some(owner) } else { None };
                    out.push(
                        value_to_variable_dto(
                            &client,
                            f.name,
                            pretty_signature(&f.signature),
                            &f.value,
                            f.is_static,
                            declaring,
                        )
                        .await,
                    );
                }
                Ok(out)
            }
        }
    }

    /// Recursively expands an object/array up to `depth` levels (capped at 3) with cycle protection.
    pub async fn inspect_object_depth(
        &self,
        object_id_hex: &str,
        depth: usize,
        max_array_elements: usize,
    ) -> Result<Vec<InspectedNode>, String> {
        let client = self.active_client().await?;
        let obj_id = parse_hex_id(object_id_hex)?;
        let depth = depth.clamp(1, 3);
        let max_arr = max_array_elements.clamp(1, 100);
        let mut visited = HashSet::new();
        visited.insert(obj_id);
        inspect_recursive(&client, obj_id, depth, max_arr, &mut visited).await
    }

    async fn active_client(&self) -> Result<Arc<JdwpClient>, String> {
        self.inner
            .lock()
            .await
            .conn
            .as_ref()
            .map(|c| Arc::clone(&c.client))
            .ok_or_else(|| "not attached to a debuggable process".to_string())
    }

    async fn resolve_thread(
        &self,
        thread_id_hex: Option<String>,
    ) -> Result<(Arc<JdwpClient>, u64), String> {
        let st = self.inner.lock().await;
        let client = st
            .conn
            .as_ref()
            .map(|c| Arc::clone(&c.client))
            .ok_or_else(|| "not attached".to_string())?;
        let tid = match thread_id_hex {
            Some(ref h) => parse_hex_id(h)?,
            None => st
                .paused_thread_id
                .ok_or_else(|| "no paused thread".to_string())?,
        };
        Ok((client, tid))
    }
}

fn inspect_recursive<'a>(
    client: &'a JdwpClient,
    object_id: u64,
    remaining_depth: usize,
    max_array_elements: usize,
    visited: &'a mut HashSet<u64>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<InspectedNode>, String>> + Send + 'a>,
> {
    Box::pin(async move {
        let inspected = client
            .inspect_object(object_id, max_array_elements)
            .await
            .map_err(|e| e.to_string())?;

        let raw_items: Vec<(String, String, JdwpValue, bool, Option<String>)> = match inspected {
            InspectedObject::Array { elements, .. } => elements
                .into_iter()
                .enumerate()
                .map(|(i, val)| (format!("[{i}]"), String::new(), val, false, None))
                .collect(),
            InspectedObject::Object { signature, fields } => {
                let root = pretty_signature(&signature);
                fields
                    .into_iter()
                    .map(|f| {
                        let owner = pretty_signature(&f.declaring_class);
                        let declaring = if owner != root { Some(owner) } else { None };
                        (
                            f.name,
                            pretty_signature(&f.signature),
                            f.value,
                            f.is_static,
                            declaring,
                        )
                    })
                    .collect()
            }
        };

        let mut nodes = Vec::with_capacity(raw_items.len());
        for (name, type_name, val, is_static, declaring_class) in raw_items {
            let formatted = client.format_value(&val).await;
            let (object_id_hex, child_id) = match val {
                JdwpValue::Object { tag, id } if id != 0 => {
                    (Some(format!("0x{id:x}")), (tag != b's').then_some(id))
                }
                _ => (None, None),
            };

            let children = if remaining_depth > 1 {
                if let Some(cid) = child_id {
                    if visited.insert(cid) {
                        inspect_recursive(
                            client,
                            cid,
                            remaining_depth - 1,
                            max_array_elements.min(20),
                            visited,
                        )
                        .await
                        .ok()
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            nodes.push(InspectedNode {
                name,
                type_name,
                value: formatted,
                object_id_hex,
                is_static,
                declaring_class,
                children,
            });
        }
        Ok(nodes)
    })
}

pub fn parse_hex_id(raw: &str) -> Result<u64, String> {
    let token = raw
        .trim()
        .rsplit_once('@')
        .map(|(_, rhs)| rhs)
        .unwrap_or(raw.trim());
    let hex = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
        .unwrap_or(token);
    u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex id '{raw}': {e}"))
}

/// Reads a formatted snippet (±`context_lines`) around `focus_line` for a source file inside `project_root`.
pub fn read_source_snippet(
    project_root: &str,
    file_name: &str,
    class_hint: Option<&str>,
    focus_line: i32,
    context_lines: i32,
) -> Option<SourceSnippet> {
    let abs = find_source_file(project_root, file_name, class_hint)?;
    let content = fs::read_to_string(&abs).ok()?;
    let all_lines: Vec<&str> = content.lines().collect();
    if all_lines.is_empty() || focus_line <= 0 {
        return None;
    }

    let ctx = context_lines.clamp(1, 30);
    let start = (focus_line - ctx).max(1);
    let end = (focus_line + ctx).min(all_lines.len() as i32);
    let mut lines = Vec::with_capacity((end - start + 1) as usize);
    for ln in start..=end {
        let marker = if ln == focus_line { ">" } else { " " };
        let text = all_lines[(ln - 1) as usize];
        lines.push(format!("{marker} {ln:>4} | {text}"));
    }

    Some(SourceSnippet {
        path: abs,
        file_name: file_name.to_string(),
        focus_line,
        start_line: start,
        end_line: end,
        lines,
    })
}

pub fn find_source_file(
    project_root: &str,
    file_name: &str,
    class_hint: Option<&str>,
) -> Option<String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    collect_matching_files(Path::new(project_root), file_name, 0, &mut matches);
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0].to_string_lossy().to_string());
    }

    if let Some(cls) = class_hint {
        let pkg_path = cls
            .rsplit_once('.')
            .map(|(pkg, _)| pkg.replace('.', "/"))
            .unwrap_or_default();
        if !pkg_path.is_empty() {
            if let Some(best) = matches
                .iter()
                .find(|p| p.to_string_lossy().replace('\\', "/").contains(&pkg_path))
            {
                return Some(best.to_string_lossy().to_string());
            }
        }
    }

    if let Some(best) = matches.iter().find(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.contains("/src/main/") && !s.contains("/.claude/")
    }) {
        return Some(best.to_string_lossy().to_string());
    }

    Some(matches[0].to_string_lossy().to_string())
}

fn collect_matching_files(dir: &Path, target_name: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 14 || out.len() >= 16 {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if !IGNORED_DIRS.contains(&name.as_str()) {
                collect_matching_files(&path, target_name, depth + 1, out);
            }
        } else if name.eq_ignore_ascii_case(target_name) {
            out.push(path);
        }
    }
}

async fn launch_and_wait(
    serial: &str,
    package: &str,
    activity: Option<String>,
) -> Result<u32, String> {
    let component = match activity.filter(|s| !s.trim().is_empty()) {
        Some(comp) => {
            if comp.contains('/') {
                comp
            } else {
                format!("{package}/{comp}")
            }
        }
        None => adb_bridge::resolve_launcher_activity(serial, package)
            .await
            .map_err(|e| e.to_string())?,
    };
    adb_bridge::launch_wait_for_debugger(serial, &component)
        .await
        .map_err(|e| format!("failed to launch '{component}': {e}"))?;
    adb_bridge::wait_for_package_pid(serial, package, Duration::from_secs(15))
        .await
        .map_err(|e| format!("timed out waiting for '{package}': {e}"))
}

async fn connect_with_retry(addr: &str, attempts: usize) -> Result<JdwpClient, JdwpError> {
    let mut last_err = JdwpError::ConnectionClosed;
    for attempt in 0..attempts {
        match JdwpClient::connect(addr).await {
            Ok(c) => return Ok(c),
            Err(JdwpError::HandshakeTimeout) => return Err(JdwpError::HandshakeTimeout),
            Err(e) => {
                last_err = e;
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        }
    }
    Err(last_err)
}

async fn handle_pause_event(
    tx: &broadcast::Sender<DebugCoreEvent>,
    inner: &Arc<Mutex<InnerState>>,
    client: &JdwpClient,
    reason: &str,
    thread_id: u64,
    location: Location,
) {
    let loc_dto = describe_location(client, location).await;
    let tname = client
        .thread_name(thread_id)
        .await
        .unwrap_or_else(|_| format!("thread@0x{thread_id:x}"));

    let status = {
        let mut st = inner.lock().await;
        st.paused_thread_id = Some(thread_id);
        st.paused_thread_name = Some(tname.clone());
        st.paused_location = Some(loc_dto.clone());
        st.status()
    };

    let _ = tx.send(DebugCoreEvent::Paused {
        reason: reason.to_string(),
        thread_id_hex: format!("0x{thread_id:x}"),
        thread_name: tname,
        location: loc_dto,
    });
    let _ = tx.send(DebugCoreEvent::StatusChanged { status });
}

async fn resolve_pending_for_class(
    tx: &broadcast::Sender<DebugCoreEvent>,
    inner: &Arc<Mutex<InnerState>>,
    client: &JdwpClient,
    ref_type_tag: u8,
    type_id: u64,
    signature: &str,
) {
    let pending_snapshot: Vec<(String, String, i32)> = {
        let st = inner.lock().await;
        st.breakpoints
            .iter()
            .filter(|b| b.status == "pending")
            .map(|b| (b.id.clone(), b.file.clone(), b.line))
            .collect()
    };
    if pending_snapshot.is_empty() {
        return;
    }

    let mut changed = false;
    for (id, file, line) in pending_snapshot {
        let Ok(matches) = client
            .resolve_source_line_in_class(ref_type_tag, type_id, signature, &file, line)
            .await
        else {
            continue;
        };
        if matches.is_empty() {
            continue;
        }
        let mut req_ids = Vec::new();
        let mut cls_name = None;
        let mut m_name = None;
        for m in matches {
            if let Ok(rid) = client.set_breakpoint(&m.location).await {
                req_ids.push(rid);
                if cls_name.is_none() {
                    cls_name = Some(pretty_signature(&m.class_signature));
                    m_name = Some(m.method_name);
                }
            }
        }
        if !req_ids.is_empty() {
            changed = true;
            let mut st = inner.lock().await;
            if let Some(bp) = st.breakpoints.iter_mut().find(|b| b.id == id) {
                bp.status = "active".into();
                bp.class_name = cls_name;
                bp.method_name = m_name;
                bp.request_ids = req_ids;
            }
        }
    }

    if changed {
        let (bps, clear_cp) = {
            let mut st = inner.lock().await;
            let still_pending = st.breakpoints.iter().any(|b| b.status == "pending");
            let clear_cp = if !still_pending {
                st.conn.as_mut().and_then(|c| c.class_prepare_req_id.take())
            } else {
                None
            };
            (st.breakpoints.clone(), clear_cp)
        };
        if let Some(cp_id) = clear_cp {
            let _ = client.clear_event_request(EVENT_CLASS_PREPARE, cp_id).await;
        }
        let _ = tx.send(DebugCoreEvent::BreakpointsChanged { breakpoints: bps });
    }
}

async fn describe_location(client: &JdwpClient, loc: Location) -> LocationDto {
    let class_name = client
        .reference_type_signature(loc.class_id)
        .await
        .map(|s| pretty_signature(&s))
        .unwrap_or_else(|_| format!("class@0x{:x}", loc.class_id));

    let method_name = client
        .methods_with_generic(loc.class_id)
        .await
        .ok()
        .and_then(|ms| ms.into_iter().find(|m| m.method_id == loc.method_id))
        .map(|m| m.name)
        .unwrap_or_else(|| format!("method@0x{:x}", loc.method_id));

    let source_file = client.source_file(loc.class_id).await.ok();
    let line_number = client
        .line_table(loc.class_id, loc.method_id)
        .await
        .ok()
        .and_then(|table| {
            table
                .into_iter()
                .rfind(|e| e.code_index <= loc.index)
                .map(|e| e.line_number)
        });

    LocationDto {
        class_name,
        method_name,
        source_file,
        line_number,
    }
}

async fn value_to_variable_dto(
    client: &JdwpClient,
    name: String,
    type_name: String,
    val: &JdwpValue,
    is_static: bool,
    declaring_class: Option<String>,
) -> VariableDto {
    let formatted = client.format_value(val).await;
    let (object_id_hex, is_expandable) = match val {
        JdwpValue::Object { tag, id } if *id != 0 => (Some(format!("0x{id:x}")), *tag != b's'),
        _ => (None, false),
    };
    VariableDto {
        name,
        type_name,
        value: formatted,
        object_id_hex,
        is_expandable,
        is_static,
        declaring_class,
    }
}
