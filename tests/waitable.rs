use std::{
    mem::MaybeUninit,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null,
};

use wepoll2::{Event, Poller};
use windows_sys::Win32::System::Threading::{CreateEventA, SetEvent};

#[test]
fn poll_event() {
    let e = unsafe { CreateEventA(null(), 0, 0, null()) };
    assert_ne!(e, 0);
    let e = unsafe { OwnedHandle::from_raw_handle(e as _) };

    let mut poller = Poller::new().unwrap();
    let interest = Event::none(114514).with_readable(true);
    poller
        .add_waitable(e.as_raw_handle() as _, interest)
        .unwrap();

    let res = unsafe { SetEvent(e.as_raw_handle() as _) };
    assert!(res != 0);

    let mut entries = [MaybeUninit::uninit(); 8];
    let len = poller.wait(&mut entries, None, false).unwrap();
    assert_eq!(len, 1);
    let event = unsafe { MaybeUninit::assume_init_ref(&entries[0]) };
    assert_eq!(event.key(), 114514);
    assert!(event.is_readable());

    poller.delete_waitable(e.as_raw_handle() as _).unwrap();
}
