//! FFI of this crate. Imitate epoll(2).

use alloc::{boxed::Box, vec::Vec};
use core::{
    ffi::c_int,
    mem::MaybeUninit,
    ptr::{null, null_mut},
    time::Duration,
};

use either::Either;
use windows_sys::Win32::{
    Foundation::{SetLastError, ERROR_INVALID_PARAMETER, ERROR_NOT_ENOUGH_MEMORY, HANDLE},
    Networking::WinSock::{WSAGetLastError, WSAGetQOSByName, SOCKET, WSAENOTSOCK},
    System::IO::OVERLAPPED_ENTRY,
};

use crate::{Error, Event, PollMode, Poller, Result};

#[inline]
fn io_result_ok<T>(res: Result<T>) -> Option<T> {
    match res {
        Ok(value) => Some(value),
        Err(e) => {
            unsafe { SetLastError(e.0) };
            None
        }
    }
}

#[inline]
fn io_result_ret(res: Result<c_int>) -> c_int {
    io_result_ok(res).unwrap_or(-1)
}

#[inline]
fn check_pointer<'a, T>(ptr: *const T) -> Result<&'a T> {
    if ptr.is_aligned() && !ptr.is_null() {
        Ok(unsafe { &*ptr })
    } else {
        Err(Error(ERROR_INVALID_PARAMETER))
    }
}

#[inline]
fn check_pointer_mut<'a, T>(ptr: *mut T) -> Result<&'a mut T> {
    if ptr.is_aligned() && !ptr.is_null() {
        Ok(unsafe { &mut *ptr })
    } else {
        Err(Error(ERROR_INVALID_PARAMETER))
    }
}

/// Readable event.
pub const EPOLLIN: c_int = 1 << 0;
/// Writable event.
pub const EPOLLOUT: c_int = 1 << 1;
/// Hangup event.
pub const EPOLLHUP: c_int = 1 << 2;
/// Error event.
pub const EPOLLERR: c_int = 1 << 6;
/// Edge trigger.
pub const EPOLLET: c_int = 1 << 8;
/// Oneshot trigger.
pub const EPOLLONESHOT: c_int = 1 << 9;

/// Add an entry.
pub const EPOLL_CTL_ADD: c_int = 1;
/// Modify an entry.
pub const EPOLL_CTL_MOD: c_int = 2;
/// Delete an entry.
pub const EPOLL_CTL_DEL: c_int = 3;

#[inline(never)]
fn epoll_try_create() -> Result<Box<Poller>> {
    Box::try_new(Poller::new()?).map_err(|_| Error(ERROR_NOT_ENOUGH_MEMORY))
}

/// Create a new wepoll instance. `size` should be positive.
#[no_mangle]
#[deprecated]
pub extern "C" fn epoll_create(size: c_int) -> Option<Box<Poller>> {
    io_result_ok({
        if size <= 0 {
            Err(Error(ERROR_INVALID_PARAMETER))
        } else {
            epoll_try_create()
        }
    })
}

/// Create a new wepoll instance. `flags` should be zero.
#[no_mangle]
pub extern "C" fn epoll_create1(flags: c_int) -> Option<Box<Poller>> {
    io_result_ok({
        if flags != 0 {
            Err(Error(ERROR_INVALID_PARAMETER))
        } else {
            epoll_try_create()
        }
    })
}

/// Close a wepoll instance.
#[no_mangle]
pub extern "C" fn epoll_close(poller: Option<Box<Poller>>) -> c_int {
    io_result_ret({
        if let Some(poller) = poller {
            drop(poller);
            Ok(0)
        } else {
            Err(Error(ERROR_INVALID_PARAMETER))
        }
    })
}

#[inline(never)]
unsafe fn epoll_wait_duration(
    poller: *const Poller,
    events: *mut Event,
    len: c_int,
    timeout: Option<Duration>,
    alertable: bool,
) -> c_int {
    io_result_ret(
        try {
            let poller = check_pointer(poller)?;
            if len != 0 {
                check_pointer(events)?;
            }
            let len = len as usize;
            let events: &mut [MaybeUninit<Event>] =
                unsafe { core::slice::from_raw_parts_mut(events.cast(), len) };

            const STATIC_ENTRIES_COUNT: usize = 256;

            let mut static_entries: [MaybeUninit<OVERLAPPED_ENTRY>; STATIC_ENTRIES_COUNT] =
                [MaybeUninit::uninit(); STATIC_ENTRIES_COUNT];
            let mut spare_entries = if len > STATIC_ENTRIES_COUNT {
                Either::Left(
                    Vec::try_with_capacity(len).map_err(|_| Error(ERROR_NOT_ENOUGH_MEMORY))?,
                )
            } else {
                Either::Right(&mut static_entries)
            };
            let entries_mut = AsMut::as_mut(&mut spare_entries);

            let len = poller.wait(entries_mut, timeout, alertable)?;

            for (ev, entry) in events.get_unchecked_mut(..len).iter_mut().zip(entries_mut) {
                ev.write(Event::from(MaybeUninit::assume_init_ref(entry)));
            }

            len as _
        },
    )
}

/// Wait for events on the wepoll instance.
///
/// # Safety
///
/// Given pointer should be valid.
#[no_mangle]
pub unsafe extern "C" fn epoll_wait(
    poller: *const Poller,
    events: *mut Event,
    len: c_int,
    timeout: c_int,
) -> c_int {
    epoll_pwait(poller, events, len, timeout, false)
}

/// Wait for events on the wepoll instance.
///
/// `alterable` indicates whether the wait is alertable.
///
/// # Safety
///
/// Given pointer should be valid.
#[no_mangle]
#[inline(never)]
pub unsafe extern "C" fn epoll_pwait(
    poller: *const Poller,
    events: *mut Event,
    len: c_int,
    timeout: c_int,
    alertable: bool,
) -> c_int {
    let timeout = if timeout == -1 {
        None
    } else {
        Some(Duration::from_millis(timeout as _))
    };
    epoll_wait_duration(poller, events, len, timeout, alertable)
}

/// Wait for events on the wepoll instance.
///
/// `alterable` indicates whether the wait is alertable.
///
/// # Safety
///
/// Given pointer should be valid.
#[no_mangle]
pub unsafe extern "C" fn epoll_pwait2(
    poller: *const Poller,
    events: *mut Event,
    len: c_int,
    timeout: *const libc::timespec,
    alertable: bool,
) -> c_int {
    if timeout.is_null() {
        epoll_wait_duration(poller, events, len, None, alertable)
    } else if timeout.is_aligned() {
        let timeout = unsafe { &*timeout };
        let timeout = Some(
            Duration::from_nanos(timeout.tv_nsec as _) + Duration::from_secs(timeout.tv_sec as _),
        );
        epoll_wait_duration(poller, events, len, timeout, alertable)
    } else {
        unsafe { SetLastError(ERROR_INVALID_PARAMETER) };
        -1
    }
}

fn is_socket(handle: HANDLE) -> bool {
    let res = unsafe { WSAGetQOSByName(handle as _, null(), null_mut()) };
    res != 0 || (unsafe { WSAGetLastError() } != WSAENOTSOCK)
}

fn interest_mode(event: *const Event) -> Result<(Event, PollMode)> {
    let event = check_pointer(event)?;
    let events = event.events as c_int;
    let mode = match (((events & EPOLLET) != 0), ((events & EPOLLONESHOT) != 0)) {
        (false, false) => PollMode::Level,
        (false, true) => PollMode::Oneshot,
        (true, false) => PollMode::Edge,
        (true, true) => PollMode::EdgeOneshot,
    };
    Ok((*event, mode))
}

fn epoll_ctl_socket(
    poller: &mut Poller,
    op: c_int,
    socket: SOCKET,
    event: *const Event,
) -> Result<()> {
    match op {
        EPOLL_CTL_ADD => {
            let (interest, mode) = interest_mode(event)?;
            poller.add(socket, interest, mode)?
        }
        EPOLL_CTL_MOD => {
            let (interest, mode) = interest_mode(event)?;
            poller.modify(socket, interest, mode)?
        }
        EPOLL_CTL_DEL => poller.delete(socket)?,
        _ => return Err(Error(ERROR_INVALID_PARAMETER)),
    }
    Ok(())
}

fn epoll_ctl_waitable(
    poller: &mut Poller,
    op: c_int,
    handle: HANDLE,
    event: *const Event,
) -> Result<()> {
    match op {
        EPOLL_CTL_ADD => poller.add_waitable(handle, *check_pointer(event)?)?,
        EPOLL_CTL_MOD => poller.modify_waitable(handle, *check_pointer(event)?)?,
        EPOLL_CTL_DEL => poller.delete_waitable(handle)?,
        _ => return Err(Error(ERROR_INVALID_PARAMETER)),
    }
    Ok(())
}

/// Add, modify, or remove entries in the wepoll interest list.
///
/// # Safety
///
/// Given pointer should be valid.
#[no_mangle]
pub unsafe extern "C" fn epoll_ctl(
    poller: *mut Poller,
    op: c_int,
    handle: HANDLE,
    event: *mut Event,
) -> c_int {
    io_result_ret(
        try {
            let poller = check_pointer_mut(poller)?;
            if is_socket(handle) {
                epoll_ctl_socket(poller, op, handle as _, event)?;
            } else {
                epoll_ctl_waitable(poller, op, handle, event)?;
            }
            0
        },
    )
}

#[cfg(all(test, feature = "std"))]
mod test {
    use std::{
        fs::File,
        os::windows::io::{AsRawHandle, AsRawSocket, FromRawHandle, OwnedHandle},
        ptr::null,
    };

    use socket2::{Domain, Protocol, Socket, Type};
    use windows_sys::Win32::System::Threading::CreateEventA;

    use super::is_socket;

    #[test]
    fn handles() {
        let s = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
        assert!(is_socket(s.as_raw_socket() as _));

        let f = File::open("Cargo.toml").unwrap();
        assert!(!is_socket(f.as_raw_handle() as _));

        let e = unsafe { CreateEventA(null(), 0, 0, null()) };
        assert_ne!(e, 0);
        let e = unsafe { OwnedHandle::from_raw_handle(e as _) };
        assert!(!is_socket(e.as_raw_handle() as _));
    }
}
