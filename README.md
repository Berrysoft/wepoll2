# wepoll2

This is a Rust crate inspired by the famous `wepoll` library. It provides similar FFI, but not ABI-compatible.

## Implementation details

This library is implemented in Rust, which may panic in some rare cases. It is still on the plan to reduce the panics.
The crate is `no-std` and `panic-abort` by default to reduce the binary size.

Unlike `wepoll`, [`ProcessSocketNotifications`](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-processsocketnotifications) is used in this library. It behaves a little different from `epoll` in Linux.

`wepoll2` supports event objects with `NtAssociateWaitCompletionPacket` API series. No thread pool involved, and one-shot trigger only.

## Extensions
`epoll_pwait` and `epoll_pwait2` is implemented for alertable waiting and `timespec` support.

## Limitations

* `ProcessSocketNotifications` is a very new API.
* The edge trigger behaves a little different.
* One socket could be only associated to one IOCP.
* Not all `EPOLL*` flags are supported.

## Bugs

* Cannot fully work on i686 target. Still investigating.
* `epoll_wait` series API may wait for a shorter time than specified. Maybe bugs of Windows API.
