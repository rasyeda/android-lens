pub mod events;
pub mod protocol;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::events::{
    parse_composite_event, CompositeEvent, EVENT_BREAKPOINT, EVENT_CLASS_PREPARE,
    EVENT_SINGLE_STEP, SUSPEND_ALL,
};
use crate::protocol::{
    encode_command, error_name, IdSizes, JdwpError, JdwpValue, Location, Reader, Writer,
    FLAG_REPLY, HANDSHAKE, HEADER_LEN,
};

#[derive(Debug, Clone)]
pub struct VmVersion {
    pub description: String,
    pub jdwp_major: i32,
    pub jdwp_minor: i32,
    pub vm_version: String,
    pub vm_name: String,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub ref_type_tag: u8,
    pub type_id: u64,
    pub signature: String,
    pub generic_signature: String,
    pub status: i32,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub method_id: u64,
    pub name: String,
    pub signature: String,
    pub generic_signature: String,
    pub mod_bits: i32,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub field_id: u64,
    pub name: String,
    pub signature: String,
    pub generic_signature: String,
    pub mod_bits: i32,
}

#[derive(Debug, Clone)]
pub struct LineEntry {
    pub code_index: u64,
    pub line_number: i32,
}

#[derive(Debug, Clone)]
pub struct VariableSlot {
    pub code_index: u64,
    pub name: String,
    pub signature: String,
    pub generic_signature: String,
    pub length: i32,
    pub slot: i32,
}

#[derive(Debug, Clone)]
pub struct StackFrameInfo {
    pub frame_id: u64,
    pub location: Location,
}

#[derive(Debug, Clone)]
pub struct ResolvedLocation {
    pub location: Location,
    pub class_signature: String,
    pub method_name: String,
    pub source_file: String,
    pub line_number: i32,
}

#[derive(Debug, Clone, Copy)]
pub enum StepDepth {
    Into = 0,
    Over = 1,
    Out = 2,
}

type PendingMap = Arc<Mutex<HashMap<u32, oneshot::Sender<Result<Vec<u8>, JdwpError>>>>>;

pub struct JdwpClient {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    next_id: AtomicU32,
    pending: PendingMap,
    id_sizes: Arc<RwLock<IdSizes>>,
    events_tx: broadcast::Sender<CompositeEvent>,
}

impl JdwpClient {
    /// Connects to a JDWP TCP endpoint (e.g., `127.0.0.1:5005`), performs the handshake,
    /// spawns the async demux reader, and queries `VirtualMachine.IDSizes`.
    pub async fn connect(addr: &str) -> Result<Self, JdwpError> {
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(HANDSHAKE).await?;
        stream.flush().await?;

        let mut resp = [0u8; 14];
        stream.read_exact(&mut resp).await?;
        if &resp != HANDSHAKE {
            return Err(JdwpError::BadHandshake);
        }

        let (mut reader, writer) = stream.into_split();
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let id_sizes = Arc::new(RwLock::new(IdSizes::default()));
        let (events_tx, _) = broadcast::channel(128);

        let pending_bg = Arc::clone(&pending);
        let sizes_bg = Arc::clone(&id_sizes);
        let events_bg = events_tx.clone();

        tokio::spawn(async move {
            let mut header = [0u8; HEADER_LEN];
            loop {
                if reader.read_exact(&mut header).await.is_err() {
                    break;
                }
                let length = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
                let id = u32::from_be_bytes(header[4..8].try_into().unwrap());
                let flags = header[8];
                let payload_len = length.saturating_sub(HEADER_LEN);
                let mut payload = vec![0u8; payload_len];
                if payload_len > 0 && reader.read_exact(&mut payload).await.is_err() {
                    break;
                }

                if (flags & FLAG_REPLY) != 0 {
                    let error_code = u16::from_be_bytes([header[9], header[10]]);
                    let tx = pending_bg.lock().await.remove(&id);
                    if let Some(tx) = tx {
                        let res = if error_code == 0 {
                            Ok(payload)
                        } else {
                            Err(JdwpError::VmError(error_code, error_name(error_code)))
                        };
                        let _ = tx.send(res);
                    }
                } else {
                    let cmd_set = header[9];
                    let cmd = header[10];
                    if cmd_set == 64 && cmd == 100 {
                        let sizes = *sizes_bg.read().unwrap();
                        if let Ok(comp) = parse_composite_event(&payload, &sizes) {
                            let _ = events_bg.send(comp);
                        }
                    }
                }
            }
            // Fail any remaining waiters on disconnect
            let mut map = pending_bg.lock().await;
            for (_, tx) in map.drain() {
                let _ = tx.send(Err(JdwpError::ConnectionClosed));
            }
        });

        let client = Self {
            writer: Arc::new(Mutex::new(writer)),
            next_id: AtomicU32::new(1),
            pending,
            id_sizes,
            events_tx,
        };

        // Immediately fetch dynamic ID sizes from ART
        let sizes = client.fetch_id_sizes().await?;
        *client.id_sizes.write().unwrap() = sizes;

        Ok(client)
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<CompositeEvent> {
        self.events_tx.subscribe()
    }

    pub fn id_sizes(&self) -> IdSizes {
        *self.id_sizes.read().unwrap()
    }

    async fn send_command(
        &self,
        cmd_set: u8,
        cmd: u8,
        payload: &[u8],
    ) -> Result<Vec<u8>, JdwpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let pkt = encode_command(id, cmd_set, cmd, payload);
        {
            let mut w = self.writer.lock().await;
            w.write_all(&pkt).await?;
            w.flush().await?;
        }

        rx.await.map_err(|_| JdwpError::ConnectionClosed)?
    }

    async fn fetch_id_sizes(&self) -> Result<IdSizes, JdwpError> {
        let data = self.send_command(1, 7, &[]).await?;
        let mut r = Reader::new(&data);
        Ok(IdSizes {
            field_id: r.i32()? as usize,
            method_id: r.i32()? as usize,
            object_id: r.i32()? as usize,
            reference_type_id: r.i32()? as usize,
            frame_id: r.i32()? as usize,
        })
    }

    pub async fn version(&self) -> Result<VmVersion, JdwpError> {
        let data = self.send_command(1, 1, &[]).await?;
        let mut r = Reader::new(&data);
        Ok(VmVersion {
            description: r.string()?,
            jdwp_major: r.i32()?,
            jdwp_minor: r.i32()?,
            vm_version: r.string()?,
            vm_name: r.string()?,
        })
    }

    pub async fn all_classes_with_generic(&self) -> Result<Vec<ClassInfo>, JdwpError> {
        let sizes = self.id_sizes();
        let data = self.send_command(1, 20, &[]).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut classes = Vec::with_capacity(count);
        for _ in 0..count {
            classes.push(ClassInfo {
                ref_type_tag: r.u8()?,
                type_id: r.id(sizes.reference_type_id)?,
                signature: r.string()?,
                generic_signature: r.string()?,
                status: r.i32()?,
            });
        }
        Ok(classes)
    }

    pub async fn all_threads(&self) -> Result<Vec<u64>, JdwpError> {
        let sizes = self.id_sizes();
        let data = self.send_command(1, 4, &[]).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut threads = Vec::with_capacity(count);
        for _ in 0..count {
            threads.push(r.id(sizes.object_id)?);
        }
        Ok(threads)
    }

    pub async fn suspend_vm(&self) -> Result<(), JdwpError> {
        self.send_command(1, 8, &[]).await?;
        Ok(())
    }

    pub async fn resume_vm(&self) -> Result<(), JdwpError> {
        self.send_command(1, 9, &[]).await?;
        Ok(())
    }

    pub async fn reference_type_signature(&self, ref_type_id: u64) -> Result<String, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        let data = self.send_command(2, 1, &w.into_bytes()).await?;
        Reader::new(&data).string()
    }

    pub async fn source_file(&self, ref_type_id: u64) -> Result<String, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        let data = self.send_command(2, 7, &w.into_bytes()).await?;
        Reader::new(&data).string()
    }

    pub async fn fields_with_generic(&self, ref_type_id: u64) -> Result<Vec<FieldInfo>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        let data = self.send_command(2, 14, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(FieldInfo {
                field_id: r.id(sizes.field_id)?,
                name: r.string()?,
                signature: r.string()?,
                generic_signature: r.string()?,
                mod_bits: r.i32()?,
            });
        }
        Ok(out)
    }

    pub async fn methods_with_generic(
        &self,
        ref_type_id: u64,
    ) -> Result<Vec<MethodInfo>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        let data = self.send_command(2, 15, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(MethodInfo {
                method_id: r.id(sizes.method_id)?,
                name: r.string()?,
                signature: r.string()?,
                generic_signature: r.string()?,
                mod_bits: r.i32()?,
            });
        }
        Ok(out)
    }

    pub async fn line_table(
        &self,
        ref_type_id: u64,
        method_id: u64,
    ) -> Result<Vec<LineEntry>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        w.id(method_id, sizes.method_id);
        let data = self.send_command(6, 1, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let _start = r.i64()?;
        let _end = r.i64()?;
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(LineEntry {
                code_index: r.u64()?,
                line_number: r.i32()?,
            });
        }
        Ok(out)
    }

    pub async fn variable_table_with_generic(
        &self,
        ref_type_id: u64,
        method_id: u64,
    ) -> Result<Vec<VariableSlot>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        w.id(method_id, sizes.method_id);
        let data = self.send_command(6, 5, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let _arg_cnt = r.i32()?;
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(VariableSlot {
                code_index: r.u64()?,
                name: r.string()?,
                signature: r.string()?,
                generic_signature: r.string()?,
                length: r.i32()?,
                slot: r.i32()?,
            });
        }
        Ok(out)
    }

    pub async fn thread_name(&self, thread_id: u64) -> Result<String, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(thread_id, sizes.object_id);
        let data = self.send_command(11, 1, &w.into_bytes()).await?;
        Reader::new(&data).string()
    }

    pub async fn thread_frames(
        &self,
        thread_id: u64,
        start_frame: i32,
        length: i32,
    ) -> Result<Vec<StackFrameInfo>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(thread_id, sizes.object_id);
        w.i32(start_frame);
        w.i32(length);
        let data = self.send_command(11, 6, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(StackFrameInfo {
                frame_id: r.id(sizes.frame_id)?,
                location: r.location(&sizes)?,
            });
        }
        Ok(out)
    }

    pub async fn frame_get_values(
        &self,
        thread_id: u64,
        frame_id: u64,
        slots: &[(i32, u8)],
    ) -> Result<Vec<JdwpValue>, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(thread_id, sizes.object_id);
        w.id(frame_id, sizes.frame_id);
        w.u32(slots.len() as u32);
        for &(slot, sigbyte) in slots {
            w.i32(slot);
            w.u8(sigbyte);
        }
        let data = self.send_command(16, 1, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(r.tagged_value(&sizes)?);
        }
        Ok(out)
    }

    pub async fn string_value(&self, object_id: u64) -> Result<String, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(object_id, sizes.object_id);
        let data = self.send_command(10, 1, &w.into_bytes()).await?;
        Reader::new(&data).string()
    }

    pub async fn object_reference_type(&self, object_id: u64) -> Result<(u8, u64), JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(object_id, sizes.object_id);
        let data = self.send_command(9, 1, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        Ok((r.u8()?, r.id(sizes.reference_type_id)?))
    }

    pub async fn array_length(&self, array_id: u64) -> Result<i32, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(array_id, sizes.object_id);
        let data = self.send_command(13, 1, &w.into_bytes()).await?;
        Reader::new(&data).i32()
    }

    pub async fn set_breakpoint(&self, loc: &Location) -> Result<i32, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.u8(EVENT_BREAKPOINT);
        w.u8(SUSPEND_ALL);
        w.u32(1); // 1 modifier: LocationOnly
        w.u8(7); // LocationOnly
        w.location(loc, &sizes);
        let data = self.send_command(15, 1, &w.into_bytes()).await?;
        Reader::new(&data).i32()
    }

    pub async fn set_class_prepare(&self, class_pattern: &str) -> Result<i32, JdwpError> {
        let mut w = Writer::new();
        w.u8(EVENT_CLASS_PREPARE);
        w.u8(SUSPEND_ALL);
        w.u32(1);
        w.u8(5); // ClassMatch
        w.string(class_pattern);
        let data = self.send_command(15, 1, &w.into_bytes()).await?;
        Reader::new(&data).i32()
    }

    pub async fn set_single_step(
        &self,
        thread_id: u64,
        depth: StepDepth,
    ) -> Result<i32, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.u8(EVENT_SINGLE_STEP);
        w.u8(SUSPEND_ALL);
        w.u32(2); // Count(1) + Step
        w.u8(1); // Count
        w.i32(1);
        w.u8(10); // Step
        w.id(thread_id, sizes.object_id);
        w.i32(1); // STEP_LINE
        w.i32(depth as i32);
        let data = self.send_command(15, 1, &w.into_bytes()).await?;
        Reader::new(&data).i32()
    }

    pub async fn clear_event_request(
        &self,
        event_kind: u8,
        request_id: i32,
    ) -> Result<(), JdwpError> {
        let mut w = Writer::new();
        w.u8(event_kind);
        w.i32(request_id);
        self.send_command(15, 2, &w.into_bytes()).await?;
        Ok(())
    }

    /// Resolves a human-readable location (`File.kt` + line number) across loaded classes
    /// filtered by an optional package prefix (e.g. `com/example/app`).
    pub async fn resolve_source_line(
        &self,
        package_prefix: Option<&str>,
        file_name: &str,
        target_line: i32,
    ) -> Result<Vec<ResolvedLocation>, JdwpError> {
        let classes = self.all_classes_with_generic().await?;
        let pkg_sig = package_prefix.map(|p| format!("L{}/", p.replace('.', "/")));
        let mut matches = Vec::new();

        for cls in classes {
            if let Some(ref prefix) = pkg_sig {
                if !cls.signature.starts_with(prefix) {
                    continue;
                }
            } else if cls.signature.starts_with("Ljava/")
                || cls.signature.starts_with("Ljavax/")
                || cls.signature.starts_with("Landroid/")
                || cls.signature.starts_with("Landroidx/")
                || cls.signature.starts_with("Ldalvik/")
                || cls.signature.starts_with("Lkotlin/")
                || cls.signature.starts_with("Lkotlinx/")
                || cls.signature.starts_with("[")
            {
                continue;
            }

            let Ok(src) = self.source_file(cls.type_id).await else {
                continue;
            };
            if !src.eq_ignore_ascii_case(file_name) && !src.ends_with(file_name) {
                continue;
            }

            let Ok(methods) = self.methods_with_generic(cls.type_id).await else {
                continue;
            };
            for m in methods {
                let Ok(lines) = self.line_table(cls.type_id, m.method_id).await else {
                    continue;
                };
                if let Some(entry) = lines.iter().find(|e| e.line_number == target_line) {
                    matches.push(ResolvedLocation {
                        location: Location {
                            type_tag: cls.ref_type_tag,
                            class_id: cls.type_id,
                            method_id: m.method_id,
                            index: entry.code_index,
                        },
                        class_signature: cls.signature.clone(),
                        method_name: m.name,
                        source_file: src.clone(),
                        line_number: entry.line_number,
                    });
                }
            }
        }
        Ok(matches)
    }

    /// Formats a `JdwpValue` nicely, resolving `String` contents and object class names.
    pub async fn format_value(&self, val: &JdwpValue) -> String {
        match val {
            JdwpValue::Object { tag, id } if *id != 0 => {
                if *tag == b's' {
                    if let Ok(s) = self.string_value(*id).await {
                        return format!("\"{s}\"");
                    }
                }
                if *tag == b'[' {
                    if let Ok(len) = self.array_length(*id).await {
                        return format!("Array(len={len})@0x{id:x}");
                    }
                }
                if let Ok((_, ref_id)) = self.object_reference_type(*id).await {
                    if let Ok(sig) = self.reference_type_signature(ref_id).await {
                        let pretty = pretty_signature(&sig);
                        return format!("{pretty}@0x{id:x}");
                    }
                }
                val.format_short()
            }
            _ => val.format_short(),
        }
    }
}

/// Converts JVM signature like `Lcom/example/MainActivity;` -> `com.example.MainActivity`.
pub fn pretty_signature(sig: &str) -> String {
    if let Some(inner) = sig.strip_prefix('L').and_then(|s| s.strip_suffix(';')) {
        inner.replace('/', ".")
    } else {
        sig.to_string()
    }
}
