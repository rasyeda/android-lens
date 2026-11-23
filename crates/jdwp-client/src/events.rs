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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
