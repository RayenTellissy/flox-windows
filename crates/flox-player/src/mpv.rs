//! A safe mpv handle with properties, commands and an event channel.
//!
//! Every property goes through `MPV_FORMAT_NODE`, converted to and from
//! `serde_json::Value`. Events are drained on a dedicated thread that the
//! wakeup callback nudges, and forwarded to a tokio mpsc channel.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use flox_core::error::{Error, Result};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::ffi::{self, MpvHandle, MpvLib, MpvNode};

/// mpv data formats for `observe`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    None,
    String,
    Flag,
    Int64,
    Double,
    Node,
}

impl Format {
    fn raw(self) -> c_int {
        match self {
            Format::None => ffi::MPV_FORMAT_NONE,
            Format::String => ffi::MPV_FORMAT_STRING,
            Format::Flag => ffi::MPV_FORMAT_FLAG,
            Format::Int64 => ffi::MPV_FORMAT_INT64,
            Format::Double => ffi::MPV_FORMAT_DOUBLE,
            Format::Node => ffi::MPV_FORMAT_NODE,
        }
    }
}

/// A value that can be read from or written to an mpv property. Values travel
/// as `MPV_FORMAT_NODE`, through their JSON form.
pub trait MpvValue: Sized + Send {
    /// The JSON form written as an mpv node.
    fn to_json(self) -> Value;
    /// Parses the JSON form of a node read from mpv; `None` on a type mismatch.
    fn from_json(v: Value) -> Option<Self>;
}

impl MpvValue for bool {
    fn to_json(self) -> Value {
        Value::Bool(self)
    }
    fn from_json(v: Value) -> Option<Self> {
        v.as_bool()
    }
}

impl MpvValue for i64 {
    fn to_json(self) -> Value {
        Value::from(self)
    }
    fn from_json(v: Value) -> Option<Self> {
        v.as_i64().or_else(|| {
            v.as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)
                .map(|f| f as i64)
        })
    }
}

impl MpvValue for f64 {
    fn to_json(self) -> Value {
        Value::from(self)
    }
    fn from_json(v: Value) -> Option<Self> {
        v.as_f64()
    }
}

impl MpvValue for String {
    fn to_json(self) -> Value {
        Value::String(self)
    }
    fn from_json(v: Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

impl MpvValue for Value {
    fn to_json(self) -> Value {
        self
    }
    fn from_json(v: Value) -> Option<Self> {
        Some(v)
    }
}

/// Events drained from `mpv_wait_event`.
#[derive(Clone, Debug, PartialEq)]
pub enum MpvEvent {
    StartFile,
    FileLoaded,
    PlaybackRestart,
    /// `reason` is `"eof"`, `"stop"`, `"quit"`, `"error"`, `"redirect"` or `"unknown"`;
    /// `error` is the mpv error code (non-zero only for `"error"`).
    EndFile {
        reason: String,
        error: i32,
    },
    PropertyChange {
        name: String,
        value: serde_json::Value,
    },
    Idle,
    Shutdown,
}

/// Capacity of the event channel.
const EVENT_CAPACITY: usize = 512;

/// The raw handle shared by [`Mpv`], stream callbacks and the renderer.
/// `mpv_terminate_destroy` runs when the last owner lets go.
pub(crate) struct Handle {
    pub(crate) lib: Arc<MpvLib>,
    pub(crate) ptr: *mut MpvHandle,
    /// Stream-cb opener boxes; mpv holds raw pointers to them until the core is gone,
    /// so they are dropped only after `mpv_terminate_destroy` (fields drop after `drop`).
    pub(crate) keep_alive: Mutex<Vec<Box<dyn std::any::Any + Send + Sync>>>,
}

// SAFETY: the mpv client API is thread-safe: every function except
// `mpv_wait_event` may be called from any thread on the same handle, and Flox
// calls `mpv_wait_event` from the event thread only.
unsafe impl Send for Handle {}
// SAFETY: see above.
unsafe impl Sync for Handle {}

impl Handle {
    pub(crate) fn check(&self, code: c_int) -> Result<()> {
        self.lib.check(code)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from mpv_create and is destroyed exactly once, here.
        // Every render context holds an Arc<Handle> and frees itself first.
        unsafe { (self.lib.terminate_destroy)(self.ptr) };
    }
}

/// Shared wake-up state between the wakeup callback and the event thread.
#[derive(Default)]
struct Wake {
    pending: Mutex<bool>,
    cv: Condvar,
    stop: AtomicBool,
}

impl Wake {
    fn notify(&self) {
        *self.pending.lock() = true;
        self.cv.notify_one();
    }
}

/// `mpv_set_wakeup_callback` target. Runs on arbitrary mpv threads and must not
/// call back into mpv.
unsafe extern "C" fn on_wakeup(d: *mut c_void) {
    let _ = catch_unwind(|| {
        // SAFETY: `d` is `Arc::as_ptr` of the `Wake` owned by `Mpv`; the callback is
        // cleared before that Arc can be dropped.
        let wake = unsafe { &*(d as *const Wake) };
        wake.notify();
    });
}

/// One mpv instance; `mpv_terminate_destroy` on drop.
pub struct Mpv {
    pub(crate) handle: Arc<Handle>,
    wake: Arc<Wake>,
    thread: Option<JoinHandle<()>>,
    events: Mutex<Option<mpsc::Receiver<MpvEvent>>>,
    next_observe_id: AtomicU64,
}

impl std::fmt::Debug for Mpv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mpv").finish_non_exhaustive()
    }
}

fn cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Other(format!("NUL byte in mpv argument {s:?}")))
}

impl Mpv {
    /// `mpv_create`, the options, `mpv_initialize`, and the event thread.
    pub fn new(lib: Arc<MpvLib>, opts: &[(&str, &str)]) -> Result<Mpv> {
        // SAFETY: mpv_create has no preconditions; NULL means out of memory.
        let ptr = unsafe { (lib.create)() };
        if ptr.is_null() {
            return Err(Error::Mpv {
                code: -2,
                message: "mpv_create failed".to_owned(),
            });
        }
        let handle = Arc::new(Handle {
            lib,
            ptr,
            keep_alive: Mutex::new(Vec::new()),
        });
        for (name, value) in opts {
            let n = cstring(name)?;
            let v = cstring(value)?;
            // SAFETY: valid handle before initialisation, NUL-terminated strings.
            let code = unsafe { (handle.lib.set_option_string)(ptr, n.as_ptr(), v.as_ptr()) };
            handle.check(code).map_err(|e| match e {
                Error::Mpv { code, message } => Error::Mpv {
                    code,
                    message: format!("option {name}={value}: {message}"),
                },
                other => other,
            })?;
        }
        // SAFETY: valid, not yet initialised handle.
        handle.check(unsafe { (handle.lib.initialize)(ptr) })?;

        let wake = Arc::new(Wake::default());
        let (tx, rx) = mpsc::channel(EVENT_CAPACITY);
        let thread = {
            let handle = handle.clone();
            let wake = wake.clone();
            std::thread::Builder::new()
                .name("mpv-events".to_owned())
                .spawn(move || event_loop(&handle, &wake, &tx))?
        };
        // SAFETY: `wake` outlives the registration: Drop clears the callback before
        // releasing its Arc.
        unsafe {
            (handle.lib.set_wakeup_callback)(
                ptr,
                Some(on_wakeup),
                Arc::as_ptr(&wake) as *mut c_void,
            )
        };
        // Drain anything queued before the callback was installed.
        wake.notify();

        Ok(Mpv {
            handle,
            wake,
            thread: Some(thread),
            events: Mutex::new(Some(rx)),
            next_observe_id: AtomicU64::new(1),
        })
    }

    /// The loaded library this instance uses.
    pub fn lib(&self) -> &Arc<MpvLib> {
        &self.handle.lib
    }

    /// `mpv_command`.
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned = args
            .iter()
            .map(|a| cstring(a))
            .collect::<Result<Vec<_>>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: NULL-terminated array of NUL-terminated strings that outlive the call.
        let code = unsafe { (self.handle.lib.command)(self.handle.ptr, ptrs.as_mut_ptr()) };
        self.handle.check(code)
    }

    /// `mpv_set_property` with `MPV_FORMAT_NODE`.
    pub fn set_property<T: MpvValue>(&self, name: &str, v: T) -> Result<()> {
        let n = cstring(name)?;
        let mut node = node::NodeBuf::from_json(&v.to_json())?;
        // SAFETY: `node` and everything it points to live until the call returns;
        // mpv copies the data.
        let code = unsafe {
            (self.handle.lib.set_property)(
                self.handle.ptr,
                n.as_ptr(),
                ffi::MPV_FORMAT_NODE,
                node.as_mut_ptr() as *mut c_void,
            )
        };
        self.handle.check(code)
    }

    /// `mpv_get_property` with `MPV_FORMAT_NODE`.
    pub fn get_property<T: MpvValue>(&self, name: &str) -> Result<T> {
        let n = cstring(name)?;
        let mut raw = MpvNode::none();
        // SAFETY: `raw` is a valid out-pointer for MPV_FORMAT_NODE.
        let code = unsafe {
            (self.handle.lib.get_property)(
                self.handle.ptr,
                n.as_ptr(),
                ffi::MPV_FORMAT_NODE,
                &mut raw as *mut MpvNode as *mut c_void,
            )
        };
        self.handle.check(code)?;
        // SAFETY: on success mpv filled `raw` with a node tree it allocated.
        let value = unsafe { node::to_json(&raw) };
        // SAFETY: frees exactly the tree mpv allocated for this call.
        unsafe { (self.handle.lib.free_node_contents)(&mut raw) };
        T::from_json(value.clone()).ok_or_else(|| Error::Mpv {
            code: -9,
            message: format!("property {name} has unexpected value {value}"),
        })
    }

    /// `mpv_observe_property`. Changes arrive as [`MpvEvent::PropertyChange`].
    pub fn observe(&self, name: &str, fmt: Format) -> Result<()> {
        let n = cstring(name)?;
        let id = self.next_observe_id.fetch_add(1, Ordering::Relaxed);
        // SAFETY: valid handle and NUL-terminated name.
        let code = unsafe {
            (self.handle.lib.observe_property)(self.handle.ptr, id, n.as_ptr(), fmt.raw())
        };
        self.handle.check(code)
    }

    /// The event receiver. The first call takes it; later calls get a closed receiver.
    pub fn events(&self) -> tokio::sync::mpsc::Receiver<MpvEvent> {
        self.events.lock().take().unwrap_or_else(|| {
            let (_tx, rx) = mpsc::channel(1);
            rx
        })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: clearing the callback with a valid handle; after this returns mpv
        // no longer references `self.wake`.
        unsafe {
            (self.handle.lib.set_wakeup_callback)(self.handle.ptr, None, std::ptr::null_mut())
        };
        self.wake.stop.store(true, Ordering::SeqCst);
        self.wake.notify();
        if let Some(t) = self.thread.take() {
            if t.join().is_err() {
                tracing::warn!("mpv event thread panicked");
            }
        }
        // `handle` drops next: mpv_terminate_destroy once no renderer holds it.
    }
}

fn event_loop(handle: &Handle, wake: &Wake, tx: &mpsc::Sender<MpvEvent>) {
    loop {
        {
            let mut pending = wake.pending.lock();
            while !*pending && !wake.stop.load(Ordering::SeqCst) {
                wake.cv.wait(&mut pending);
            }
            *pending = false;
        }
        if wake.stop.load(Ordering::SeqCst) {
            return;
        }
        loop {
            // SAFETY: only this thread calls mpv_wait_event; timeout 0 never blocks.
            let ev = unsafe { (handle.lib.wait_event)(handle.ptr, 0.0) };
            if ev.is_null() {
                break;
            }
            // SAFETY: mpv returns a valid event that stays valid until the next
            // mpv_wait_event call, and `convert` copies everything out of it.
            let (id, converted) = unsafe { convert(&*ev) };
            if id == ffi::MPV_EVENT_NONE {
                break;
            }
            if let Some(e) = converted {
                if !deliver(tx, e, wake) {
                    return;
                }
            }
            if id == ffi::MPV_EVENT_SHUTDOWN {
                return;
            }
        }
    }
}

/// Sends without blocking forever: retries while the channel is full, gives up
/// when the receiver is gone (`true`) or when the instance is dropping (`false`).
fn deliver(tx: &mpsc::Sender<MpvEvent>, mut e: MpvEvent, wake: &Wake) -> bool {
    loop {
        match tx.try_send(e) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => return true,
            Err(mpsc::error::TrySendError::Full(back)) => {
                if wake.stop.load(Ordering::SeqCst) {
                    return false;
                }
                e = back;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn end_reason(r: c_int) -> &'static str {
    match r {
        ffi::MPV_END_FILE_REASON_EOF => "eof",
        ffi::MPV_END_FILE_REASON_STOP => "stop",
        ffi::MPV_END_FILE_REASON_QUIT => "quit",
        ffi::MPV_END_FILE_REASON_ERROR => "error",
        ffi::MPV_END_FILE_REASON_REDIRECT => "redirect",
        _ => "unknown",
    }
}

/// Converts one raw event.
///
/// # Safety
/// `ev` must be an event returned by `mpv_wait_event` and not yet invalidated.
unsafe fn convert(ev: &ffi::MpvEventRaw) -> (c_int, Option<MpvEvent>) {
    let e = match ev.event_id {
        ffi::MPV_EVENT_SHUTDOWN => Some(MpvEvent::Shutdown),
        ffi::MPV_EVENT_START_FILE => Some(MpvEvent::StartFile),
        ffi::MPV_EVENT_FILE_LOADED => Some(MpvEvent::FileLoaded),
        ffi::MPV_EVENT_PLAYBACK_RESTART => Some(MpvEvent::PlaybackRestart),
        ffi::MPV_EVENT_IDLE => Some(MpvEvent::Idle),
        ffi::MPV_EVENT_END_FILE if !ev.data.is_null() => {
            // SAFETY: END_FILE events carry an mpv_event_end_file.
            let d = unsafe { &*(ev.data as *const ffi::MpvEventEndFile) };
            Some(MpvEvent::EndFile {
                reason: end_reason(d.reason).to_owned(),
                error: d.error,
            })
        }
        ffi::MPV_EVENT_PROPERTY_CHANGE if !ev.data.is_null() => {
            // SAFETY: PROPERTY_CHANGE events carry an mpv_event_property.
            let p = unsafe { &*(ev.data as *const ffi::MpvEventProperty) };
            if p.name.is_null() {
                None
            } else {
                // SAFETY: non-null NUL-terminated name owned by the event.
                let name = unsafe { CStr::from_ptr(p.name) }
                    .to_string_lossy()
                    .into_owned();
                // SAFETY: `data` matches `format` per the mpv contract.
                let value = unsafe { property_value(p.format, p.data) };
                Some(MpvEvent::PropertyChange { name, value })
            }
        }
        _ => None,
    };
    (ev.event_id, e)
}

/// Reads an `mpv_event_property` payload.
///
/// # Safety
/// `data` must point to a value of `format` (or be NULL).
unsafe fn property_value(format: c_int, data: *mut c_void) -> Value {
    if data.is_null() {
        return Value::Null;
    }
    // SAFETY (all arms): `data` points to the C type that `format` names.
    unsafe {
        match format {
            ffi::MPV_FORMAT_STRING | ffi::MPV_FORMAT_OSD_STRING => {
                let s = *(data as *const *const c_char);
                if s.is_null() {
                    Value::Null
                } else {
                    Value::String(CStr::from_ptr(s).to_string_lossy().into_owned())
                }
            }
            ffi::MPV_FORMAT_FLAG => Value::Bool(*(data as *const c_int) != 0),
            ffi::MPV_FORMAT_INT64 => Value::from(*(data as *const i64)),
            ffi::MPV_FORMAT_DOUBLE => node::double(*(data as *const f64)),
            ffi::MPV_FORMAT_NODE => node::to_json(&*(data as *const MpvNode)),
            _ => Value::Null,
        }
    }
}

/// `mpv_node` ↔ `serde_json::Value`.
pub(crate) mod node {
    use super::*;

    pub(crate) fn double(f: f64) -> Value {
        serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
    }

    /// Reads an mpv node tree into JSON. Byte arrays and unknown formats become `null`.
    ///
    /// # Safety
    /// `n` must be a valid node whose pointers follow the `mpv_node` rules.
    pub(crate) unsafe fn to_json(n: &MpvNode) -> Value {
        // SAFETY (all arms): the union member read is the one `format` selects,
        // and list pointers are valid for `num` entries.
        unsafe {
            match n.format {
                ffi::MPV_FORMAT_STRING | ffi::MPV_FORMAT_OSD_STRING => {
                    if n.u.string.is_null() {
                        Value::Null
                    } else {
                        Value::String(CStr::from_ptr(n.u.string).to_string_lossy().into_owned())
                    }
                }
                ffi::MPV_FORMAT_FLAG => Value::Bool(n.u.flag != 0),
                ffi::MPV_FORMAT_INT64 => Value::from(n.u.int64),
                ffi::MPV_FORMAT_DOUBLE => double(n.u.double_),
                ffi::MPV_FORMAT_NODE_ARRAY => {
                    let list = n.u.list;
                    if list.is_null() || (*list).num <= 0 || (*list).values.is_null() {
                        return Value::Array(Vec::new());
                    }
                    let len = (*list).num as usize;
                    let values = std::slice::from_raw_parts((*list).values, len);
                    Value::Array(values.iter().map(|v| to_json(v)).collect())
                }
                ffi::MPV_FORMAT_NODE_MAP => {
                    let list = n.u.list;
                    let mut map = serde_json::Map::new();
                    if list.is_null()
                        || (*list).num <= 0
                        || (*list).values.is_null()
                        || (*list).keys.is_null()
                    {
                        return Value::Object(map);
                    }
                    let len = (*list).num as usize;
                    let values = std::slice::from_raw_parts((*list).values, len);
                    let keys = std::slice::from_raw_parts((*list).keys, len);
                    for (k, v) in keys.iter().zip(values) {
                        if k.is_null() {
                            continue;
                        }
                        let key = CStr::from_ptr(*k).to_string_lossy().into_owned();
                        map.insert(key, to_json(v));
                    }
                    Value::Object(map)
                }
                _ => Value::Null,
            }
        }
    }

    /// Storage that keeps a node tree's strings and lists alive.
    enum Hold {
        None,
        Str(#[allow(dead_code)] CString),
        List {
            _list: Box<ffi::MpvNodeList>,
            _values: Vec<MpvNode>,
            _keys: Vec<CString>,
            _key_ptrs: Vec<*mut c_char>,
            _children: Vec<NodeBuf>,
        },
    }

    /// A Rust-owned `mpv_node` tree built from JSON, for passing into mpv.
    /// Heap buffers never move when the struct moves, so the raw pointers inside
    /// `node` stay valid for the life of the value.
    pub(crate) struct NodeBuf {
        node: MpvNode,
        _hold: Hold,
    }

    impl NodeBuf {
        pub(crate) fn from_json(v: &Value) -> Result<NodeBuf> {
            Ok(match v {
                Value::Null => NodeBuf {
                    node: MpvNode::none(),
                    _hold: Hold::None,
                },
                Value::Bool(b) => NodeBuf {
                    node: MpvNode {
                        u: ffi::MpvNodeU {
                            flag: c_int::from(*b),
                        },
                        format: ffi::MPV_FORMAT_FLAG,
                    },
                    _hold: Hold::None,
                },
                Value::Number(n) => NodeBuf {
                    node: match n.as_i64() {
                        Some(i) => MpvNode {
                            u: ffi::MpvNodeU { int64: i },
                            format: ffi::MPV_FORMAT_INT64,
                        },
                        None => MpvNode {
                            u: ffi::MpvNodeU {
                                double_: n.as_f64().unwrap_or(0.0),
                            },
                            format: ffi::MPV_FORMAT_DOUBLE,
                        },
                    },
                    _hold: Hold::None,
                },
                Value::String(s) => {
                    let c = cstring(s)?;
                    NodeBuf {
                        node: MpvNode {
                            u: ffi::MpvNodeU {
                                string: c.as_ptr() as *mut c_char,
                            },
                            format: ffi::MPV_FORMAT_STRING,
                        },
                        _hold: Hold::Str(c),
                    }
                }
                Value::Array(items) => {
                    let children = items
                        .iter()
                        .map(NodeBuf::from_json)
                        .collect::<Result<Vec<_>>>()?;
                    list(children, Vec::new(), ffi::MPV_FORMAT_NODE_ARRAY)?
                }
                Value::Object(map) => {
                    let keys = map.keys().map(|k| cstring(k)).collect::<Result<Vec<_>>>()?;
                    let children = map
                        .values()
                        .map(NodeBuf::from_json)
                        .collect::<Result<Vec<_>>>()?;
                    list(children, keys, ffi::MPV_FORMAT_NODE_MAP)?
                }
            })
        }

        pub(crate) fn as_mut_ptr(&mut self) -> *mut MpvNode {
            &mut self.node
        }

        #[cfg(test)]
        pub(crate) fn node(&self) -> &MpvNode {
            &self.node
        }
    }

    fn list(children: Vec<NodeBuf>, keys: Vec<CString>, format: c_int) -> Result<NodeBuf> {
        let num = c_int::try_from(children.len())
            .map_err(|_| Error::Other("mpv node list too long".to_owned()))?;
        let mut values: Vec<MpvNode> = children.iter().map(|c| c.node).collect();
        let mut key_ptrs: Vec<*mut c_char> =
            keys.iter().map(|k| k.as_ptr() as *mut c_char).collect();
        let mut boxed = Box::new(ffi::MpvNodeList {
            num,
            values: if values.is_empty() {
                std::ptr::null_mut()
            } else {
                values.as_mut_ptr()
            },
            keys: if format == ffi::MPV_FORMAT_NODE_MAP && !key_ptrs.is_empty() {
                key_ptrs.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
        });
        let list_ptr: *mut ffi::MpvNodeList = &mut *boxed;
        Ok(NodeBuf {
            node: MpvNode {
                u: ffi::MpvNodeU { list: list_ptr },
                format,
            },
            _hold: Hold::List {
                _list: boxed,
                _values: values,
                _keys: keys,
                _key_ptrs: key_ptrs,
                _children: children,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::node::{to_json, NodeBuf};
    use super::*;
    use serde_json::json;

    fn round_trip(v: Value) -> Value {
        let buf = NodeBuf::from_json(&v).unwrap();
        // SAFETY: `buf` owns a well-formed tree for the duration of the call.
        unsafe { to_json(buf.node()) }
    }

    #[test]
    fn scalars_round_trip() {
        for v in [
            json!(null),
            json!(true),
            json!(false),
            json!(42),
            json!(-7),
            json!(1.5),
            json!("hello · world"),
        ] {
            assert_eq!(round_trip(v.clone()), v);
        }
    }

    #[test]
    fn nested_trees_round_trip() {
        let v = json!([
            {"id": 1, "type": "audio", "lang": "eng", "codec": "eac3", "demux-channel-count": 6, "default": true},
            {"id": 2, "type": "sub", "external": false, "title": null, "ratio": 0.5},
            [],
            {},
            [[1, 2], {"a": {"b": "c"}}]
        ]);
        assert_eq!(round_trip(v.clone()), v);
    }

    #[test]
    fn interior_nul_is_rejected() {
        assert!(NodeBuf::from_json(&json!("a\0b")).is_err());
        assert!(NodeBuf::from_json(&json!({"a\0": 1})).is_err());
    }

    #[test]
    fn large_unsigned_becomes_double() {
        let v = json!(u64::MAX);
        let buf = NodeBuf::from_json(&v).unwrap();
        assert_eq!(buf.node().format, ffi::MPV_FORMAT_DOUBLE);
    }

    #[test]
    fn value_conversions() {
        assert_eq!(i64::from_json(json!(3.0)), Some(3));
        assert_eq!(i64::from_json(json!(3.5)), None);
        assert_eq!(f64::from_json(json!(3)), Some(3.0));
        assert_eq!(bool::from_json(json!(1)), None);
        assert_eq!(String::from_json(json!("x")), Some("x".to_owned()));
        assert_eq!(String::from_json(json!(1)), None);
        assert_eq!(1.25f64.to_json(), json!(1.25));
    }

    #[test]
    fn property_payloads() {
        let mut d = 2.5f64;
        let mut flag: c_int = 1;
        let mut i = 9i64;
        let s = CString::new("abc").unwrap();
        let mut sp = s.as_ptr();
        // SAFETY: each pointer refers to a live value of the stated format.
        unsafe {
            assert_eq!(
                property_value(ffi::MPV_FORMAT_DOUBLE, (&mut d as *mut f64).cast()),
                json!(2.5)
            );
            assert_eq!(
                property_value(ffi::MPV_FORMAT_FLAG, (&mut flag as *mut c_int).cast()),
                json!(true)
            );
            assert_eq!(
                property_value(ffi::MPV_FORMAT_INT64, (&mut i as *mut i64).cast()),
                json!(9)
            );
            assert_eq!(
                property_value(
                    ffi::MPV_FORMAT_STRING,
                    (&mut sp as *mut *const c_char).cast()
                ),
                json!("abc")
            );
            assert_eq!(
                property_value(ffi::MPV_FORMAT_NONE, std::ptr::null_mut()),
                Value::Null
            );
        }
        assert_eq!(node::double(f64::NAN), Value::Null);
    }

    #[test]
    fn end_file_reasons() {
        assert_eq!(end_reason(0), "eof");
        assert_eq!(end_reason(2), "stop");
        assert_eq!(end_reason(4), "error");
        assert_eq!(end_reason(99), "unknown");
    }
}
