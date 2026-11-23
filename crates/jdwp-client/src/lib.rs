pub mod events;
pub mod protocol;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

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

pub const REF_TYPE_CLASS: u8 = 1;
pub const REF_TYPE_INTERFACE: u8 = 2;
pub const REF_TYPE_ARRAY: u8 = 3;

const ACC_STATIC: i32 = 0x0008;

/// Framework and runtime class patterns excluded by default when listening for
/// app-level `ClassPrepare` events.
pub const DEFAULT_CLASS_EXCLUDES: &[&str] = &[
    "java.*",
    "javax.*",
    "android.*",
    "androidx.*",
    "dalvik.*",
    "kotlin.*",
    "kotlinx.*",
    "com.android.*",
    "sun.*",
    "libcore.*",
    "jdk.*",
    "com.sun.*",
    "org.apache.*",
    "org.xml.*",
    "org.w3c.*",
    "org.json.*",
];

fn is_framework_signature(sig: &str) -> bool {
    sig.starts_with('[')
        || sig.starts_with("Ljava/")
        || sig.starts_with("Ljavax/")
        || sig.starts_with("Landroid/")
        || sig.starts_with("Landroidx/")
        || sig.starts_with("Ldalvik/")
        || sig.starts_with("Lkotlin/")
        || sig.starts_with("Lkotlinx/")
        || sig.starts_with("Lcom/android/")
        || sig.starts_with("Lsun/")
        || sig.starts_with("Llibcore/")
        || sig.starts_with("Ljdk/")
        || sig.starts_with("Lcom/sun/")
        || sig.starts_with("Lorg/apache/")
        || sig.starts_with("Lorg/xml/")
        || sig.starts_with("Lorg/w3c/")
        || sig.starts_with("Lorg/json/")
}

/// Derives a broad app-package root from an Android `applicationId` so flavor/build-type
/// suffixes (like `.dev` or `.native.dev`) still match the app's Kotlin/Java packages.
/// Example: `"com.everybit.sholattv.native.dev"` -> `"com.everybit"`.
pub fn root_package_prefix(package: &str) -> String {
    let mut parts = package.split('.');
    match (parts.next(), parts.next()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => format!("{a}.{b}"),
        _ => package.to_string(),
    }
}

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

#[derive(Debug, Clone)]
pub struct ObjectFieldValue {
    pub declaring_class: String,
    pub name: String,
    pub signature: String,
    pub is_static: bool,
    pub value: JdwpValue,
}

#[derive(Debug, Clone)]
pub enum InspectedObject {
    Object {
        signature: String,
        fields: Vec<ObjectFieldValue>,
    },
    Array {
        signature: String,
        total_length: usize,
        elements: Vec<JdwpValue>,
    },
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
        let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| JdwpError::HandshakeTimeout)??;
        stream.write_all(HANDSHAKE).await?;
        stream.flush().await?;

        let mut resp = [0u8; 14];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut resp))
            .await
            .map_err(|_| JdwpError::HandshakeTimeout)??;
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

    pub async fn class_superclass(&self, class_id: u64) -> Result<u64, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(class_id, sizes.reference_type_id);
        let data = self.send_command(3, 1, &w.into_bytes()).await?;
        Reader::new(&data).id(sizes.reference_type_id)
    }

    pub async fn object_get_values(
        &self,
        object_id: u64,
        field_ids: &[u64],
    ) -> Result<Vec<JdwpValue>, JdwpError> {
        if field_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(object_id, sizes.object_id);
        w.u32(field_ids.len() as u32);
        for &fid in field_ids {
            w.id(fid, sizes.field_id);
        }
        let data = self.send_command(9, 2, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(r.tagged_value(&sizes)?);
        }
        Ok(out)
    }

    pub async fn reference_type_get_values(
        &self,
        ref_type_id: u64,
        field_ids: &[u64],
    ) -> Result<Vec<JdwpValue>, JdwpError> {
        if field_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(ref_type_id, sizes.reference_type_id);
        w.u32(field_ids.len() as u32);
        for &fid in field_ids {
            w.id(fid, sizes.field_id);
        }
        let data = self.send_command(2, 6, &w.into_bytes()).await?;
        let mut r = Reader::new(&data);
        let count = r.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(r.tagged_value(&sizes)?);
        }
        Ok(out)
    }

    pub async fn array_length(&self, array_id: u64) -> Result<i32, JdwpError> {
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(array_id, sizes.object_id);
        let data = self.send_command(13, 1, &w.into_bytes()).await?;
        Reader::new(&data).i32()
    }

    pub async fn array_get_values(
        &self,
        array_id: u64,
        first_index: i32,
        length: i32,
    ) -> Result<Vec<JdwpValue>, JdwpError> {
        if length <= 0 {
            return Ok(Vec::new());
        }
        let sizes = self.id_sizes();
        let mut w = Writer::new();
        w.id(array_id, sizes.object_id);
        w.i32(first_index);
        w.i32(length);
        let data = self.send_command(13, 2, &w.into_bytes()).await?;
        Reader::new(&data).array_region(&sizes)
    }

    /// Inspects an object or array reference by walking its superclass chain and reading
    /// instance fields (via `ObjectReference.GetValues`) and static fields (via
    /// `ReferenceType.GetValues`), or array elements (via `ArrayReference.GetValues`).
    pub async fn inspect_object(
        &self,
        object_id: u64,
        max_array_elements: usize,
    ) -> Result<InspectedObject, JdwpError> {
        let (ref_type_tag, ref_type_id) = self.object_reference_type(object_id).await?;
        let signature = self.reference_type_signature(ref_type_id).await?;

        if ref_type_tag == REF_TYPE_ARRAY {
            let total_len = self.array_length(object_id).await?.max(0) as usize;
            let to_read = total_len.min(max_array_elements) as i32;
            let elements = self.array_get_values(object_id, 0, to_read).await?;
            return Ok(InspectedObject::Array {
                signature,
                total_length: total_len,
                elements,
            });
        }

        let mut instance_meta: Vec<(String, FieldInfo)> = Vec::new();
        let mut static_groups: Vec<(u64, String, Vec<FieldInfo>)> = Vec::new();

        let mut current = ref_type_id;
        for depth in 0..32 {
            if current == 0 {
                break;
            }
            let cls_sig = if depth == 0 {
                signature.clone()
            } else {
                self.reference_type_signature(current).await?
            };
            if cls_sig == "Ljava/lang/Object;" {
                break;
            }

            if let Ok(fields) = self.fields_with_generic(current).await {
                let mut statics = Vec::new();
                for f in fields {
                    if (f.mod_bits & ACC_STATIC) != 0 {
                        // Only include static fields declared directly on the object's concrete
                        // class (avoids dumping hundreds of framework constants like Context.*_SERVICE).
                        if depth == 0 {
                            statics.push(f);
                        }
                    } else {
                        instance_meta.push((cls_sig.clone(), f));
                    }
                }
                if !statics.is_empty() {
                    static_groups.push((current, cls_sig, statics));
                }
            }

            if ref_type_tag != REF_TYPE_CLASS {
                break;
            }
            current = self.class_superclass(current).await.unwrap_or(0);
        }

        let mut out_fields = Vec::with_capacity(instance_meta.len());

        if !instance_meta.is_empty() {
            let ids: Vec<u64> = instance_meta.iter().map(|(_, f)| f.field_id).collect();
            let values = self.object_get_values(object_id, &ids).await?;
            for ((declaring_class, f), value) in instance_meta.into_iter().zip(values) {
                out_fields.push(ObjectFieldValue {
                    declaring_class,
                    name: f.name,
                    signature: f.signature,
                    is_static: false,
                    value,
                });
            }
        }

        for (cls_id, declaring_class, statics) in static_groups {
            let ids: Vec<u64> = statics.iter().map(|f| f.field_id).collect();
            if let Ok(values) = self.reference_type_get_values(cls_id, &ids).await {
                for (f, value) in statics.into_iter().zip(values) {
                    out_fields.push(ObjectFieldValue {
                        declaring_class: declaring_class.clone(),
                        name: f.name,
                        signature: f.signature,
                        is_static: true,
                        value,
                    });
                }
            }
        }

        Ok(InspectedObject::Object {
            signature,
            fields: out_fields,
        })
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
        self.set_class_prepare_filtered(Some(class_pattern), &[])
            .await
    }

    /// Installs a `ClassPrepare` event request with optional `ClassMatch` (kind 5) and
    /// `ClassExclude` (kind 6) filters. Suspends the VM on match so breakpoints can be
    /// installed before the newly loaded class runs.
    pub async fn set_class_prepare_filtered(
        &self,
        class_match: Option<&str>,
        exclude_patterns: &[&str],
    ) -> Result<i32, JdwpError> {
        let mod_count = usize::from(class_match.is_some()) + exclude_patterns.len();
        let mut w = Writer::new();
        w.u8(EVENT_CLASS_PREPARE);
        w.u8(SUSPEND_ALL);
        w.u32(mod_count as u32);
        if let Some(pattern) = class_match {
            w.u8(5); // ClassMatch
            w.string(pattern);
        }
        for pattern in exclude_patterns {
            w.u8(6); // ClassExclude
            w.string(pattern);
        }
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
        w.u32(2); // Step first, then Count(1) so Count only decrements on a matching step
        w.u8(10); // Step
        w.id(thread_id, sizes.object_id);
        w.i32(1); // STEP_LINE
        w.i32(depth as i32);
        w.u8(1); // Count
        w.i32(1);
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

    /// Resolves a source file + line number inside a single known class (useful for
    /// `ClassPrepare` pending breakpoint resolution).
    pub async fn resolve_source_line_in_class(
        &self,
        ref_type_tag: u8,
        type_id: u64,
        signature: &str,
        file_name: &str,
        target_line: i32,
    ) -> Result<Vec<ResolvedLocation>, JdwpError> {
        let Ok(src) = self.source_file(type_id).await else {
            return Ok(Vec::new());
        };
        if !src.eq_ignore_ascii_case(file_name) && !src.ends_with(file_name) {
            return Ok(Vec::new());
        }

        let Ok(methods) = self.methods_with_generic(type_id).await else {
            return Ok(Vec::new());
        };
        let mut matches = Vec::new();
        for m in methods {
            let Ok(lines) = self.line_table(type_id, m.method_id).await else {
                continue;
            };
            if let Some(entry) = lines.iter().find(|e| e.line_number == target_line) {
                matches.push(ResolvedLocation {
                    location: Location {
                        type_tag: ref_type_tag,
                        class_id: type_id,
                        method_id: m.method_id,
                        index: entry.code_index,
                    },
                    class_signature: signature.to_string(),
                    method_name: m.name,
                    source_file: src.clone(),
                    line_number: entry.line_number,
                });
            }
        }
        Ok(matches)
    }

    /// Resolves a human-readable location (`File.kt` + line number) across loaded classes.
    ///
    /// Uses a fast 2-pass strategy to avoid thousands of sequential `SourceFile` RPCs:
    /// 1. Check non-framework classes whose class signature contains the file stem
    ///    (e.g. `MainActivity` for `MainActivity.kt`).
    /// 2. If pass 1 finds nothing, check remaining loaded classes under `package_prefix`
    ///    (or its 2-segment root prefix when `applicationIdSuffix` differs from the code package).
    pub async fn resolve_source_line(
        &self,
        package_prefix: Option<&str>,
        file_name: &str,
        target_line: i32,
    ) -> Result<Vec<ResolvedLocation>, JdwpError> {
        let classes = self.all_classes_with_generic().await?;
        let stem = file_name
            .rsplit('/')
            .next()
            .unwrap_or(file_name)
            .split('.')
            .next()
            .unwrap_or(file_name)
            .to_ascii_lowercase();

        let non_framework: Vec<&ClassInfo> = classes
            .iter()
            .filter(|c| !is_framework_signature(&c.signature))
            .collect();

        // Pass 1: fast filename-stem match (handles 95%+ of Java/Kotlin files in a few RPCs)
        let mut matches = Vec::new();
        if !stem.is_empty() {
            for cls in &non_framework {
                if cls.signature.to_ascii_lowercase().contains(&stem) {
                    matches.extend(
                        self.resolve_source_line_in_class(
                            cls.ref_type_tag,
                            cls.type_id,
                            &cls.signature,
                            file_name,
                            target_line,
                        )
                        .await?,
                    );
                }
            }
            if !matches.is_empty() {
                return Ok(matches);
            }
        }

        // Pass 2: classes whose filename differs from class name (e.g. multi-class Kotlin files)
        let root_sig = package_prefix.map(|p| {
            let root = root_package_prefix(p);
            format!("L{}/", root.replace('.', "/"))
        });

        for cls in &non_framework {
            if !stem.is_empty() && cls.signature.to_ascii_lowercase().contains(&stem) {
                continue; // already checked in pass 1
            }
            if let Some(ref prefix) = root_sig {
                if !cls.signature.starts_with(prefix) {
                    continue;
                }
            }
            matches.extend(
                self.resolve_source_line_in_class(
                    cls.ref_type_tag,
                    cls.type_id,
                    &cls.signature,
                    file_name,
                    target_line,
                )
                .await?,
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::JdwpEvent;
    use tokio::net::TcpListener;

    fn encode_reply(id: u32, error_code: u16, payload: &[u8]) -> Vec<u8> {
        let total_len = (HEADER_LEN + payload.len()) as u32;
        let mut out = Vec::with_capacity(total_len as usize);
        out.extend_from_slice(&total_len.to_be_bytes());
        out.extend_from_slice(&id.to_be_bytes());
        out.push(FLAG_REPLY);
        out.extend_from_slice(&error_code.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn pretty_signature_formats_class_and_leaves_primitives() {
        assert_eq!(
            pretty_signature("Lcom/example/MainActivity;"),
            "com.example.MainActivity"
        );
        assert_eq!(pretty_signature("I"), "I");
        assert_eq!(
            pretty_signature("[Ljava/lang/String;"),
            "[Ljava/lang/String;"
        );
    }

    #[tokio::test]
    async fn mock_jdwp_server_4byte_ids_inspect_and_events() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let sizes = IdSizes {
            field_id: 4,
            method_id: 4,
            object_id: 4,
            reference_type_id: 4,
            frame_id: 4,
        };

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut hs = [0u8; 14];
            sock.read_exact(&mut hs).await.unwrap();
            assert_eq!(&hs, HANDSHAKE);
            sock.write_all(HANDSHAKE).await.unwrap();

            let mut header = [0u8; HEADER_LEN];
            while sock.read_exact(&mut header).await.is_ok() {
                let len = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
                let id = u32::from_be_bytes(header[4..8].try_into().unwrap());
                let cmd_set = header[9];
                let cmd = header[10];
                let mut payload = vec![0u8; len.saturating_sub(HEADER_LEN)];
                if !payload.is_empty() {
                    sock.read_exact(&mut payload).await.unwrap();
                }

                let mut w = Writer::new();
                match (cmd_set, cmd) {
                    // VirtualMachine.IDSizes
                    (1, 7) => {
                        w.i32(4);
                        w.i32(4);
                        w.i32(4);
                        w.i32(4);
                        w.i32(4);
                    }
                    // ObjectReference.ReferenceType
                    (9, 1) => {
                        w.u8(REF_TYPE_CLASS);
                        w.id(0x200, sizes.reference_type_id);
                    }
                    // ReferenceType.Signature
                    (2, 1) => {
                        let ref_id = Reader::new(&payload).id(sizes.reference_type_id).unwrap();
                        match ref_id {
                            0x200 => w.string("Lcom/example/Child;"),
                            0x100 => w.string("Lcom/example/Base;"),
                            _ => w.string("Ljava/lang/Object;"),
                        }
                    }
                    // ClassType.Superclass
                    (3, 1) => {
                        let ref_id = Reader::new(&payload).id(sizes.reference_type_id).unwrap();
                        let super_id = match ref_id {
                            0x200 => 0x100,
                            0x100 => 0x050,
                            _ => 0,
                        };
                        w.id(super_id, sizes.reference_type_id);
                    }
                    // ReferenceType.FieldsWithGeneric
                    (2, 14) => {
                        let ref_id = Reader::new(&payload).id(sizes.reference_type_id).unwrap();
                        match ref_id {
                            0x200 => {
                                w.u32(2);
                                // instance field
                                w.id(0xF1, sizes.field_id);
                                w.string("count");
                                w.string("I");
                                w.string("");
                                w.i32(0x0002); // private
                                               // static field
                                w.id(0xF2, sizes.field_id);
                                w.string("TAG");
                                w.string("Ljava/lang/String;");
                                w.string("");
                                w.i32(ACC_STATIC);
                            }
                            0x100 => {
                                w.u32(1);
                                w.id(0xF3, sizes.field_id);
                                w.string("enabled");
                                w.string("Z");
                                w.string("");
                                w.i32(0x0004); // protected
                            }
                            _ => w.u32(0),
                        }
                    }
                    // ObjectReference.GetValues
                    (9, 2) => {
                        w.u32(2);
                        w.u8(b'I');
                        w.i32(42);
                        w.u8(b'Z');
                        w.u8(1);
                    }
                    // ReferenceType.GetValues (static fields)
                    (2, 6) => {
                        w.u32(1);
                        w.u8(b's');
                        w.id(0x999, sizes.object_id);
                    }
                    // EventRequest.Set -> return req_id=55 and push a ClassPrepare event
                    (15, 1) => {
                        w.i32(55);
                        let reply = encode_reply(id, 0, &w.into_bytes());
                        sock.write_all(&reply).await.unwrap();

                        let mut ev = Writer::new();
                        ev.u8(SUSPEND_ALL);
                        ev.u32(1);
                        ev.u8(EVENT_CLASS_PREPARE);
                        ev.i32(55);
                        ev.id(0x77, sizes.object_id);
                        ev.u8(REF_TYPE_CLASS);
                        ev.id(0x300, sizes.reference_type_id);
                        ev.string("Lcom/example/LazyScreen;");
                        ev.i32(7);
                        let ev_pkt = encode_command(999, 64, 100, &ev.into_bytes());
                        sock.write_all(&ev_pkt).await.unwrap();
                        continue;
                    }
                    _ => {}
                }
                let reply = encode_reply(id, 0, &w.into_bytes());
                sock.write_all(&reply).await.unwrap();
            }
        });

        let client = JdwpClient::connect(&addr).await.unwrap();
        assert_eq!(client.id_sizes(), sizes);

        let inspected = client.inspect_object(0xABCD, 50).await.unwrap();
        match inspected {
            InspectedObject::Object { signature, fields } => {
                assert_eq!(signature, "Lcom/example/Child;");
                assert_eq!(fields.len(), 3);
                assert_eq!(fields[0].name, "count");
                assert_eq!(fields[0].value, JdwpValue::Int(42));
                assert!(!fields[0].is_static);
                assert_eq!(fields[1].name, "enabled");
                assert_eq!(fields[1].declaring_class, "Lcom/example/Base;");
                assert_eq!(fields[1].value, JdwpValue::Boolean(true));
                assert_eq!(fields[2].name, "TAG");
                assert!(fields[2].is_static);
                assert_eq!(
                    fields[2].value,
                    JdwpValue::Object {
                        tag: b's',
                        id: 0x999
                    }
                );
            }
            _ => panic!("expected InspectedObject::Object"),
        }

        let mut rx = client.subscribe_events();
        let req_id = client
            .set_class_prepare_filtered(None, DEFAULT_CLASS_EXCLUDES)
            .await
            .unwrap();
        assert_eq!(req_id, 55);

        let comp = rx.recv().await.unwrap();
        assert_eq!(comp.suspend_policy, SUSPEND_ALL);
        assert_eq!(
            comp.events,
            vec![JdwpEvent::ClassPrepare {
                request_id: 55,
                thread_id: 0x77,
                ref_type_tag: REF_TYPE_CLASS,
                type_id: 0x300,
                signature: "Lcom/example/LazyScreen;".to_string(),
                status: 7,
            }]
        );

        drop(client);
        let _ = server.await;
    }
}
