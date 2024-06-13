//! Bindings to Windows IOCP with `ProcessSocketNotifications` and
//! `NtAssociateWaitCompletionPacket` support.
//!
//! `ProcessSocketNotifications` is a new Windows API after 21H1. It is much
//! like kqueue, and support edge triggers. There are some behaviors different
//! from other platforms:
//! - It distingushes "disabled" state and "removed" state. When the
//!   registration disabled, the notifications won't be queued to the poller.
//! - The edge trigger only triggers condition changes after it is enabled. You
//!   cannot expect an event coming if you change the condition before
//!   registering the notification.
//! - A socket can be registered to only one IOCP at a time.
//!
//! `NtAssociateWaitCompletionPacket` is an undocumented API and it's the back
//! of thread pool APIs like `RegisterWaitForSingleObject`. We use it to avoid
//! starting thread pools. It only supports `Oneshot` mode.

#![warn(missing_docs)]

pub mod ffi;
mod wait;

use std::{
    collections::BTreeMap,
    io,
    mem::MaybeUninit,
    os::windows::io::{
        AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle, RawSocket,
    },
    ptr::null_mut,
    sync::{Mutex, RwLock},
    time::Duration,
};

use wait::WaitCompletionPacket;
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE, WAIT_IO_COMPLETION, WAIT_TIMEOUT},
    Networking::WinSock::{
        ProcessSocketNotifications, SOCK_NOTIFY_EVENT_ERR, SOCK_NOTIFY_EVENT_HANGUP,
        SOCK_NOTIFY_EVENT_IN, SOCK_NOTIFY_EVENT_OUT, SOCK_NOTIFY_EVENT_REMOVE,
        SOCK_NOTIFY_OP_DISABLE, SOCK_NOTIFY_OP_ENABLE, SOCK_NOTIFY_OP_REMOVE,
        SOCK_NOTIFY_REGISTER_EVENT_HANGUP, SOCK_NOTIFY_REGISTER_EVENT_IN,
        SOCK_NOTIFY_REGISTER_EVENT_NONE, SOCK_NOTIFY_REGISTER_EVENT_OUT, SOCK_NOTIFY_REGISTRATION,
        SOCK_NOTIFY_TRIGGER_EDGE, SOCK_NOTIFY_TRIGGER_LEVEL, SOCK_NOTIFY_TRIGGER_ONESHOT,
        SOCK_NOTIFY_TRIGGER_PERSISTENT,
    },
    System::{
        Threading::INFINITE,
        IO::{
            CreateIoCompletionPort, GetQueuedCompletionStatusEx, PostQueuedCompletionStatus,
            OVERLAPPED, OVERLAPPED_ENTRY,
        },
    },
};

/// Macro to lock and ignore lock poisoning.
macro_rules! lock {
    ($lock_result:expr) => {{ $lock_result.unwrap_or_else(|e| e.into_inner()) }};
}

/// The mode in which the poller waits for I/O events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PollMode {
    /// Poll in oneshot mode.
    ///
    /// In this mode, the poller will only deliver one event per socket.
    /// Once an event has been delivered, interest in the event needs to be
    /// re-enabled by calling [`Poller::modify`].
    Oneshot,

    /// Poll in level-triggered mode.
    ///
    /// Once an event has been delivered, polling will continue to deliver that
    /// event until interest in the event is disabled by calling
    /// [`Poller::modify`] or [`Poller::delete`].
    Level,

    /// Poll in edge-triggered mode.
    ///
    /// Once an event has been delivered, polling will not deliver that event
    /// again unless a new event occurs.
    ///
    /// The edge trigger only triggers condition changes. If a socket is
    /// readable, and a readable event has already been delivered, no more
    /// readable event will be delivered until the socket inner buffer be
    /// cleared.
    Edge,

    /// Poll in both edge-triggered and oneshot mode.
    ///
    /// This mode is similar to the `Oneshot` mode, but it will only deliver one
    /// event per new event.
    ///
    /// No events will be queued after an event is delivered. Register the
    /// interest before the condition changes.
    EdgeOneshot,
}

/// Interface to kqueue.
#[derive(Debug)]
pub struct Poller {
    /// The I/O completion port.
    port: OwnedHandle,

    /// The state of the sources registered with this poller.
    ///
    /// Each source is keyed by its raw socket ID.
    sources: RwLock<BTreeMap<RawSocket, usize>>,

    /// The state of the waitable handles registered with this poller.
    waitables: Mutex<BTreeMap<RawHandle, WaitableAttr>>,
}

/// A waitable object with key and [`WaitCompletionPacket`].
///
/// [`WaitCompletionPacket`]: wait::WaitCompletionPacket
#[derive(Debug)]
struct WaitableAttr {
    key: usize,
    packet: wait::WaitCompletionPacket,
}

impl Poller {
    /// Creates a new poller.
    pub fn new() -> io::Result<Self> {
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, 0, 0, 0) };
        if handle == 0 {
            return Err(io::Error::last_os_error());
        }

        let port = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        Ok(Poller {
            port,
            sources: RwLock::default(),
            waitables: Mutex::default(),
        })
    }

    /// Adds a new socket.
    pub fn add(&self, socket: RawSocket, interest: Event, mode: PollMode) -> io::Result<()> {
        let mut sources = lock!(self.sources.write());
        if sources.contains_key(&socket) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        sources.insert(socket, interest.key);

        let info = create_registration(socket, interest, mode, true);
        self.update_source(info)
    }

    /// Modifies an existing socket.
    pub fn modify(&self, socket: RawSocket, interest: Event, mode: PollMode) -> io::Result<()> {
        let sources = lock!(self.sources.read());
        let oldkey = sources
            .get(&socket)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

        if oldkey != &interest.key {
            // To change the key, remove the old registration and wait for REMOVE event.
            let info = create_registration(socket, Event::none(*oldkey), PollMode::Oneshot, false);
            self.update_and_wait_for_remove(info, *oldkey)?;
        }
        let info = create_registration(socket, interest, mode, true);
        self.update_source(info)
    }

    /// Deletes a socket.
    pub fn delete(&self, socket: RawSocket) -> io::Result<()> {
        let key = lock!(self.sources.write())
            .remove(&socket)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let info = create_registration(socket, Event::none(key), PollMode::Oneshot, false);
        self.update_and_wait_for_remove(info, key)
    }

    /// Add a new waitable to the poller.
    pub fn add_waitable(&self, handle: RawHandle, interest: Event) -> io::Result<()> {
        let key = interest.key;

        let mut waitables = lock!(self.waitables.lock());
        if waitables.contains_key(&handle) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }

        let mut packet = wait::WaitCompletionPacket::new()?;
        packet.associate(
            self.port.as_raw_handle(),
            handle,
            key,
            interest_to_events(&interest) as _,
        )?;
        waitables.insert(handle, WaitableAttr { key, packet });
        Ok(())
    }

    /// Update a waitable in the poller.
    pub fn modify_waitable(&self, waitable: RawHandle, interest: Event) -> io::Result<()> {
        let mut waitables = lock!(self.waitables.lock());
        let WaitableAttr { key, packet } = waitables
            .get_mut(&waitable)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

        let cancelled = packet.cancel()?;
        if !cancelled {
            // The packet could not be reused, create a new one.
            *packet = WaitCompletionPacket::new()?;
        }
        packet.associate(
            self.port.as_raw_handle(),
            waitable,
            *key,
            interest_to_events(&interest) as _,
        )
    }

    /// Delete a waitable from the poller.
    pub fn delete_waitable(&self, waitable: RawHandle) -> io::Result<()> {
        let WaitableAttr { mut packet, .. } = lock!(self.waitables.lock())
            .remove(&waitable)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

        packet.cancel()?;
        Ok(())
    }

    /// Add or modify the registration.
    fn update_source(&self, mut reg: SOCK_NOTIFY_REGISTRATION) -> io::Result<()> {
        let res = unsafe {
            ProcessSocketNotifications(
                self.port.as_raw_handle() as _,
                1,
                &mut reg,
                0,
                0,
                null_mut(),
                null_mut(),
            )
        };
        if res == ERROR_SUCCESS {
            if reg.registrationResult == ERROR_SUCCESS {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(reg.registrationResult as _))
            }
        } else {
            Err(io::Error::from_raw_os_error(res as _))
        }
    }

    /// Attempt to remove a registration, and wait for the
    /// `SOCK_NOTIFY_EVENT_REMOVE` event.
    fn update_and_wait_for_remove(
        &self,
        mut reg: SOCK_NOTIFY_REGISTRATION,
        key: usize,
    ) -> io::Result<()> {
        debug_assert_eq!(reg.operation, SOCK_NOTIFY_OP_REMOVE as _);
        let mut received = 0;
        let mut entry: MaybeUninit<OVERLAPPED_ENTRY> = MaybeUninit::uninit();

        let repost = |entry: OVERLAPPED_ENTRY| {
            self.post_raw(
                entry.dwNumberOfBytesTransferred,
                entry.lpCompletionKey,
                entry.lpOverlapped,
            )
        };

        // Update the registration and wait for the event in the same time.
        // However, the returned completion entry may not be the wanted REMOVE event.
        let res = unsafe {
            ProcessSocketNotifications(
                self.port.as_raw_handle() as _,
                1,
                &mut reg,
                0,
                1,
                entry.as_mut_ptr().cast(),
                &mut received,
            )
        };
        match res {
            ERROR_SUCCESS | WAIT_TIMEOUT => {
                if reg.registrationResult != ERROR_SUCCESS {
                    // If the registration is not successful, the received entry should be reposted.
                    if received == 1 {
                        repost(unsafe { entry.assume_init() })?;
                    }
                    return Err(io::Error::from_raw_os_error(reg.registrationResult as _));
                }
            }
            _ => return Err(io::Error::from_raw_os_error(res as _)),
        }
        if received == 1 {
            // The registration is successful, and check the received entry.
            let entry = unsafe { entry.assume_init() };
            if entry.lpCompletionKey == key {
                // If the entry is current key but not the remove event, just ignore it.
                if (entry.dwNumberOfBytesTransferred & SOCK_NOTIFY_EVENT_REMOVE) != 0 {
                    return Ok(());
                }
            } else {
                repost(entry)?;
            }
        }

        // No wanted event, start a loop to wait for it.
        // TODO: any better solutions?
        loop {
            let res = unsafe {
                ProcessSocketNotifications(
                    self.port.as_raw_handle() as _,
                    0,
                    null_mut(),
                    0,
                    1,
                    entry.as_mut_ptr().cast(),
                    &mut received,
                )
            };
            match res {
                ERROR_SUCCESS => {
                    debug_assert_eq!(received, 1);
                    let entry = unsafe { entry.assume_init() };
                    if entry.lpCompletionKey == key {
                        if (entry.dwNumberOfBytesTransferred & SOCK_NOTIFY_EVENT_REMOVE) != 0 {
                            return Ok(());
                        }
                    } else {
                        repost(entry)?;
                    }
                }
                WAIT_TIMEOUT => {}
                _ => return Err(io::Error::from_raw_os_error(res as _)),
            }
        }
    }

    /// Waits for I/O events with an optional timeout.
    pub fn wait(
        &self,
        events: &mut [MaybeUninit<OVERLAPPED_ENTRY>],
        timeout: Option<Duration>,
        alertable: bool,
    ) -> io::Result<usize> {
        let timeout = timeout.map_or(INFINITE, dur2timeout);
        let mut received = 0;
        let res = unsafe {
            GetQueuedCompletionStatusEx(
                self.port.as_raw_handle() as _,
                events.as_mut_ptr().cast(),
                events.len() as _,
                &mut received,
                timeout,
                alertable as _,
            )
        };
        match res as u32 {
            ERROR_SUCCESS => Ok(received as _),
            WAIT_TIMEOUT | WAIT_IO_COMPLETION => Ok(0),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Push an IOCP packet into the queue.
    pub fn post(&self, event: Event) -> io::Result<()> {
        self.post_raw(interest_to_events(&event), event.key, null_mut())
    }

    fn post_raw(
        &self,
        transferred: u32,
        key: usize,
        overlapped: *mut OVERLAPPED,
    ) -> io::Result<()> {
        let res = unsafe {
            PostQueuedCompletionStatus(self.port.as_raw_handle() as _, transferred, key, overlapped)
        };
        if res == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl AsRawHandle for Poller {
    fn as_raw_handle(&self) -> RawHandle {
        self.port.as_raw_handle()
    }
}

impl AsHandle for Poller {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.port.as_handle()
    }
}

unsafe impl Send for Poller {}
unsafe impl Sync for Poller {}

/// Indicates that a socket can read or write without blocking.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Event {
    /// The available events.
    pub events: u32,
    /// Key identifying the socket.
    pub key: usize,
}

impl Event {
    /// Create an event with no interest.
    pub fn none(key: usize) -> Self {
        Self { events: 0, key }
    }

    fn set_event(&mut self, e: u32, value: bool) {
        if value {
            self.events |= e;
        } else {
            self.events &= !e;
        }
    }

    fn get_event(&self, e: u32) -> bool {
        (self.events & e) != 0
    }

    /// Interest in readable event.
    pub fn set_readable(&mut self, value: bool) {
        self.set_event(SOCK_NOTIFY_EVENT_IN, value)
    }

    /// Interest in writable event.
    pub fn set_writable(&mut self, value: bool) {
        self.set_event(SOCK_NOTIFY_EVENT_OUT, value)
    }

    /// Interest in hangup event.
    pub fn set_hangup(&mut self, value: bool) {
        self.set_event(SOCK_NOTIFY_EVENT_HANGUP, value)
    }

    /// Interest in error event.
    pub fn set_error(&mut self, value: bool) {
        self.set_event(SOCK_NOTIFY_EVENT_ERR, value)
    }

    /// Is readable event.
    pub fn is_readable(&self) -> bool {
        self.get_event(SOCK_NOTIFY_EVENT_IN)
    }

    /// Is writable event.
    pub fn is_writable(&self) -> bool {
        self.get_event(SOCK_NOTIFY_EVENT_OUT)
    }

    /// Is hangup event.
    pub fn is_hangup(&self) -> bool {
        self.get_event(SOCK_NOTIFY_EVENT_HANGUP)
    }

    /// Is error event.
    pub fn is_error(&self) -> bool {
        self.get_event(SOCK_NOTIFY_EVENT_ERR)
    }
}

impl From<OVERLAPPED_ENTRY> for Event {
    fn from(value: OVERLAPPED_ENTRY) -> Self {
        Self {
            events: value.dwNumberOfBytesTransferred,
            key: value.lpCompletionKey,
        }
    }
}

pub(crate) fn interest_to_filter(interest: &Event) -> u16 {
    let mut filter = SOCK_NOTIFY_REGISTER_EVENT_NONE;
    if interest.is_readable() {
        filter |= SOCK_NOTIFY_REGISTER_EVENT_IN;
    }
    if interest.is_writable() {
        filter |= SOCK_NOTIFY_REGISTER_EVENT_OUT;
    }
    if interest.is_hangup() {
        filter |= SOCK_NOTIFY_REGISTER_EVENT_HANGUP;
    }
    filter as _
}

pub(crate) fn interest_to_events(interest: &Event) -> u32 {
    let mut events = 0;
    if interest.is_readable() {
        events |= SOCK_NOTIFY_EVENT_IN;
    }
    if interest.is_writable() {
        events |= SOCK_NOTIFY_EVENT_OUT;
    }
    if interest.is_hangup() {
        events |= SOCK_NOTIFY_EVENT_HANGUP;
    }
    if interest.is_error() {
        events |= SOCK_NOTIFY_EVENT_ERR;
    }
    events
}

pub(crate) fn mode_to_flags(mode: PollMode) -> u8 {
    let flags = match mode {
        PollMode::Oneshot => SOCK_NOTIFY_TRIGGER_ONESHOT | SOCK_NOTIFY_TRIGGER_LEVEL,
        PollMode::Level => SOCK_NOTIFY_TRIGGER_PERSISTENT | SOCK_NOTIFY_TRIGGER_LEVEL,
        PollMode::Edge => SOCK_NOTIFY_TRIGGER_PERSISTENT | SOCK_NOTIFY_TRIGGER_EDGE,
        PollMode::EdgeOneshot => SOCK_NOTIFY_TRIGGER_ONESHOT | SOCK_NOTIFY_TRIGGER_EDGE,
    };
    flags as u8
}

pub(crate) fn create_registration(
    socket: RawSocket,
    interest: Event,
    mode: PollMode,
    enable: bool,
) -> SOCK_NOTIFY_REGISTRATION {
    let filter = interest_to_filter(&interest);
    SOCK_NOTIFY_REGISTRATION {
        socket: socket as _,
        completionKey: interest.key as _,
        eventFilter: filter,
        operation: if enable {
            if filter == SOCK_NOTIFY_REGISTER_EVENT_NONE as _ {
                SOCK_NOTIFY_OP_DISABLE as _
            } else {
                SOCK_NOTIFY_OP_ENABLE as _
            }
        } else {
            SOCK_NOTIFY_OP_REMOVE as _
        },
        triggerFlags: mode_to_flags(mode),
        registrationResult: 0,
    }
}

// Implementation taken from https://github.com/rust-lang/rust/blob/db5476571d9b27c862b95c1e64764b0ac8980e23/src/libstd/sys/windows/mod.rs
fn dur2timeout(dur: Duration) -> u32 {
    // Note that a duration is a (u64, u32) (seconds, nanoseconds) pair, and the
    // timeouts in windows APIs are typically u32 milliseconds. To translate, we
    // have two pieces to take care of:
    //
    // * Nanosecond precision is rounded up
    // * Greater than u32::MAX milliseconds (50 days) is rounded up to INFINITE
    //   (never time out).
    dur.as_secs()
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add((dur.subsec_nanos() as u64) / 1_000_000))
        .and_then(|ms| {
            if dur.subsec_nanos() % 1_000_000 > 0 {
                ms.checked_add(1)
            } else {
                Some(ms)
            }
        })
        .and_then(|x| u32::try_from(x).ok())
        .unwrap_or(INFINITE)
}
