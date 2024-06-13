//! FFI of this crate. Imitate epoll(2).

use std::{
    ffi::c_int,
    io,
    mem::MaybeUninit,
    os::windows::io::{RawHandle, RawSocket},
    panic::{catch_unwind, UnwindSafe},
    ptr::{null, null_mut},
    time::Duration,
};

use smallvec::SmallVec;
use windows_sys::Win32::{
    Foundation::{SetLastError, ERROR_INVALID_PARAMETER, ERROR_UNKNOWN_EXCEPTION, HANDLE},
    Networking::WinSock::{WSAGetLastError, WSAGetQOSByName, WSAENOTSOCK},
    System::IO::OVERLAPPED_ENTRY,
};

use crate::{Event, PollMode, Poller};

#[inline]
fn catch_unwind_result<F, R>(f: F) -> io::Result<R>
where
    F: FnOnce() -> io::Result<R> + UnwindSafe,
{
    match catch_unwind(f) {
        Ok(res) => res,
        Err(_) => Err(io::Error::from_raw_os_error(ERROR_UNKNOWN_EXCEPTION as _)),
    }
}

#[inline]
fn io_result_ok<T>(res: io::Result<T>) -> Option<T> {
    match res {
        Ok(value) => Some(value),
        Err(e) => {
            unsafe { SetLastError(e.raw_os_error().unwrap_or(ERROR_UNKNOWN_EXCEPTION as _) as _) };
            None
        }
    }
}

#[inline]
fn io_result_ret(res: io::Result<c_int>) -> c_int {
    io_result_ok(res).unwrap_or(-1)
}

#[inline]
fn check_pointer<'a, T>(ptr: *const T) -> io::Result<&'a T> {
    if ptr.is_aligned() && !ptr.is_null() {
        Ok(unsafe { &*ptr })
    } else {
        Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _))
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
fn epoll_try_create() -> io::Result<Box<Poller>> {
    Ok(Box::new(Poller::new()?))
}

/// Create a new wepoll instance. `size` should be positive.
#[no_mangle]
#[deprecated]
pub extern "C" fn epoll_create(size: c_int) -> Option<Box<Poller>> {
    io_result_ok(catch_unwind_result(|| {
        if size <= 0 {
            Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _))
        } else {
            epoll_try_create()
        }
    }))
}

/// Create a new wepoll instance. `flags` should be zero.
#[no_mangle]
pub extern "C" fn epoll_create1(flags: c_int) -> Option<Box<Poller>> {
    io_result_ok(catch_unwind_result(|| {
        if flags != 0 {
            Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _))
        } else {
            epoll_try_create()
        }
    }))
}

/// Close a wepoll instance.
#[no_mangle]
pub extern "C" fn epoll_close(poller: Option<Box<Poller>>) -> c_int {
    io_result_ret(catch_unwind_result(|| {
        if let Some(poller) = poller {
            drop(poller);
            Ok(0)
        } else {
            Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _))
        }
    }))
}

#[inline(never)]
unsafe fn epoll_wait_duration(
    poller: *const Poller,
    events: *mut Event,
    len: c_int,
    timeout: Option<Duration>,
    alertable: bool,
) -> c_int {
    io_result_ret(catch_unwind_result(|| {
        let poller = check_pointer(poller)?;
        if len != 0 {
            check_pointer(events)?;
        }
        let events: &mut [MaybeUninit<Event>] =
            unsafe { std::slice::from_raw_parts_mut(events.cast(), len as _) };

        let mut entries = SmallVec::<[OVERLAPPED_ENTRY; 256]>::with_capacity(events.len());
        let spare_entries = unsafe {
            std::slice::from_raw_parts_mut(entries.as_mut_ptr().cast(), entries.capacity())
        };
        let len = poller.wait(spare_entries, timeout, alertable)?;
        unsafe { entries.set_len(len) };

        for (ev, entry) in events.iter_mut().zip(entries) {
            ev.write(Event::from(entry));
        }

        Ok(len as _)
    }))
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
    res == 0 || (unsafe { WSAGetLastError() } != WSAENOTSOCK)
}

fn interest_mode(event: *const Event) -> io::Result<(Event, PollMode)> {
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
    poller: &Poller,
    op: c_int,
    socket: RawSocket,
    event: *const Event,
) -> io::Result<()> {
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
        _ => return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _)),
    }
    Ok(())
}

fn epoll_ctl_waitable(
    poller: &Poller,
    op: c_int,
    handle: RawHandle,
    event: *const Event,
) -> io::Result<()> {
    match op {
        EPOLL_CTL_ADD => poller.add_waitable(handle, *check_pointer(event)?)?,
        EPOLL_CTL_MOD => poller.modify_waitable(handle, *check_pointer(event)?)?,
        EPOLL_CTL_DEL => poller.delete_waitable(handle)?,
        _ => return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as _)),
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
    poller: *const Poller,
    op: c_int,
    handle: HANDLE,
    event: *mut Event,
) -> c_int {
    io_result_ret(catch_unwind_result(|| {
        let poller = check_pointer(poller)?;
        if is_socket(handle) {
            epoll_ctl_socket(poller, op, handle as _, event)?;
        } else {
            epoll_ctl_waitable(poller, op, handle as _, event)?;
        }
        Ok(0)
    }))
}
