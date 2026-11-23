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
    #[error("JDWP handshake timed out (is another debugger already attached?)")]
    HandshakeTimeout,
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
        500 => "INVALID_INDEX",
        502 => "INVALID_ARRAY",
        503 => "INVALID_STRING",
        504 => "INVALID_LENGTH",
        506 => "INVALID_TAG",
        508 => "INVALID_CLASS_LOADER",
        _ => "UNKNOWN",
    }
}

/// Dynamic ID byte sizes reported by `VirtualMachine.IDSizes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq)]
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
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
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

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn f64(&mut self, v: f64) {
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

    pub fn untagged_value(&mut self, tag: u8, sizes: &IdSizes) -> Result<JdwpValue, JdwpError> {
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

    pub fn tagged_value(&mut self, sizes: &IdSizes) -> Result<JdwpValue, JdwpError> {
        let tag = self.u8()?;
        self.untagged_value(tag, sizes)
    }

    /// Decodes a JDWP `arrayregion` returned by `ArrayReference.GetValues`.
    /// Primitive arrays store untagged values; object arrays store tagged object IDs.
    pub fn array_region(&mut self, sizes: &IdSizes) -> Result<Vec<JdwpValue>, JdwpError> {
        let component_tag = self.u8()?;
        let length = self.u32()? as usize;
        let is_primitive = matches!(
            component_tag,
            b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V'
        );
        let mut values = Vec::with_capacity(length);
        for _ in 0..length {
            if is_primitive {
                values.push(self.untagged_value(component_tag, sizes)?);
            } else {
                values.push(self.tagged_value(sizes)?);
            }
        }
        Ok(values)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes_4() -> IdSizes {
        IdSizes {
            field_id: 4,
            method_id: 4,
            object_id: 4,
            reference_type_id: 4,
            frame_id: 4,
        }
    }

    fn sizes_8() -> IdSizes {
        IdSizes::default()
    }

    #[test]
    fn writer_reader_primitives_and_strings_round_trip() {
        let mut w = Writer::new();
        w.u8(0xAB);
        w.u16(0x1234);
        w.i16(-1234);
        w.u32(0xDEAD_BEEF);
        w.i32(-42);
        w.u64(0x0123_4567_89AB_CDEF);
        w.i64(-987_654_321_012);
        w.f32(3.5);
        w.f64(-123.75);
        w.string("");
        w.string("hello kotlin \u{1F50D}");

        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.i16().unwrap(), -1234);
        assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.i32().unwrap(), -42);
        assert_eq!(r.u64().unwrap(), 0x0123_4567_89AB_CDEF);
        assert_eq!(r.i64().unwrap(), -987_654_321_012);
        assert_eq!(r.f32().unwrap(), 3.5);
        assert_eq!(r.f64().unwrap(), -123.75);
        assert_eq!(r.string().unwrap(), "");
        assert_eq!(r.string().unwrap(), "hello kotlin \u{1F50D}");
        assert!(matches!(r.u8(), Err(JdwpError::UnexpectedEof)));
    }

    #[test]
    fn id_and_location_round_trip_4_byte_vs_8_byte() {
        let loc4 = Location {
            type_tag: 1,
            class_id: 0x1122_3344,
            method_id: 0x5566_7788,
            index: 99,
        };
        let mut w4 = Writer::new();
        w4.location(&loc4, &sizes_4());
        let b4 = w4.into_bytes();
        // 1 (tag) + 4 (class) + 4 (method) + 8 (index) = 17
        assert_eq!(b4.len(), 17);
        assert_eq!(Reader::new(&b4).location(&sizes_4()).unwrap(), loc4);

        let loc8 = Location {
            type_tag: 1,
            class_id: 0x1122_3344_5566_7788,
            method_id: 0x99AA_BBCC_DDEE_FF00,
            index: 1234,
        };
        let mut w8 = Writer::new();
        w8.location(&loc8, &sizes_8());
        let b8 = w8.into_bytes();
        // 1 (tag) + 8 (class) + 8 (method) + 8 (index) = 25
        assert_eq!(b8.len(), 25);
        assert_eq!(Reader::new(&b8).location(&sizes_8()).unwrap(), loc8);

        // Mixed ID sizes (e.g. 4-byte method_id, 8-byte reference_type_id)
        let mixed = IdSizes {
            field_id: 4,
            method_id: 4,
            object_id: 8,
            reference_type_id: 8,
            frame_id: 8,
        };
        let loc_mixed = Location {
            type_tag: 2,
            class_id: 0x0000_0001_0000_0002,
            method_id: 0x0000_0000_1234_5678,
            index: 7,
        };
        let mut wm = Writer::new();
        wm.location(&loc_mixed, &mixed);
        let bm = wm.into_bytes();
        assert_eq!(bm.len(), 21);
        assert_eq!(Reader::new(&bm).location(&mixed).unwrap(), loc_mixed);
    }

    #[test]
    fn tagged_values_all_types_4_and_8_byte() {
        for sizes in [sizes_4(), sizes_8()] {
            let obj_id = if sizes.object_id == 4 {
                0xCAFE_BABE
            } else {
                0x0011_2233_CAFE_BABE
            };

            let mut w = Writer::new();
            w.u8(b'B');
            w.u8((-5i8) as u8);
            w.u8(b'C');
            w.u16('A' as u16);
            w.u8(b'D');
            w.f64(2.5);
            w.u8(b'F');
            w.f32(-1.25);
            w.u8(b'I');
            w.i32(42);
            w.u8(b'J');
            w.i64(9_000_000_000);
            w.u8(b'S');
            w.i16(-300);
            w.u8(b'Z');
            w.u8(1);
            w.u8(b'V');
            w.u8(b's');
            w.id(obj_id, sizes.object_id);
            w.u8(b'L');
            w.id(0, sizes.object_id);

            let bytes = w.into_bytes();
            let mut r = Reader::new(&bytes);
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Byte(-5));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Char('A' as u16));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Double(2.5));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Float(-1.25));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Int(42));
            assert_eq!(
                r.tagged_value(&sizes).unwrap(),
                JdwpValue::Long(9_000_000_000)
            );
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Short(-300));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Boolean(true));
            assert_eq!(r.tagged_value(&sizes).unwrap(), JdwpValue::Void);
            assert_eq!(
                r.tagged_value(&sizes).unwrap(),
                JdwpValue::Object {
                    tag: b's',
                    id: obj_id
                }
            );
            assert_eq!(
                r.tagged_value(&sizes).unwrap(),
                JdwpValue::Object { tag: b'L', id: 0 }
            );
        }

        let bad = *b"?";
        assert!(matches!(
            Reader::new(&bad).tagged_value(&sizes_8()),
            Err(JdwpError::UnsupportedTag(b'?'))
        ));
    }

    #[test]
    fn array_region_primitive_and_object() {
        let sizes = sizes_4();

        // Primitive int[] { 10, -20, 30 }
        let mut wp = Writer::new();
        wp.u8(b'I');
        wp.u32(3);
        wp.i32(10);
        wp.i32(-20);
        wp.i32(30);
        let prim = Reader::new(&wp.into_bytes()).array_region(&sizes).unwrap();
        assert_eq!(
            prim,
            vec![JdwpValue::Int(10), JdwpValue::Int(-20), JdwpValue::Int(30)]
        );

        // Object String[] { "a"@0x100, null }
        let mut wo = Writer::new();
        wo.u8(b'L');
        wo.u32(2);
        wo.u8(b's');
        wo.id(0x100, sizes.object_id);
        wo.u8(b'L');
        wo.id(0, sizes.object_id);
        let objs = Reader::new(&wo.into_bytes()).array_region(&sizes).unwrap();
        assert_eq!(
            objs,
            vec![
                JdwpValue::Object {
                    tag: b's',
                    id: 0x100
                },
                JdwpValue::Object { tag: b'L', id: 0 },
            ]
        );
    }

    #[test]
    fn encode_command_header_is_big_endian() {
        let pkt = encode_command(7, 15, 1, &[0xAA, 0xBB]);
        assert_eq!(pkt.len(), HEADER_LEN + 2);
        assert_eq!(&pkt[0..4], &13u32.to_be_bytes());
        assert_eq!(&pkt[4..8], &7u32.to_be_bytes());
        assert_eq!(pkt[8], 0x00);
        assert_eq!(pkt[9], 15);
        assert_eq!(pkt[10], 1);
        assert_eq!(&pkt[11..], &[0xAA, 0xBB]);
    }
}
