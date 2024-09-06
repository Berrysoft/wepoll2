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

#![feature(allocator_api, try_blocks, build_hasher_default_const_new)]
#![warn(missing_docs)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod ffi;
mod io;
mod lock;
mod map;
mod wait;

use core::{mem::MaybeUninit, ptr::null_mut, time::Duration};

use hashbrown::TryReserveError;
use io::OwnedHandle;
pub use io::{Error, Result};
use map::HashMap;
use wait::WaitCompletionPacket;
use windows_sys::Win32::{
    Foundation::{
        RtlNtStatusToDosError, BOOLEAN, ERROR_ALREADY_EXISTS, ERROR_NOT_ENOUGH_MEMORY,
        ERROR_NOT_ENOUGH_QUOTA, ERROR_NOT_FOUND, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE,
        NTSTATUS, STATUS_SUCCESS, STATUS_TIMEOUT, STATUS_USER_APC, WAIT_TIMEOUT,
    },
    Networking::WinSock::{
        ProcessSocketNotifications, SOCKET, SOCK_NOTIFY_EVENT_ERR, SOCK_NOTIFY_EVENT_HANGUP,
        SOCK_NOTIFY_EVENT_IN, SOCK_NOTIFY_EVENT_OUT, SOCK_NOTIFY_EVENT_REMOVE,
        SOCK_NOTIFY_OP_DISABLE, SOCK_NOTIFY_OP_ENABLE, SOCK_NOTIFY_OP_REMOVE,
        SOCK_NOTIFY_REGISTER_EVENT_HANGUP, SOCK_NOTIFY_REGISTER_EVENT_IN,
        SOCK_NOTIFY_REGISTER_EVENT_NONE, SOCK_NOTIFY_REGISTER_EVENT_OUT, SOCK_NOTIFY_REGISTRATION,
        SOCK_NOTIFY_TRIGGER_EDGE, SOCK_NOTIFY_TRIGGER_LEVEL, SOCK_NOTIFY_TRIGGER_ONESHOT,
        SOCK_NOTIFY_TRIGGER_PERSISTENT,
    },
    System::IO::{
        CreateIoCompletionPort, PostQueuedCompletionStatus, OVERLAPPED, OVERLAPPED_ENTRY,
    },
};

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
    sources: HashMap<SOCKET, usize>,

    /// The state of the waitable handles registered with this poller.
    waitables: HashMap<HANDLE, WaitableAttr>,
}

unsafe impl Send for Poller {}
unsafe impl Sync for Poller {}

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
    pub fn new() -> Result<Self> {
        let handle = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 0) };
        if handle.is_null() {
            return Err(Error::last_os_error());
        }

        let port = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Poller {
            port,
            sources: HashMap::new(),
            waitables: HashMap::new(),
        })
    }

    /// Adds a new socket.
    pub fn add(&mut self, socket: SOCKET, interest: Event, mode: PollMode) -> Result<()> {
        if self.sources.contains_key(&socket) {
            return Err(Error(ERROR_ALREADY_EXISTS));
        }
        self.sources
            .try_insert(socket, interest.key())
            .map_err(map_try_reserve_error)?;

        let info = create_registration(socket, interest, mode, true);
        self.update_source(info)
    }

    /// Modifies an existing socket.
    pub fn modify(&self, socket: SOCKET, interest: Event, mode: PollMode) -> Result<()> {
        let oldkey = self.sources.get(&socket).ok_or(Error(ERROR_NOT_FOUND))?;

        if oldkey != &interest.key() {
            // To change the key, remove the old registration and wait for REMOVE event.
            let info = create_registration(socket, Event::none(*oldkey), PollMode::Oneshot, false);
            self.update_and_wait_for_remove(info, *oldkey)?;
        }
        let info = create_registration(socket, interest, mode, true);
        self.update_source(info)
    }

    /// Deletes a socket.
    pub fn delete(&mut self, socket: SOCKET) -> Result<()> {
        let key = self.sources.remove(&socket).ok_or(Error(ERROR_NOT_FOUND))?;
        let info = create_registration(socket, Event::none(key), PollMode::Oneshot, false);
        self.update_and_wait_for_remove(info, key)
    }

    /// Add a new waitable to the poller.
    pub fn add_waitable(&mut self, handle: HANDLE, interest: Event) -> Result<()> {
        let key = interest.key();
        if self.waitables.contains_key(&handle) {
            return Err(Error(ERROR_ALREADY_EXISTS));
        }

        let mut packet = wait::WaitCompletionPacket::new()?;
        packet.associate(
            self.port.as_raw_handle(),
            handle,
            key,
            interest_to_events(&interest) as _,
        )?;
        self.waitables
            .try_insert(handle, WaitableAttr { key, packet })
            .map_err(map_try_reserve_error)?;
        Ok(())
    }

    /// Update a waitable in the poller.
    pub fn modify_waitable(&mut self, waitable: HANDLE, interest: Event) -> Result<()> {
        let WaitableAttr { key, packet } = self
            .waitables
            .get_mut(&waitable)
            .ok_or(Error(ERROR_NOT_FOUND))?;

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
    pub fn delete_waitable(&mut self, waitable: HANDLE) -> Result<()> {
        let WaitableAttr { mut packet, .. } = self
            .waitables
            .remove(&waitable)
            .ok_or(Error(ERROR_NOT_FOUND))?;

        packet.cancel()?;
        Ok(())
    }

    /// Add or modify the registration.
    fn update_source(&self, mut reg: SOCK_NOTIFY_REGISTRATION) -> Result<()> {
        let res = unsafe {
            ProcessSocketNotifications(
                self.port.as_raw_handle(),
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
                Err(Error(reg.registrationResult))
            }
        } else {
            Err(Error(res))
        }
    }

    /// Attempt to remove a registration, and wait for the
    /// `SOCK_NOTIFY_EVENT_REMOVE` event.
    fn update_and_wait_for_remove(
        &self,
        mut reg: SOCK_NOTIFY_REGISTRATION,
        key: usize,
    ) -> Result<()> {
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
                self.port.as_raw_handle(),
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
                    return Err(Error(reg.registrationResult));
                }
            }
            _ => return Err(Error(res)),
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
                    self.port.as_raw_handle(),
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
                _ => return Err(Error(res)),
            }
        }
    }

    /// Waits for I/O events with an optional timeout.
    pub fn wait(
        &self,
        events: &mut [MaybeUninit<Event>],
        timeout: Option<Duration>,
        alertable: bool,
    ) -> Result<usize> {
        #[link(name = "ntdll")]
        extern "system" {
            fn NtRemoveIoCompletionEx(
                handle: HANDLE,
                information: *mut MaybeUninit<OVERLAPPED_ENTRY>,
                count: u32,
                removed: *mut u32,
                timeout: Option<&mut u64>,
                alertable: BOOLEAN,
            ) -> NTSTATUS;
        }

        let mut timeout: Option<u64> = timeout.and_then(|dur| {
            dur.as_secs()
                .checked_mul(10_000_000)
                .and_then(|ns| ns.checked_add(dur.subsec_nanos().div_ceil(100) as _))
                .and_then(|ns| (ns as i64).checked_neg())
                .map(|ns| ns as u64)
        });
        let mut received = 0;
        let res = unsafe {
            NtRemoveIoCompletionEx(
                self.port.as_raw_handle(),
                events.as_mut_ptr().cast(),
                events.len() as _,
                &mut received,
                timeout.as_mut(),
                alertable as _,
            )
        };
        match res {
            STATUS_SUCCESS => Ok(received as _),
            STATUS_TIMEOUT | STATUS_USER_APC => Ok(0),
            _ => Err(Error(unsafe { RtlNtStatusToDosError(res) })),
        }
    }

    /// Push an IOCP packet into the queue.
    pub fn post(&self, event: Event) -> Result<()> {
        self.post_raw(interest_to_events(&event), event.key(), null_mut())
    }

    fn post_raw(&self, transferred: u32, key: usize, overlapped: *mut OVERLAPPED) -> Result<()> {
        let res = unsafe {
            PostQueuedCompletionStatus(self.port.as_raw_handle(), transferred, key, overlapped)
        };
        if res == 0 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Indicates that a socket can read or write without blocking.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Event(pub OVERLAPPED_ENTRY);

impl Event {
    /// Create an event with no interest.
    pub fn none(key: usize) -> Self {
        Self(OVERLAPPED_ENTRY {
            lpCompletionKey: key,
            lpOverlapped: null_mut(),
            Internal: 0,
            dwNumberOfBytesTransferred: 0,
        })
    }

    /// Key of the event.
    pub const fn key(&self) -> usize {
        self.0.lpCompletionKey
    }

    /// The flags of the event.
    pub const fn events(&self) -> u32 {
        self.0.dwNumberOfBytesTransferred
    }

    fn set_event(&mut self, e: u32, value: bool) {
        if value {
            self.0.dwNumberOfBytesTransferred |= e;
        } else {
            self.0.dwNumberOfBytesTransferred &= !e;
        }
    }

    fn get_event(&self, e: u32) -> bool {
        (self.events() & e) != 0
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

    /// Interest in readable event.
    pub fn with_readable(mut self, value: bool) -> Self {
        self.set_readable(value);
        self
    }

    /// Interest in writable event.
    pub fn with_writable(mut self, value: bool) -> Self {
        self.set_writable(value);
        self
    }

    /// Interest in hangup event.
    pub fn with_hangup(mut self, value: bool) -> Self {
        self.set_hangup(value);
        self
    }

    /// Interest in error event.
    pub fn with_error(mut self, value: bool) -> Self {
        self.set_error(value);
        self
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

fn interest_to_filter(interest: &Event) -> u16 {
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

fn interest_to_events(interest: &Event) -> u32 {
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

fn mode_to_flags(mode: PollMode) -> u8 {
    let flags = match mode {
        PollMode::Oneshot => SOCK_NOTIFY_TRIGGER_ONESHOT | SOCK_NOTIFY_TRIGGER_LEVEL,
        PollMode::Level => SOCK_NOTIFY_TRIGGER_PERSISTENT | SOCK_NOTIFY_TRIGGER_LEVEL,
        PollMode::Edge => SOCK_NOTIFY_TRIGGER_PERSISTENT | SOCK_NOTIFY_TRIGGER_EDGE,
        PollMode::EdgeOneshot => SOCK_NOTIFY_TRIGGER_ONESHOT | SOCK_NOTIFY_TRIGGER_EDGE,
    };
    flags as u8
}

fn create_registration(
    socket: SOCKET,
    interest: Event,
    mode: PollMode,
    enable: bool,
) -> SOCK_NOTIFY_REGISTRATION {
    let filter = interest_to_filter(&interest);
    SOCK_NOTIFY_REGISTRATION {
        socket,
        completionKey: interest.key() as _,
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

fn map_try_reserve_error(e: TryReserveError) -> Error {
    match e {
        TryReserveError::AllocError { .. } => Error(ERROR_NOT_ENOUGH_MEMORY),
        TryReserveError::CapacityOverflow => Error(ERROR_NOT_ENOUGH_QUOTA),
    }
}
