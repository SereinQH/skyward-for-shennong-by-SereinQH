//! The layer the phone's weaver reaches a secure element through.
//!
//! The weaver does not touch the chip itself: it asks Android for the
//! elements the phone has, for one of them, for a session on that, and for
//! a channel on the session, and then sends the element's own protocol
//! through the channel. Android keeps the answering end of that in a
//! service of its own inside the running system, which a recovery does not
//! run — and a weaver that cannot find it says `Failed to connect to OMAPI`
//! and reads no slot, which is half of what opens the data partition.
//!
//! So Skyward answers it here. Each of those — the service, the reader, the
//! session, the channel — is an object of its own, served under a number of
//! its own, and every call goes straight through to the element's own
//! hardware service, which is the only thing in this that touches the chip.

use anyhow::{bail, Context, Result};
use skyward_binder::driver::{Binder, Object, Reply};
use skyward_binder::parcel::{Parcel, Reader};
use skyward_binder::{service, status};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What the phone looks this layer up as.
pub const SERVICE: &str = "android.se.omapi.ISecureElementService/default";

const OMAPI: &str = "android.se.omapi.ISecureElementService";
const OMAPI_READER: &str = "android.se.omapi.ISecureElementReader";
const OMAPI_SESSION: &str = "android.se.omapi.ISecureElementSession";
const OMAPI_CHANNEL: &str = "android.se.omapi.ISecureElementChannel";

/// The element's own hardware service, registered once for each element the
/// phone has, under that element's name.
const ELEMENT: &str = "android.hardware.secure_element.ISecureElement";
const ELEMENT_CALLBACK: &str = "android.hardware.secure_element.ISecureElementCallback";

/// The version and the mark the phone's own copies of these interfaces
/// serve. A program that takes an object as one of its own asks for both
/// first, so what is answered here has to be what those copies answer.
const VERSION: i32 = 1;
const HASH: &str = "894069bcfe4f35ceb2088278ddf87c83adee8014";

/// What libbinder answers for every object it serves, before any interface
/// sees the call.
const INTERFACE: u32 = u32::from_be_bytes(*b"_NTF");
const DEBUG_PID: u32 = u32::from_be_bytes(*b"_PID");
const INTERFACE_VERSION: u32 = 0x00ff_ffff;
const INTERFACE_HASH: u32 = 0x00ff_fffe;

/// `android.se.omapi.ISecureElementService`, in the order its own file
/// declares its methods.
const GET_READERS: u32 = 1;
const GET_READER: u32 = 2;
const IS_NFC_EVENT_ALLOWED: u32 = 3;

/// `android.se.omapi.ISecureElementReader`.
const READER_PRESENT: u32 = 1;
const OPEN_SESSION: u32 = 2;
const CLOSE_SESSIONS: u32 = 3;
const READER_RESET: u32 = 4;

/// `android.se.omapi.ISecureElementSession`.
const SESSION_ATR: u32 = 1;
const SESSION_CLOSE: u32 = 2;
const CLOSE_CHANNELS: u32 = 3;
const SESSION_CLOSED: u32 = 4;
const OPEN_BASIC_CHANNEL: u32 = 5;
const OPEN_LOGICAL_CHANNEL: u32 = 6;

/// `android.se.omapi.ISecureElementChannel`.
const CHANNEL_CLOSE: u32 = 1;
const CHANNEL_CLOSED: u32 = 2;
const CHANNEL_BASIC: u32 = 3;
const SELECT_RESPONSE: u32 = 4;
const TRANSMIT: u32 = 5;
const SELECT_NEXT: u32 = 6;

/// `android.hardware.secure_element.ISecureElement`.
const ELEMENT_CLOSE_CHANNEL: u32 = 1;
const ELEMENT_ATR: u32 = 2;
const ELEMENT_INIT: u32 = 3;
const ELEMENT_PRESENT: u32 = 4;
const ELEMENT_OPEN_BASIC: u32 = 5;
const ELEMENT_OPEN_LOGICAL: u32 = 6;
const ELEMENT_RESET: u32 = 7;
const ELEMENT_TRANSMIT: u32 = 8;

/// `android.hardware.secure_element.ISecureElementCallback`.
const ELEMENT_STATE_CHANGED: u32 = 1;

/// The numbers this program serves its objects under. One of each is all a
/// weaver asks for: it keeps the reader it is given, opens a session on it,
/// and opens a channel through that.
const SERVICE_OBJECT: u64 = 1;
const READER_OBJECT: u64 = 2;
const CALLBACK_OBJECT: u64 = 3;
const SESSION_OBJECT: u64 = 4;

/// The number the first channel this program hands out is served under.
/// A channel is an object of its own — one may be open while another is,
/// and each answers about the applet it was opened on — so every channel
/// opened is given a number of its own, counted on from here.
const FIRST_CHANNEL_OBJECT: u64 = 5;

/// The element's first channel, the one the applet selected as the element
/// was opened sits on. Every element has it, so it has no number of its
/// own to be told.
const BASIC_CHANNEL: i32 = 0;

/// The element's own hardware service, as this program calls it. The driver
/// is shared with the end that answers calls on this program's own objects,
/// because the objects handed to the element belong to whichever connection
/// handed them over, and the element's answer about them comes back on that
/// one alone.
struct Element {
    binder: Rc<Binder>,
    /// The number the driver knows the element's service by, taken up for
    /// as long as this program lives.
    device: u32,
}

impl Element {
    /// Reach the element the phone registers under `reader`, and tell it
    /// where its own notices are to go.
    fn open(binder: Rc<Binder>, reader: &str) -> Result<Element> {
        let name = format!("{ELEMENT}/{reader}");
        let device = service::find(&binder, &name)?
            .with_context(|| format!("{name} is not running, so the element cannot be read"))?
            .keep();
        let element = Element { binder, device };
        element.begin()?;
        Ok(element)
    }

    fn ask(&self, code: u32, call: &Parcel, what: &str) -> Result<Reply<'_>> {
        self.binder.transact(self.device, code, call).with_context(|| what.to_string())
    }

    /// Say where the element's own notices are to go, which it asks for
    /// before it will do anything else.
    fn begin(&self) -> Result<()> {
        let mut call = Parcel::call(ELEMENT);
        call.object(CALLBACK_OBJECT);
        // The share the driver takes of the object is given back on the
        // answer, which is all the element needs to keep calling it.
        let reply = self.ask(ELEMENT_INIT, &call, "open the element")?;
        status::ok(&mut reply.read()).context("open the element")
    }

    fn present(&self) -> Result<bool> {
        let reply = self.ask(ELEMENT_PRESENT, &Parcel::call(ELEMENT), "ask whether the element is there")?;
        let mut read = reply.read();
        status::ok(&mut read).context("ask whether the element is there")?;
        read.bool()
    }

    fn atr(&self) -> Result<Vec<u8>> {
        let reply = self.ask(ELEMENT_ATR, &Parcel::call(ELEMENT), "ask the element what it answers with")?;
        let mut read = reply.read();
        status::ok(&mut read).context("ask the element what it answers with")?;
        read.blob()
    }

    fn reset(&self) -> Result<bool> {
        let reply = self.ask(ELEMENT_RESET, &Parcel::call(ELEMENT), "reset the element")?;
        let mut read = reply.read();
        status::ok(&mut read).context("reset the element")?;
        Ok(true)
    }

    fn transmit(&self, command: &[u8]) -> Result<Vec<u8>> {
        let mut call = Parcel::call(ELEMENT);
        call.blob(command);
        let reply = self.ask(ELEMENT_TRANSMIT, &call, "send the element's own protocol to it")?;
        let _ = skyward_sys::kmsg(&format!("omapi: transmit {:02x?} answered {:02x?}", command, reply.bytes()));
        let mut read = reply.read();
        status::ok(&mut read).context("send the element's own protocol to it")?;
        read.blob()
    }

    fn close_channel(&self, channel: i32) -> Result<()> {
        let mut call = Parcel::call(ELEMENT);
        call.i32(channel);
        let reply = self.ask(ELEMENT_CLOSE_CHANNEL, &call, "close the channel")?;
        status::ok(&mut reply.read()).context("close the channel")
    }

    /// Open a channel to the applet named by `aid`, which the element
    /// selects as it opens it, and say which channel it is and what the
    /// applet answered the selection with.
    fn open_logical_channel(&self, aid: &[u8], p2: u8) -> Result<(i32, Vec<u8>)> {
        let mut call = Parcel::call(ELEMENT);
        call.blob(aid);
        call.i32(p2 as i32);
        let reply = self.ask(ELEMENT_OPEN_LOGICAL, &call, "open a channel to the applet")?;
        let mut read = reply.read();
        status::ok(&mut read).context("open a channel to the applet")?;
        let end = read.structure().context("read what the element answered")?;
        let channel = read.i32().context("read which channel it is")?;
        let selected = read.blob().context("read what the applet answered the selection with")?;
        read.go(end)?;
        Ok((channel, selected))
    }

    /// Open the basic channel, the element's first, which it keeps for the
    /// one applet it selects as it opens. Nothing says which channel it is
    /// — it is always the first — so all that comes back is the applet's
    /// own answer to that selection.
    fn open_basic_channel(&self, aid: &[u8], p2: u8) -> Result<Vec<u8>> {
        let mut call = Parcel::call(ELEMENT);
        call.blob(aid);
        call.i32(p2 as i32);
        let reply = self.ask(ELEMENT_OPEN_BASIC, &call, "open the basic channel")?;
        let mut read = reply.read();
        status::ok(&mut read).context("open the basic channel")?;
        read.blob().context("read what the applet answered the selection with")
    }
}

/// One channel this program has handed out: what the element calls it, what
/// the applet answered when the selection opened it, and whether it is the
/// element's first. Each channel keeps a record of its own, so what is sent
/// on one is never sent on another.
struct Opened {
    /// The number the element gave the channel, until it is closed.
    channel: Option<i32>,
    /// Whether this is the element's first channel, the one an applet was
    /// selected on as the element was opened.
    basic: bool,
    /// What the applet answered when the channel's applet was selected.
    selected: Vec<u8>,
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

/// The layer itself: what the phone asks for the elements it has.
struct Layer {
    /// The element this recovery runs the layer for, by the name the
    /// element's own service registers it under.
    reader: String,
}

impl Object for Layer {
    fn on_transact(&self, code: u32, call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("omapi: the layer is asked {code}"));
        if common(code, OMAPI, reply) {
            return Ok(());
        }
        match code {
            GET_READERS => {
                *reply = answering();
                reply.i32(1);
                reply.string(&self.reader);
            }
            GET_READER => {
                let asked = call.string().context("read which element is asked for")?;
                *reply = answering();
                reply.object(READER_OBJECT);
                if asked != self.reader {
                    // The element asked for is not one this phone has, and
                    // a reader that answers for nothing is worse than none.
                    let _ = skyward_sys::kmsg(&format!("omapi: asked for {asked}, which this phone does not have"));
                }
            }
            // Nothing here stands between an element and a program allowed
            // to reach it, so no event is turned away.
            IS_NFC_EVENT_ALLOWED => {
                *reply = answering();
                reply.i32(0);
            }
            _ => bail!("the layer has no method {code}"),
        }
        Ok(())
    }
}

/// One element, as the phone's programs reach it.
struct ElementReader {
    element: Rc<Element>,
    /// The session this reader hands out. A reader keeps one, and the phone
    /// asks for it again after closing the last, so this is what it is
    /// given each time it asks.
    session: Rc<Session>,
}

impl Object for ElementReader {
    fn on_transact(&self, code: u32, _call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("omapi: the reader is asked {code}"));
        if common(code, OMAPI_READER, reply) {
            return Ok(());
        }
        match code {
            READER_PRESENT => {
                let present = self.element.present()?;
                *reply = answering();
                reply.bool(present);
            }
            OPEN_SESSION => {
                // The element will not answer about itself until a session
                // is open, so the session is what opens it.
                self.element.atr()?;
                self.session.reopen()?;
                *reply = answering();
                reply.object(SESSION_OBJECT);
            }
            CLOSE_SESSIONS => {
                self.session.shut()?;
                *reply = answering();
            }
            READER_RESET => {
                let done = self.element.reset()?;
                *reply = answering();
                reply.bool(done);
            }
            _ => bail!("the reader has no method {code}"),
        }
        Ok(())
    }
}

/// A session on an element, through which channels are opened. The phone is
/// answered one session object and asks for it afresh after closing the
/// last, so this is that one object, emptied each time it is asked for.
struct Session {
    element: Rc<Element>,
    /// The channels the session has open, each one the record the channel
    /// object itself answers from rather than a copy of it.
    opened: RefCell<Vec<Rc<RefCell<Opened>>>>,
    /// Whether the session itself has been closed.
    closed: Cell<bool>,
    /// The number the next channel handed out is served under. Numbers are
    /// counted on and never reused, so a channel the phone still holds is
    /// never answered for by a later one.
    next: Cell<u64>,
}

impl Session {
    /// Close every channel the session has open. A channel is reached
    /// through the session that opened it, so none of them outlives it.
    fn close_channels(&self) -> Result<()> {
        for opened in std::mem::take(&mut *self.opened.borrow_mut()) {
            if let Some(number) = opened.borrow_mut().channel.take() {
                self.element.close_channel(number)?;
            }
        }
        Ok(())
    }

    /// Close the session itself, which takes its channels with it.
    fn shut(&self) -> Result<()> {
        self.closed.set(true);
        self.close_channels()
    }

    /// Take the session up for the next caller that asks for one, closing
    /// whatever the last one left open.
    fn reopen(&self) -> Result<()> {
        self.close_channels()?;
        self.closed.set(false);
        Ok(())
    }

    /// Hand the phone a channel object of its own for a channel the element
    /// has just opened, and keep the channel so that closing the session
    /// closes it. The object stays for as long as this program lives: the
    /// driver is told of it once and has no way to be told to forget it.
    fn hand_out(&self, opened: Opened, reply: &mut Parcel) {
        let opened = Rc::new(RefCell::new(opened));
        self.opened.borrow_mut().push(Rc::clone(&opened));
        let at = self.next.get();
        self.next.set(at + 1);
        self.element.binder.serving(vec![(at, Rc::new(Channel { element: Rc::clone(&self.element), opened }) as Rc<dyn Object>)]);
        *reply = answering();
        reply.object(at);
    }
}

impl Object for Session {
    fn on_transact(&self, code: u32, call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("omapi: the session is asked {code}"));
        if common(code, OMAPI_SESSION, reply) {
            return Ok(());
        }
        match code {
            SESSION_ATR => {
                let atr = self.element.atr()?;
                *reply = answering();
                reply.blob(&atr);
            }
            SESSION_CLOSE => {
                self.shut()?;
                *reply = answering();
            }
            // Closing the session's channels leaves the session itself
            // open, which is the difference between this and closing it.
            CLOSE_CHANNELS => {
                self.close_channels()?;
                *reply = answering();
            }
            SESSION_CLOSED => {
                *reply = answering();
                reply.bool(self.closed.get());
            }
            OPEN_BASIC_CHANNEL => {
                let aid = call.blob().context("read which applet is asked for")?;
                let p2 = call.i32().context("read how the applet is to be selected")? as u8;
                let selected = self.element.open_basic_channel(&aid, p2)?;
                let _ = skyward_sys::kmsg(&format!("omapi: the basic channel is open, the applet answered {} bytes", selected.len()));
                self.hand_out(Opened { channel: Some(BASIC_CHANNEL), basic: true, selected }, reply);
            }
            OPEN_LOGICAL_CHANNEL => {
                let aid = call.blob().context("read which applet is asked for")?;
                let p2 = call.i32().context("read how the applet is to be selected")? as u8;
                let (channel, selected) = self.element.open_logical_channel(&aid, p2)?;
                let _ = skyward_sys::kmsg(&format!("omapi: channel {channel} open, the applet answered {} bytes", selected.len()));
                self.hand_out(Opened { channel: Some(channel), basic: false, selected }, reply);
            }
            _ => bail!("the session has no method {code}"),
        }
        Ok(())
    }
}

/// A channel to an applet, through which the element's own protocol goes.
struct Channel {
    element: Rc<Element>,
    /// This channel's own record, which no other channel shares: what the
    /// element calls this one, and what its applet answered.
    opened: Rc<RefCell<Opened>>,
}

impl Object for Channel {
    fn on_transact(&self, code: u32, call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("omapi: the channel is asked {code}"));
        if common(code, OMAPI_CHANNEL, reply) {
            return Ok(());
        }
        match code {
            CHANNEL_CLOSE => {
                let channel = self.opened.borrow_mut().channel.take();
                if let Some(number) = channel {
                    self.element.close_channel(number)?;
                }
                *reply = answering();
            }
            CHANNEL_CLOSED => {
                let closed = self.opened.borrow().channel.is_none();
                *reply = answering();
                reply.bool(closed);
            }
            CHANNEL_BASIC => {
                *reply = answering();
                reply.bool(self.opened.borrow().basic);
            }
            SELECT_RESPONSE => {
                let selected = self.opened.borrow().selected.clone();
                *reply = answering();
                reply.blob(&selected);
            }
            TRANSMIT => {
                let mut command = call.blob().context("read what is to be sent to the element")?;
                // The element's own service keeps no channel of its own: the
                // one call it has for sending to the element is the basic
                // channel's, and an applet reached on another channel is
                // reached by naming that channel in the first byte of the
                // APDU, as ISO 7816-4 has it. So the number the element gave
                // when it opened this channel is put into that byte here,
                // which is what the running system's own layer does and what
                // a for-the-element APDU leaves unsaid. A basic channel is
                // numbered 0 and leaves the byte alone.
                if let (Some(number), Some(first)) =
                    (self.opened.borrow().channel, command.first_mut())
                {
                    *first |= number as u8;
                }
                let answer = self.element.transmit(&command)?;
                *reply = answering();
                reply.blob(&answer);
            }
            SELECT_NEXT => {
                *reply = answering();
                reply.bool(false);
            }
            _ => bail!("the channel has no method {code}"),
        }
        Ok(())
    }
}

/// What the element calls back on when it comes and goes. Nothing here waits
/// on that, but the element will not open until it has somewhere to say it.
struct Callback;

impl Object for Callback {
    fn on_transact(&self, code: u32, call: &mut Reader<'_>, reply: &mut Parcel) -> Result<()> {
        let _ = skyward_sys::kmsg(&format!("omapi: the element called back {code}"));
        if common(code, ELEMENT_CALLBACK, reply) {
            return Ok(());
        }
        match code {
            ELEMENT_STATE_CHANGED => {
                let here = call.bool().unwrap_or(false);
                let _ = skyward_sys::kmsg(&format!("omapi: the element is {}", if here { "there" } else { "gone" }));
                *reply = answering();
            }
            _ => bail!("the element's callback has no method {code}"),
        }
        Ok(())
    }
}

/// Serve the layer for the element the phone registers under `reader`, and
/// go on serving it. Returns only when the driver or the program ends.
///
/// The element's own service has to be running before this, because the
/// element is reached through it; and this has to be registered before
/// whatever asks for it — the weaver — is started.
pub fn serve(reader: &str) -> Result<()> {
    let binder = Rc::new(Binder::open().context("open the driver the layer is served over")?);
    // The element calls back on this the moment it is opened, and that is
    // before this end has sat down to listen, so it is taken on first.
    binder.serving(vec![(CALLBACK_OBJECT, Rc::new(Callback) as Rc<dyn Object>)]);
    let element = Rc::new(Element::open(Rc::clone(&binder), reader)?);
    let session = Rc::new(Session {
        element: Rc::clone(&element),
        opened: RefCell::new(Vec::new()),
        closed: Cell::new(false),
        next: Cell::new(FIRST_CHANNEL_OBJECT),
    });
    binder.serving(vec![
        (SERVICE_OBJECT, Rc::new(Layer { reader: reader.to_string() }) as Rc<dyn Object>),
        (READER_OBJECT, Rc::new(ElementReader { element: Rc::clone(&element), session: Rc::clone(&session) }) as Rc<dyn Object>),
        (SESSION_OBJECT, Rc::clone(&session) as Rc<dyn Object>),
    ]);
    service::add(&binder, SERVICE, SERVICE_OBJECT).with_context(|| format!("offer {SERVICE} to the phone"))?;
    // A line that cannot be written is no reason to stop serving: the
    // weaver is already waiting on what is answered below.
    let _ = skyward_sys::kmsg(&format!("omapi: serving {SERVICE} for {reader}"));
    // Whatever this program learned about the element before, it is the
    // weaver's own calls that matter, and those come in over the loop
    // below. Nothing here may end before that loop is reached: a weaver
    // that finds the layer gone waits for it forever.
    binder.serve()
}
