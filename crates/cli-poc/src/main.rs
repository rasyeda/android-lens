use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jdwp_client::events::{JdwpEvent, EVENT_BREAKPOINT, EVENT_CLASS_PREPARE, SUSPEND_NONE};
use jdwp_client::{
    pretty_signature, root_package_prefix, InspectedObject, JdwpClient, StepDepth,
    DEFAULT_CLASS_EXCLUDES,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(
    name = "android-lens-poc",
    about = "Lightweight pure-Rust Android JDWP debugger PoC"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List connected ADB devices and emulators
    Devices,
    /// List installed third-party packages on a device
    Packages {
        #[arg(long)]
        serial: Option<String>,
        #[arg(long)]
        filter: Option<String>,
    },
    /// Attach to a running debuggable Android app (or launch with `am start -D`)
    Attach {
        /// Android application package name (optional if `--launch` is provided)
        #[arg(short, long)]
        package: Option<String>,
        /// Launch `<pkg/activity>` with `am start -D -S` (Wait for Debugger) before attaching
        #[arg(long)]
        launch: Option<String>,
        /// Optional ADB device serial
        #[arg(short, long)]
        serial: Option<String>,
        /// Local TCP port for `adb forward`
        #[arg(long, default_value_t = 5005)]
        port: u16,
        /// Optional initial breakpoint in `File.kt:line` format
        #[arg(short = 'b', long = "break")]
        initial_break: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct PendingBreakpoint {
    id: usize,
    file: String,
    line: i32,
}

#[derive(Debug, Clone)]
struct ActiveBreakpoint {
    request_id: i32,
    class_name: String,
    method_name: String,
    source_file: String,
    line_number: i32,
}

#[derive(Default)]
struct BreakpointManager {
    next_pending_id: usize,
    pending: Vec<PendingBreakpoint>,
    active: Vec<ActiveBreakpoint>,
    class_prepare_req_id: Option<i32>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Devices => {
            let devices = adb_bridge::list_devices().await?;
            if devices.is_empty() {
                println!("No ADB devices connected.");
            } else {
                for d in devices {
                    println!(
                        "{:<24} {:<10} model={} device={}",
                        d.serial,
                        d.state,
                        d.model.as_deref().unwrap_or("-"),
                        d.device.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Commands::Packages { serial, filter } => {
            let dev = adb_bridge::resolve_device(serial.as_deref()).await?;
            let pkgs = adb_bridge::list_packages(&dev.serial, filter.as_deref()).await?;
            for p in pkgs {
                println!("{p}");
            }
        }
        Commands::Attach {
            package,
            launch,
            serial,
            port,
            initial_break,
        } => {
            let resolved_pkg = match (package, launch.as_deref()) {
                (Some(p), _) => p,
                (None, Some(comp)) => comp
                    .split('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .context("invalid --launch component; expected <package/activity>")?
                    .to_string(),
                (None, None) => {
                    bail!("specify either --package <pkg> or --launch <pkg/activity>")
                }
            };
            run_attach(
                &resolved_pkg,
                launch.as_deref(),
                serial.as_deref(),
                port,
                initial_break.as_deref(),
            )
            .await?;
        }
    }

    Ok(())
}

async fn connect_with_retry(addr: &str, attempts: usize) -> Result<JdwpClient> {
    let mut last_err = None;
    for attempt in 0..attempts {
        match JdwpClient::connect(addr).await {
            Ok(client) => return Ok(client),
            Err(jdwp_client::protocol::JdwpError::HandshakeTimeout) => {
                return Err(jdwp_client::protocol::JdwpError::HandshakeTimeout.into());
            }
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < attempts {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        }
    }
    Err(last_err.unwrap().into())
}

async fn run_attach(
    package: &str,
    launch: Option<&str>,
    serial: Option<&str>,
    port: u16,
    initial_break: Option<&str>,
) -> Result<()> {
    let dev = adb_bridge::resolve_device(serial)
        .await
        .context("could not find an online ADB device/emulator")?;
    println!(
        "Using device {} ({})",
        dev.serial,
        dev.model.as_deref().unwrap_or("unknown")
    );

    let pid = if let Some(component) = launch {
        println!("Launching {component} with `am start -D -S` (Wait for Debugger)...");
        adb_bridge::launch_wait_for_debugger(&dev.serial, component)
            .await
            .with_context(|| format!("failed to launch '{component}' on {}", dev.serial))?;
        adb_bridge::wait_for_package_pid(&dev.serial, package, Duration::from_secs(15))
            .await
            .with_context(|| format!("timed out waiting for '{package}' PID after launch"))?
    } else {
        adb_bridge::find_package_pid(&dev.serial, package)
            .await
            .with_context(|| format!("is '{package}' running on {}?", dev.serial))?
    };
    println!("Found {package} (PID {pid})");

    let guard = adb_bridge::forward_jdwp(&dev.serial, port, pid).await?;
    println!(
        "Forwarded {} -> jdwp:{pid} on {}",
        guard.local_addr(),
        guard.serial()
    );

    let attempts = if launch.is_some() { 20 } else { 3 };
    let client = Arc::new(
        connect_with_retry(&guard.local_addr(), attempts)
            .await
            .context("JDWP handshake failed (is the app built with debuggable=true?)")?,
    );

    let ver = client.version().await?;
    let sizes = client.id_sizes();
    println!(
        "Connected! VM: {} ({}) | JDWP {}.{} | ID sizes: obj={} ref={} method={} frame={}",
        ver.vm_name,
        ver.vm_version,
        ver.jdwp_major,
        ver.jdwp_minor,
        sizes.object_id,
        sizes.reference_type_id,
        sizes.method_id,
        sizes.frame_id
    );

    if launch.is_some() {
        // Hold startup so initial breakpoints (immediate or ClassPrepare) cannot race app init.
        client.suspend_vm().await?;
    }

    let paused_thread: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let bp_mgr: Arc<Mutex<BreakpointManager>> = Arc::new(Mutex::new(BreakpointManager::default()));

    // Spawn event listener task
    let mut events_rx = client.subscribe_events();
    let client_ev = Arc::clone(&client);
    let paused_ev = Arc::clone(&paused_thread);
    let bp_ev = Arc::clone(&bp_mgr);
    tokio::spawn(async move {
        while let Ok(comp) = events_rx.recv().await {
            let mut should_stay_paused = false;
            let mut saw_class_prepare = false;

            for ev in comp.events {
                match ev {
                    JdwpEvent::Breakpoint {
                        request_id,
                        thread_id,
                        location,
                    } => {
                        should_stay_paused = true;
                        *paused_ev.lock().await = Some(thread_id);
                        let loc_str = describe_location(&client_ev, location).await;
                        let tname = client_ev
                            .thread_name(thread_id)
                            .await
                            .unwrap_or_else(|_| format!("thread@0x{thread_id:x}"));
                        println!(
                            "\n[BREAKPOINT #{request_id}] paused on thread '{tname}' at {loc_str}"
                        );
                        println!("Type `bt`, `locals`, `inspect <0xid>`, `n` (step over), `s` (into), `o` (out), or `c` (resume).");
                    }
                    JdwpEvent::SingleStep {
                        thread_id,
                        location,
                        ..
                    } => {
                        should_stay_paused = true;
                        *paused_ev.lock().await = Some(thread_id);
                        let loc_str = describe_location(&client_ev, location).await;
                        println!("\n[STEP] at {loc_str}");
                    }
                    JdwpEvent::ClassPrepare {
                        ref_type_tag,
                        type_id,
                        signature,
                        ..
                    } => {
                        saw_class_prepare = true;
                        handle_class_prepare(&client_ev, &bp_ev, ref_type_tag, type_id, &signature)
                            .await;
                    }
                    JdwpEvent::Exception {
                        request_id,
                        thread_id,
                        location,
                        ..
                    } => {
                        should_stay_paused = true;
                        *paused_ev.lock().await = Some(thread_id);
                        let loc_str = describe_location(&client_ev, location).await;
                        println!("\n[EXCEPTION #{request_id}] at {loc_str}");
                    }
                    JdwpEvent::VmDeath { .. } => {
                        println!("\n[VM_DEATH] Target app exited.");
                    }
                    _ => {}
                }
            }

            if saw_class_prepare && !should_stay_paused && comp.suspend_policy != SUSPEND_NONE {
                let _ = client_ev.resume_vm().await;
            }
        }
    });

    if let Some(spec) = initial_break {
        set_breakpoint_from_spec(&client, &bp_mgr, package, spec).await?;
    }

    if launch.is_some() {
        if initial_break.is_some() {
            client.resume_vm().await?;
            println!("Resumed launched app; waiting for startup breakpoint...");
        } else {
            println!(
                "App suspended at startup. Set breakpoints with `b <File.kt:line>` then type `c` to continue."
            );
        }
    }

    println!("Interactive debugger ready. Type `help` for commands.");
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    loop {
        eprint!("jdwp> ");
        let Some(line) = lines.next_line().await? else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts[0] {
            "help" | "h" => {
                println!("Commands:");
                println!("  classes [filter]        List loaded classes (default: app package)");
                println!(
                    "  b <File.kt:line>        Set line breakpoint (or pending via ClassPrepare)"
                );
                println!("  bl | breakpoints        List active and pending breakpoints");
                println!("  bd <id | P#id>          Delete an active or pending breakpoint");
                println!("  threads                 List threads");
                println!("  bt                      Print stack trace of paused thread");
                println!("  locals                  Print local variables of top frame");
                println!("  inspect <0xobject_id>   Expand object fields (including superclasses) or array");
                println!("  n | over                Step over");
                println!("  s | into                Step into");
                println!("  o | out                 Step out");
                println!("  c | resume              Resume VM");
                println!("  pause                   Suspend VM");
                println!("  q | quit                Resume & exit");
            }
            "classes" => {
                let filter = parts.get(1).copied().unwrap_or(package).replace('.', "/");
                let classes = client.all_classes_with_generic().await?;
                let mut shown = 0usize;
                for c in classes {
                    if c.signature.contains(&filter) {
                        println!("  {}", pretty_signature(&c.signature));
                        shown += 1;
                        if shown >= 50 {
                            println!("  ... (showing first 50 matches)");
                            break;
                        }
                    }
                }
                if shown == 0 {
                    println!("No loaded classes matched '{filter}'.");
                }
            }
            "b" | "break" => {
                let Some(spec) = parts.get(1) else {
                    println!("Usage: b <File.kt:line>");
                    continue;
                };
                if let Err(e) = set_breakpoint_from_spec(&client, &bp_mgr, package, spec).await {
                    println!("Error: {e}");
                }
            }
            "bl" | "breakpoints" => {
                let mgr = bp_mgr.lock().await;
                if mgr.active.is_empty() && mgr.pending.is_empty() {
                    println!("No breakpoints set.");
                } else {
                    for bp in &mgr.active {
                        println!(
                            "  #{:<3} [active]  {}.{} ({}:{})",
                            bp.request_id,
                            bp.class_name,
                            bp.method_name,
                            bp.source_file,
                            bp.line_number
                        );
                    }
                    for p in &mgr.pending {
                        println!("  P#{:<2} [pending] {}:{}", p.id, p.file, p.line);
                    }
                }
            }
            "bd" => {
                let Some(target) = parts.get(1) else {
                    println!("Usage: bd <request_id | P#pending_id>");
                    continue;
                };
                if let Err(e) = delete_breakpoint(&client, &bp_mgr, target).await {
                    println!("Error: {e}");
                }
            }
            "threads" => {
                let threads = client.all_threads().await?;
                for tid in threads {
                    let name = client
                        .thread_name(tid)
                        .await
                        .unwrap_or_else(|_| "<unknown>".into());
                    println!("  0x{tid:016x}  {name}");
                }
            }
            "bt" => {
                let Some(tid) = *paused_thread.lock().await else {
                    println!("No thread is currently paused at a breakpoint/step.");
                    continue;
                };
                match client.thread_frames(tid, 0, -1).await {
                    Ok(frames) => {
                        for (i, f) in frames.iter().take(25).enumerate() {
                            let desc = describe_location(&client, f.location).await;
                            println!("  #{i:<2} {desc}");
                        }
                        if frames.len() > 25 {
                            println!("  ... ({} more frames)", frames.len() - 25);
                        }
                    }
                    Err(e) => println!("Failed to get frames: {e}"),
                }
            }
            "locals" => {
                let Some(tid) = *paused_thread.lock().await else {
                    println!("No thread is currently paused at a breakpoint/step.");
                    continue;
                };
                if let Err(e) = print_locals(&client, tid).await {
                    println!("Failed to read locals: {e}");
                }
            }
            "inspect" | "i" => {
                let Some(raw_id) = parts.get(1) else {
                    println!("Usage: inspect <0xobject_id | Type@0xobject_id>");
                    continue;
                };
                match parse_object_id(raw_id) {
                    Ok(obj_id) => {
                        if let Err(e) = print_inspected_object(&client, obj_id).await {
                            println!("Failed to inspect 0x{obj_id:x}: {e}");
                        }
                    }
                    Err(e) => println!("Error: {e}"),
                }
            }
            "n" | "over" => {
                step_command(&client, &paused_thread, StepDepth::Over).await;
            }
            "s" | "into" => {
                step_command(&client, &paused_thread, StepDepth::Into).await;
            }
            "o" | "out" => {
                step_command(&client, &paused_thread, StepDepth::Out).await;
            }
            "c" | "resume" => {
                *paused_thread.lock().await = None;
                client.resume_vm().await?;
                println!("Resumed VM.");
            }
            "pause" => {
                client.suspend_vm().await?;
                println!("Suspended VM.");
            }
            "q" | "quit" | "exit" => {
                let _ = client.resume_vm().await;
                println!("Detached.");
                break;
            }
            other => println!("Unknown command '{other}'. Type `help`."),
        }
    }

    drop(guard);
    Ok(())
}

async fn handle_class_prepare(
    client: &JdwpClient,
    bp_mgr: &Arc<Mutex<BreakpointManager>>,
    ref_type_tag: u8,
    type_id: u64,
    signature: &str,
) {
    let pending_snapshot = {
        let mgr = bp_mgr.lock().await;
        if mgr.pending.is_empty() {
            return;
        }
        mgr.pending.clone()
    };

    let mut resolved_ids = Vec::new();
    let mut newly_active = Vec::new();

    for p in pending_snapshot {
        let Ok(matches) = client
            .resolve_source_line_in_class(ref_type_tag, type_id, signature, &p.file, p.line)
            .await
        else {
            continue;
        };
        if matches.is_empty() {
            continue;
        }
        for m in matches {
            match client.set_breakpoint(&m.location).await {
                Ok(req_id) => {
                    let cls = pretty_signature(&m.class_signature);
                    println!(
                        "\n[PENDING BP P#{} RESOLVED -> BP #{req_id}] {cls}.{} ({}:{})",
                        p.id, m.method_name, m.source_file, m.line_number
                    );
                    newly_active.push(ActiveBreakpoint {
                        request_id: req_id,
                        class_name: cls,
                        method_name: m.method_name,
                        source_file: m.source_file,
                        line_number: m.line_number,
                    });
                    resolved_ids.push(p.id);
                }
                Err(e) => {
                    println!(
                        "\n[PENDING BP P#{}] failed to install breakpoint on {signature}: {e}",
                        p.id
                    );
                }
            }
        }
    }

    if !resolved_ids.is_empty() {
        let clear_req = {
            let mut mgr = bp_mgr.lock().await;
            mgr.active.extend(newly_active);
            mgr.pending.retain(|p| !resolved_ids.contains(&p.id));
            if mgr.pending.is_empty() {
                mgr.class_prepare_req_id.take()
            } else {
                None
            }
        };
        if let Some(req_id) = clear_req {
            let _ = client
                .clear_event_request(EVENT_CLASS_PREPARE, req_id)
                .await;
        }
    }
}

async fn step_command(
    client: &JdwpClient,
    paused_thread: &Arc<Mutex<Option<u64>>>,
    depth: StepDepth,
) {
    let tid = {
        let mut guard = paused_thread.lock().await;
        guard.take()
    };
    let Some(tid) = tid else {
        println!("No paused thread to step.");
        return;
    };
    if let Err(e) = client.set_single_step(tid, depth).await {
        *paused_thread.lock().await = Some(tid);
        println!("Failed to request step: {e}");
        return;
    }
    if let Err(e) = client.resume_vm().await {
        *paused_thread.lock().await = Some(tid);
        println!("Failed to resume for step: {e}");
    }
}

async fn set_breakpoint_from_spec(
    client: &JdwpClient,
    bp_mgr: &Arc<Mutex<BreakpointManager>>,
    package: &str,
    spec: &str,
) -> Result<()> {
    let Some((file, line_str)) = spec.rsplit_once(':') else {
        bail!("expected format File.kt:line, got '{spec}'");
    };
    let line: i32 = line_str
        .parse()
        .with_context(|| format!("invalid line number '{line_str}'"))?;

    println!("Resolving {file}:{line} in package '{package}'...");
    let matches = client
        .resolve_source_line(Some(package), file, line)
        .await?;

    if matches.is_empty() {
        let (pending_id, need_class_prepare) = {
            let mut mgr = bp_mgr.lock().await;
            if let Some(existing) = mgr
                .pending
                .iter()
                .find(|p| p.file.eq_ignore_ascii_case(file) && p.line == line)
            {
                println!(
                    "Pending breakpoint P#{} already registered for {file}:{line}.",
                    existing.id
                );
                return Ok(());
            }
            mgr.next_pending_id += 1;
            let id = mgr.next_pending_id;
            mgr.pending.push(PendingBreakpoint {
                id,
                file: file.to_string(),
                line,
            });
            (id, mgr.class_prepare_req_id.is_none())
        };

        if need_class_prepare {
            let root = root_package_prefix(package);
            let pattern = format!("{root}.*");
            let req_id = client
                .set_class_prepare_filtered(Some(&pattern), DEFAULT_CLASS_EXCLUDES)
                .await?;
            bp_mgr.lock().await.class_prepare_req_id = Some(req_id);
        }
        println!(
            "No loaded class matched {file}:{line}. Registered pending breakpoint P#{pending_id} (will auto-install on ClassPrepare)."
        );
        return Ok(());
    }

    let mut mgr = bp_mgr.lock().await;
    for m in matches {
        let req_id = client.set_breakpoint(&m.location).await?;
        let cls = pretty_signature(&m.class_signature);
        println!(
            "Breakpoint #{req_id} set at {cls}.{} ({}:{})",
            m.method_name, m.source_file, m.line_number
        );
        mgr.active.push(ActiveBreakpoint {
            request_id: req_id,
            class_name: cls,
            method_name: m.method_name,
            source_file: m.source_file,
            line_number: m.line_number,
        });
    }
    Ok(())
}

async fn delete_breakpoint(
    client: &JdwpClient,
    bp_mgr: &Arc<Mutex<BreakpointManager>>,
    target: &str,
) -> Result<()> {
    if let Some(rest) = target
        .strip_prefix("P#")
        .or_else(|| target.strip_prefix("p#"))
        .or_else(|| target.strip_prefix('P'))
        .or_else(|| target.strip_prefix('p'))
    {
        let pid: usize = rest
            .parse()
            .with_context(|| format!("invalid pending breakpoint id '{target}'"))?;
        let (removed, clear_req) = {
            let mut mgr = bp_mgr.lock().await;
            let before = mgr.pending.len();
            mgr.pending.retain(|p| p.id != pid);
            let removed = mgr.pending.len() < before;
            let clear_req = if mgr.pending.is_empty() {
                mgr.class_prepare_req_id.take()
            } else {
                None
            };
            (removed, clear_req)
        };
        if let Some(req_id) = clear_req {
            let _ = client
                .clear_event_request(EVENT_CLASS_PREPARE, req_id)
                .await;
        }
        if removed {
            println!("Removed pending breakpoint P#{pid}.");
        } else {
            println!("Pending breakpoint P#{pid} not found.");
        }
        return Ok(());
    }

    let req_id: i32 = target
        .trim_start_matches('#')
        .parse()
        .with_context(|| format!("invalid breakpoint id '{target}'"))?;
    client.clear_event_request(EVENT_BREAKPOINT, req_id).await?;
    bp_mgr
        .lock()
        .await
        .active
        .retain(|bp| bp.request_id != req_id);
    println!("Deleted breakpoint #{req_id}.");
    Ok(())
}

fn parse_object_id(raw: &str) -> Result<u64> {
    let token = raw.rsplit_once('@').map(|(_, rhs)| rhs).unwrap_or(raw);
    let hex = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
        .unwrap_or(token);
    u64::from_str_radix(hex, 16).with_context(|| format!("invalid hex object id '{raw}'"))
}

async fn print_inspected_object(client: &JdwpClient, object_id: u64) -> Result<()> {
    match client.inspect_object(object_id, 50).await? {
        InspectedObject::Array {
            signature,
            total_length,
            elements,
        } => {
            println!(
                "{}@0x{object_id:x} (len={total_length}):",
                pretty_signature(&signature)
            );
            for (idx, val) in elements.iter().enumerate() {
                let formatted = client.format_value(val).await;
                println!("  [{idx}] = {formatted}");
            }
            if total_length > elements.len() {
                println!("  ... ({} more elements)", total_length - elements.len());
            }
        }
        InspectedObject::Object { signature, fields } => {
            let root_class = pretty_signature(&signature);
            println!("{root_class}@0x{object_id:x}:");
            if fields.is_empty() {
                println!("  (no fields)");
                return Ok(());
            }
            for f in fields {
                let formatted = client.format_value(&f.value).await;
                let owner = pretty_signature(&f.declaring_class);
                let static_prefix = if f.is_static { "static " } else { "" };
                let owner_suffix = if owner != root_class {
                    format!("  // from {owner}")
                } else {
                    String::new()
                };
                println!(
                    "  {static_prefix}{} : {} = {formatted}{owner_suffix}",
                    f.name,
                    pretty_signature(&f.signature)
                );
            }
        }
    }
    Ok(())
}

async fn describe_location(client: &JdwpClient, loc: jdwp_client::protocol::Location) -> String {
    let cls = client
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

    let src = client.source_file(loc.class_id).await.ok();
    let line = client
        .line_table(loc.class_id, loc.method_id)
        .await
        .ok()
        .and_then(|table| {
            table
                .into_iter()
                .rfind(|e| e.code_index <= loc.index)
                .map(|e| e.line_number)
        });

    match (src, line) {
        (Some(f), Some(l)) => format!("{cls}.{method_name} ({f}:{l})"),
        (Some(f), None) => format!("{cls}.{method_name} ({f})"),
        _ => format!("{cls}.{method_name} (idx={})", loc.index),
    }
}

async fn print_locals(client: &JdwpClient, thread_id: u64) -> Result<()> {
    let frames = client.thread_frames(thread_id, 0, 1).await?;
    let Some(top) = frames.first() else {
        println!("No stack frames.");
        return Ok(());
    };

    let vars = client
        .variable_table_with_generic(top.location.class_id, top.location.method_id)
        .await?;

    let active: Vec<_> = vars
        .into_iter()
        .filter(|v| {
            let start = v.code_index;
            let end = start + (v.length.max(0) as u64);
            top.location.index >= start && top.location.index <= end
        })
        .collect();

    if active.is_empty() {
        println!("(no local variables in scope at current bytecode index)");
        return Ok(());
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
        .frame_get_values(thread_id, top.frame_id, &slot_queries)
        .await?;

    for (var, val) in active.iter().zip(values.iter()) {
        let formatted = client.format_value(val).await;
        println!(
            "  {} : {} = {}",
            var.name,
            pretty_signature(&var.signature),
            formatted
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_object_id;

    #[test]
    fn parse_object_id_accepts_hex_and_formatted_refs() {
        assert_eq!(parse_object_id("0x1a2b").unwrap(), 0x1a2b);
        assert_eq!(parse_object_id("0XFF").unwrap(), 0xFF);
        assert_eq!(parse_object_id("deadbeef").unwrap(), 0xdeadbeef);
        assert_eq!(
            parse_object_id("com.example.MainActivity@0x7f01a2").unwrap(),
            0x7f01a2
        );
        assert!(parse_object_id("not_hex").is_err());
    }
}
