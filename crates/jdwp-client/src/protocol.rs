use thiserror::Error;

pub const HANDSHAKE: &[u8; 14] = b"JDWP-Handshake";
pub const HEADER_LEN: usize = 11;
pub const FLAG_REPLY: u8 = 0x80;

#[derive(Debug, Error)]
pub enum JdwpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JDWP handshake response")]
    BadHandshake,
    #[error("unexpected EOF while decoding JDWP packet")]
    UnexpectedEof,
    #[error("JDWP VM returned error code {0} ({1})")]
    VmError(u16, &'static str),
    #[error("JDWP connection closed")]
    ConnectionClosed,
    #[error("invalid UTF-8 string in JDWP payload")]
    InvalidUtf8,
    #[error("unsupported JDWP value tag: {0}")]
    UnsupportedTag(u8),
}

pub fn error_name(code: u16) -> &'static str {
    match code {
        0 => "NONE",
        10 => "INVALID_THREAD",
        11 => "INVALID_THREAD_GROUP",
        13 => "THREAD_NOT_SUSPENDED",
        14 => "THREAD_SUSPENDED",
        20 => "INVALID_OBJECT",
        21 => "INVALID_CLASS",
        22 => "CLASS_NOT_PREPARED",
        23 => "INVALID_METHODID",
        24 => "INVALID_LOCATION",
        25 => "INVALID_FIELDID",
        30 => "INVALID_FRAMEID",
        31 => "NO_MORE_FRAMES",
        32 => "OPAQUE_FRAME",
        33 => "NOT_CURRENT_FRAME",
        34 => "TYPE_MISMATCH",
        35 => "INVALID_SLOT",
        40 => "DUPLICATE",
        41 => "NOT_FOUND",
        99 => "NOT_IMPLEMENTED",
        101 => "ABSENT_INFORMATION",
        102 => "INVALID_EVENT_TYPE",
        112 => "VM_DEAD",
        _ => "UNKNOWN",
    }
}

/// Dynamic ID byte sizes reported by `VirtualMachine.IDSizes`.
#[derive(Debug, Clone, Copy)]
pub struct IdSizes {
    pub field_id: usize,
    pub method_id: usize,
    pub object_id: usize,
    pub reference_type_id: usize,
    pub frame_id: usize,
}

impl Default for IdSizes {
    fn default() -> Self {
        Self {
            field_id: 8,
            method_id: 8,
            object_id: 8,
            reference_type_id: 8,
            frame_id: 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub type_tag: u8,
    pub class_id: u64,
    pub method_id: u64,
    pub index: u64,
}

#[derive(Debug, Clone)]
pub enum JdwpValue {
    Byte(i8),
    Char(u16),
    Double(f64),
    Float(f32),
    Int(i32),
    Long(i64),
    Short(i16),
    Boolean(bool),
    Void,
    Object { tag: u8, id: u64 },
}

impl JdwpValue {
    pub fn format_short(&self) -> String {
        match self {
            JdwpValue::Byte(v) => format!("{v}"),
            JdwpValue::Char(v) => char::from_u32(*v as u32)
                .map(|c| format!("'{c}'"))
                .unwrap_or_else(|| format!("\\u{v:04x}")),
            JdwpValue::Double(v) => format!("{v}"),
            JdwpValue::Float(v) => format!("{v}f"),
            JdwpValue::Int(v) => format!("{v}"),
            JdwpValue::Long(v) => format!("{v}L"),
            JdwpValue::Short(v) => format!("{v}"),
            JdwpValue::Boolean(v) => format!("{v}"),
            JdwpValue::Void => "void".to_string(),
            JdwpValue::Object { id: 0, .. } => "null".to_string(),
            JdwpValue::Object { tag, id } => {
                let kind = match *tag {
                    b's' => "String",
                    b'[' => "Array",
                    b't' => "Thread",
                    b'c' => "Class",
                    _ => "Object",
                };
                format!("{kind}@0x{id:x}")
            }
        }
    }
}

/// Big-endian JDWP payload writer.
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }

    pub fn id(&mut self, v: u64, size: usize) {
        let bytes = v.to_be_bytes();
        let start = 8usize.saturating_sub(size);
        self.buf.extend_from_slice(&bytes[start..]);
    }

    pub fn location(&mut self, loc: &Location, sizes: &IdSizes) {
        self.u8(loc.type_tag);
        self.id(loc.class_id, sizes.reference_type_id);
        self.id(loc.method_id, sizes.method_id);
        self.u64(loc.index);
    }
}

/// Big-endian JDWP payload reader.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], JdwpError> {
        if self.pos + n > self.data.len() {
            return Err(JdwpError::UnexpectedEof);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, JdwpError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, JdwpError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn i16(&mut self) -> Result<i16, JdwpError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, JdwpError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> Result<i32, JdwpError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, JdwpError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64, JdwpError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, JdwpError> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64, JdwpError> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn string(&mut self) -> Result<String, JdwpError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| JdwpError::InvalidUtf8)
    }

    pub fn id(&mut self, size: usize) -> Result<u64, JdwpError> {
        let bytes = self.take(size)?;
        let mut buf = [0u8; 8];
        let start = 8usize.saturating_sub(size);
        buf[start..].copy_from_slice(bytes);
        Ok(u64::from_be_bytes(buf))
    }

    pub fn location(&mut self, sizes: &IdSizes) -> Result<Location, JdwpError> {
        let type_tag = self.u8()?;
        let class_id = self.id(sizes.reference_type_id)?;
        let method_id = self.id(sizes.method_id)?;
        let index = self.u64()?;
        Ok(Location {
            type_tag,
            class_id,
            method_id,
            index,
        })
    }

    pub fn tagged_value(&mut self, sizes: &IdSizes) -> Result<JdwpValue, JdwpError> {
        let tag = self.u8()?;
        match tag {
            b'B' => Ok(JdwpValue::Byte(self.u8()? as i8)),
            b'C' => Ok(JdwpValue::Char(self.u16()?)),
            b'D' => Ok(JdwpValue::Double(self.f64()?)),
            b'F' => Ok(JdwpValue::Float(self.f32()?)),
            b'I' => Ok(JdwpValue::Int(self.i32()?)),
            b'J' => Ok(JdwpValue::Long(self.i64()?)),
            b'S' => Ok(JdwpValue::Short(self.i16()?)),
            b'Z' => Ok(JdwpValue::Boolean(self.u8()? != 0)),
            b'V' => Ok(JdwpValue::Void),
            b'[' | b'L' | b's' | b't' | b'g' | b'l' | b'c' => {
                let id = self.id(sizes.object_id)?;
                Ok(JdwpValue::Object { tag, id })
            }
            other => Err(JdwpError::UnsupportedTag(other)),
        }
    }
}

pub fn encode_command(id: u32, cmd_set: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let total_len = (HEADER_LEN + payload.len()) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes());
    out.push(0x00); // command flag
    out.push(cmd_set);
    out.push(cmd);
    out.extend_from_slice(payload);
    out
}
