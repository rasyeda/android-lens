use crate::protocol::{IdSizes, JdwpError, Location, Reader};

pub const EVENT_SINGLE_STEP: u8 = 1;
pub const EVENT_BREAKPOINT: u8 = 2;
pub const EVENT_EXCEPTION: u8 = 4;
pub const EVENT_CLASS_PREPARE: u8 = 8;
pub const EVENT_VM_START: u8 = 90;
pub const EVENT_VM_DEATH: u8 = 99;

pub const SUSPEND_NONE: u8 = 0;
pub const SUSPEND_EVENT_THREAD: u8 = 1;
pub const SUSPEND_ALL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JdwpEvent {
    SingleStep {
        request_id: i32,
        thread_id: u64,
        location: Location,
    },
    Breakpoint {
        request_id: i32,
        thread_id: u64,
        location: Location,
    },
    ClassPrepare {
        request_id: i32,
        thread_id: u64,
        ref_type_tag: u8,
        type_id: u64,
        signature: String,
        status: i32,
    },
    Exception {
        request_id: i32,
        thread_id: u64,
        location: Location,
        exception_id: u64,
        catch_location: Location,
    },
    VmStart {
        request_id: i32,
        thread_id: u64,
    },
    VmDeath {
        request_id: i32,
    },
    Other {
        event_kind: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeEvent {
    pub suspend_policy: u8,
    pub events: Vec<JdwpEvent>,
}

pub fn parse_composite_event(payload: &[u8], sizes: &IdSizes) -> Result<CompositeEvent, JdwpError> {
    let mut r = Reader::new(payload);
    let suspend_policy = r.u8()?;
    let count = r.u32()?;
    let mut events = Vec::with_capacity(count as usize);

    for _ in 0..count {
        let kind = r.u8()?;
        let ev = match kind {
            EVENT_SINGLE_STEP => {
                let request_id = r.i32()?;
                let thread_id = r.id(sizes.object_id)?;
                let location = r.location(sizes)?;
                JdwpEvent::SingleStep {
                    request_id,
                    thread_id,
                    location,
                }
            }
            EVENT_BREAKPOINT => {
                let request_id = r.i32()?;
                let thread_id = r.id(sizes.object_id)?;
                let location = r.location(sizes)?;
                JdwpEvent::Breakpoint {
                    request_id,
                    thread_id,
                    location,
                }
            }
            EVENT_CLASS_PREPARE => {
                let request_id = r.i32()?;
                let thread_id = r.id(sizes.object_id)?;
                let ref_type_tag = r.u8()?;
                let type_id = r.id(sizes.reference_type_id)?;
                let signature = r.string()?;
                let status = r.i32()?;
                JdwpEvent::ClassPrepare {
                    request_id,
                    thread_id,
                    ref_type_tag,
                    type_id,
                    signature,
                    status,
                }
            }
            EVENT_EXCEPTION => {
                let request_id = r.i32()?;
                let thread_id = r.id(sizes.object_id)?;
                let location = r.location(sizes)?;
                let _tag = r.u8()?;
                let exception_id = r.id(sizes.object_id)?;
                let catch_location = r.location(sizes)?;
                JdwpEvent::Exception {
                    request_id,
                    thread_id,
                    location,
                    exception_id,
                    catch_location,
                }
            }
            EVENT_VM_START => {
                let request_id = r.i32()?;
                let thread_id = r.id(sizes.object_id)?;
                JdwpEvent::VmStart {
                    request_id,
                    thread_id,
                }
            }
            EVENT_VM_DEATH => {
                let request_id = r.i32()?;
                JdwpEvent::VmDeath { request_id }
            }
            other => {
                // Unknown event kind; cannot safely parse remaining events in this composite packet.
                events.push(JdwpEvent::Other { event_kind: other });
                break;
            }
        };
        events.push(ev);
    }

    Ok(CompositeEvent {
        suspend_policy,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Writer;

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
    fn parse_composite_multi_event_4_and_8_byte() {
        for sizes in [sizes_4(), sizes_8()] {
            let tid = if sizes.object_id == 4 {
                0x0000_1234
            } else {
                0x1122_3344_5566_7788
            };
            let cls = if sizes.reference_type_id == 4 {
                0x0000_5678
            } else {
                0xAABB_CCDD_EEFF_0011
            };
            let meth = if sizes.method_id == 4 {
                0x0000_9ABC
            } else {
                0x2233_4455_6677_8899
            };
            let loc = Location {
                type_tag: 1,
                class_id: cls,
                method_id: meth,
                index: 12,
            };
            let catch_loc = Location {
                type_tag: 1,
                class_id: cls,
                method_id: meth,
                index: 44,
            };

            let mut w = Writer::new();
            w.u8(SUSPEND_ALL);
            w.u32(6);

            // 1. Breakpoint
            w.u8(EVENT_BREAKPOINT);
            w.i32(10);
            w.id(tid, sizes.object_id);
            w.location(&loc, &sizes);

            // 2. SingleStep
            w.u8(EVENT_SINGLE_STEP);
            w.i32(11);
            w.id(tid, sizes.object_id);
            w.location(&loc, &sizes);

            // 3. ClassPrepare
            w.u8(EVENT_CLASS_PREPARE);
            w.i32(12);
            w.id(tid, sizes.object_id);
            w.u8(1);
            w.id(cls, sizes.reference_type_id);
            w.string("Lcom/example/MainActivity;");
            w.i32(7);

            // 4. Exception
            w.u8(EVENT_EXCEPTION);
            w.i32(13);
            w.id(tid, sizes.object_id);
            w.location(&loc, &sizes);
            w.u8(b'L');
            w.id(tid, sizes.object_id);
            w.location(&catch_loc, &sizes);

            // 5. VmStart
            w.u8(EVENT_VM_START);
            w.i32(0);
            w.id(tid, sizes.object_id);

            // 6. VmDeath
            w.u8(EVENT_VM_DEATH);
            w.i32(0);

            let comp = parse_composite_event(&w.into_bytes(), &sizes).unwrap();
            assert_eq!(comp.suspend_policy, SUSPEND_ALL);
            assert_eq!(
                comp.events,
                vec![
                    JdwpEvent::Breakpoint {
                        request_id: 10,
                        thread_id: tid,
                        location: loc,
                    },
                    JdwpEvent::SingleStep {
                        request_id: 11,
                        thread_id: tid,
                        location: loc,
                    },
                    JdwpEvent::ClassPrepare {
                        request_id: 12,
                        thread_id: tid,
                        ref_type_tag: 1,
                        type_id: cls,
                        signature: "Lcom/example/MainActivity;".to_string(),
                        status: 7,
                    },
                    JdwpEvent::Exception {
                        request_id: 13,
                        thread_id: tid,
                        location: loc,
                        exception_id: tid,
                        catch_location: catch_loc,
                    },
                    JdwpEvent::VmStart {
                        request_id: 0,
                        thread_id: tid,
                    },
                    JdwpEvent::VmDeath { request_id: 0 },
                ]
            );
        }
    }

    #[test]
    fn parse_composite_unknown_event_stops_safely() {
        let mut w = Writer::new();
        w.u8(SUSPEND_NONE);
        w.u32(2);
        w.u8(EVENT_VM_DEATH);
        w.i32(1);
        w.u8(77); // unknown event kind
        w.u32(0x1234_5678);

        let comp = parse_composite_event(&w.into_bytes(), &sizes_8()).unwrap();
        assert_eq!(comp.suspend_policy, SUSPEND_NONE);
        assert_eq!(
            comp.events,
            vec![
                JdwpEvent::VmDeath { request_id: 1 },
                JdwpEvent::Other { event_kind: 77 },
            ]
        );
    }

    #[test]
    fn parse_composite_truncated_returns_eof() {
        let mut w = Writer::new();
        w.u8(SUSPEND_ALL);
        w.u32(1);
        w.u8(EVENT_BREAKPOINT);
        w.i32(1); // missing thread_id + location

        assert!(matches!(
            parse_composite_event(&w.into_bytes(), &sizes_8()),
            Err(JdwpError::UnexpectedEof)
        ));
    }
}
