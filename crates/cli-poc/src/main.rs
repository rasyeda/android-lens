use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jdwp_client::events::JdwpEvent;
use jdwp_client::{pretty_signature, JdwpClient, StepDepth};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "android-lens-poc", about = "Lightweight pure-Rust Android JDWP debugger PoC")]
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
    /// Attach to a running debuggable Android app via ADB + JDWP
    Attach {
        /// Android application package name (e.g. com.example.app)
        #[arg(short, long)]
        package: String,
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
            serial,
            port,
            initial_break,
        } => {
            run_attach(&package, serial.as_deref(), port, initial_break.as_deref()).await?;
        }
    }

    Ok(())
}

async fn run_attach(
    package: &str,
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

    let pid = adb_bridge::find_package_pid(&dev.serial, package)
        .await
        .with_context(|| format!("is '{package}' running on {}?", dev.serial))?;
    println!("Found {package} (PID {pid})");

    let guard = adb_bridge::forward_jdwp(&dev.serial, port, pid).await?;
    println!(
        "Forwarded {} -> jdwp:{pid} on {}",
        guard.local_addr(),
        guard.serial()
    );

    let client = Arc::new(
        JdwpClient::connect(&guard.local_addr())
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

    let paused_thread: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

    // Spawn event listener task
    let mut events_rx = client.subscribe_events();
    let client_ev = Arc::clone(&client);
    let paused_ev = Arc::clone(&paused_thread);
    tokio::spawn(async move {
        while let Ok(comp) = events_rx.recv().await {
            for ev in comp.events {
                match ev {
                    JdwpEvent::Breakpoint {
                        request_id,
                        thread_id,
                        location,
                    } => {
                        *paused_ev.lock().await = Some(thread_id);
                        let loc_str = describe_location(&client_ev, location).await;
                        let tname = client_ev
                            .thread_name(thread_id)
                            .await
                            .unwrap_or_else(|_| format!("thread@0x{thread_id:x}"));
                        println!(
                            "\n[BREAKPOINT #{request_id}] paused on thread '{tname}' at {loc_str}"
                        );
                        println!("Type `bt`, `locals`, `n` (step over), `s` (into), `o` (out), or `c` (resume).");
                    }
                    JdwpEvent::SingleStep {
                        thread_id,
                        location,
                        ..
                    } => {
                        *paused_ev.lock().await = Some(thread_id);
                        let loc_str = describe_location(&client_ev, location).await;
                        println!("\n[STEP] at {loc_str}");
                    }
                    JdwpEvent::VmDeath { .. } => {
                        println!("\n[VM_DEATH] Target app exited.");
                    }
                    _ => {}
                }
            }
        }
    });

    if let Some(spec) = initial_break {
        set_breakpoint_from_spec(&client, package, spec).await?;
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
                println!("  classes [filter]  List loaded classes (default: app package)");
                println!("  b <File.kt:line>  Set line breakpoint");
                println!("  threads           List threads");
                println!("  bt                Print stack trace of paused thread");
                println!("  locals            Print local variables of top frame");
                println!("  n | over          Step over");
                println!("  s | into          Step into");
                println!("  o | out           Step out");
                println!("  c | resume        Resume VM");
                println!("  pause             Suspend VM");
                println!("  q | quit          Resume & exit");
            }
            "classes" => {
                let filter = parts
                    .get(1)
                    .copied()
                    .unwrap_or(package)
                    .replace('.', "/");
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
                if let Err(e) = set_breakpoint_from_spec(&client, package, spec).await {
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
                match client.thread_frames(tid, 0, 20).await {
                    Ok(frames) => {
                        for (i, f) in frames.iter().enumerate() {
                            let desc = describe_location(&client, f.location).await;
                            println!("  #{i:<2} {desc}");
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

async fn step_command(
    client: &JdwpClient,
    paused_thread: &Arc<Mutex<Option<u64>>>,
    depth: StepDepth,
) {
    let tid = *paused_thread.lock().await;
    let Some(tid) = tid else {
        println!("No paused thread to step.");
        return;
    };
    if let Err(e) = client.set_single_step(tid, depth).await {
        println!("Failed to request step: {e}");
        return;
    }
    if let Err(e) = client.resume_vm().await {
        println!("Failed to resume for step: {e}");
    }
}

async fn set_breakpoint_from_spec(
    client: &JdwpClient,
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
    let mut matches = client
        .resolve_source_line(Some(package), file, line)
        .await?;
    if matches.is_empty() {
        // Retry across all non-framework classes (e.g. if app module package differs from applicationId)
        matches = client.resolve_source_line(None, file, line).await?;
    }
    if matches.is_empty() {
        bail!("no loaded class matched {file}:{line} (make sure the screen/class is loaded)");
    }

    for m in matches {
        let req_id = client.set_breakpoint(&m.location).await?;
        println!(
            "Breakpoint #{req_id} set at {}.{} ({}:{})",
            pretty_signature(&m.class_signature),
            m.method_name,
            m.source_file,
            m.line_number
        );
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
                .filter(|e| e.code_index <= loc.index)
                .last()
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
        .map(|v| (v.slot, v.signature.as_bytes().first().copied().unwrap_or(b'L')))
        .collect();

    let values = client
        .frame_get_values(thread_id, top.frame_id, &slot_queries)
        .await?;

    for (var, val) in active.iter().zip(values.iter()) {
        let formatted = client.format_value(val).await;
        println!("  {} : {} = {}", var.name, pretty_signature(&var.signature), formatted);
    }
    Ok(())
}
