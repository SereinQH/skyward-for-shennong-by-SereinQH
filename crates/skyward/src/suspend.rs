//! The wake-lock service the weaver's transport takes before it touches a
//! secure element.
//!
//! The weaver does not read the element cold: its transport asks the phone
//! to keep the processor awake while it does, by looking up
//! `android.system.suspend.ISystemSuspend/default` and taking a wake lock
//! from it. The running system keeps the answering end in `system_suspend`
//! — the service the framework takes its own wake locks from — and a
//! recovery runs none of the running system's services. A weaver that
//! cannot reach it waits for the service for good and never sends the first
//! call, which is half of what opens the data partition.
//!
//! So Skyward answers it here, as it answers OMAPI. Nothing in a recovery
//! goes to its rest, so a lock taken is a lock held and letting it go
//! changes nothing: both calls answer, and the weaver goes on to the
//! element.

use anyhow::{bail, Context, Result};
use skyward_binder::driver::{Binder, Object};
use skyward_binder::parcel::{Parcel, Reader};
use skyward_binder::service;
use std::rc::Rc;

/// What the phone looks this service up as.
pub const SERVICE: &str = "android.system.suspend.ISystemSuspend/default";

const SUSPEND: &str = "android.system.suspend.ISystemSuspend";
const LOCK: &str = "android.system.suspend.IWakeLock";

/// The version and the mark the phone's own copies of these interfaces
/// serve. The version is the one the phone's libraries are built for, named
/// in the `android.system.suspend-V1-ndk.so` they carry.
const VERSION: i32 = 1;
const HASH: &str = "082a58c2505b703702f34ab8abcb1d1b5e890be4";

/// What libbinder answers for every object it serves, before any interface
/// sees the call.
const INTERFACE: u32 = u32::from_be_bytes(*b"_NTF");
const DEBUG_PID: u32 = u32::from_be_bytes(*b"_PID");
const INTERFACE_VERSION: u32 = 0x00ff_ffff;
const INTERFACE_HASH: u32 = 0x00ff_fffe;

/// `android.system.suspend.ISystemSuspend`: the one call its own file
/// declares.
const ACQUIRE_WAKE_LOCK: u32 = 1;

/// `android.system.suspend.IWakeLock`: the one call its own file declares.
const RELEASE: u32 = 1;

/// The numbers this program serves its objects under.
const SERVICE_OBJECT: u64 = 1;
const LOCK_OBJECT: u64 = 2;

/// A wake lock taken from the service. Nothing goes to its rest here, so
/// there is nothing to hold and nothing to give back.
struct Lock;

impl Object for Lock {
    fn on_transact(&self, code: u32, _call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        if common(code, LOCK, reply) {
            return Ok(());
        }
        match code {
            RELEASE => *reply = answering(),
            _ => bail!("the wake lock has no method {code}"),
        }
        Ok(())
    }
}

/// The service itself: what the weaver takes its wake lock from.
struct Suspend;

impl Object for Suspend {
    fn on_transact(&self, code: u32, call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("suspend: the service is asked {code}"));
        if common(code, SUSPEND, reply) {
            return Ok(());
        }
        match code {
            ACQUIRE_WAKE_LOCK => {
                let kind = call.i32().context("read which kind of wake lock is asked for")?;
                let name = call.string().context("read what the wake lock is called")?;
                let _ = skyward_sys::kmsg(&format!("suspend: a wake lock of kind {kind} for {name} is asked for"));
                *reply = answering();
                reply.object(LOCK_OBJECT);
            }
            _ => bail!("the suspend service has no method {code}"),
        }
        Ok(())
    }
}

/// Every interface here opens its answer the same way: the mark saying the
/// call went through, and then what the method itself answered.
fn answering() -> Parcel {
    let mut reply = Parcel::default();
    reply.i32(0);
    reply
}

/// Answer the questions libbinder puts to every object before any interface
/// sees the call. Says whether the call was one of them.
fn common(code: u32, interface: &str, reply: &mut Parcel) -> bool {
    match code {
        INTERFACE => reply.string(interface),
        INTERFACE_VERSION => reply.i32(VERSION),
        INTERFACE_HASH => reply.string(HASH),
        DEBUG_PID => reply.i32(std::process::id() as i32),
        _ => return false,
    }
    true
}

/// Serve the wake-lock service, and go on serving it. Returns only when the
/// driver or the program ends.
pub fn serve() -> Result<()> {
    let binder = Rc::new(Binder::open().context("open the driver the service is served over")?);
    binder.serving(vec![
        (SERVICE_OBJECT, Rc::new(Suspend) as Rc<dyn Object>),
        (LOCK_OBJECT, Rc::new(Lock)),
    ]);
    service::add(&binder, SERVICE, SERVICE_OBJECT).with_context(|| format!("offer {SERVICE} to the phone"))?;
    // A line that cannot be written is no reason to stop serving: the
    // weaver is already waiting on what is answered below.
    let _ = skyward_sys::kmsg(&format!("suspend: serving {SERVICE}"));
    binder.serve()
}
