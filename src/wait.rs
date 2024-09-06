//! Safe wrapper around `NtAssociateWaitCompletionPacket` API series.

use core::{ffi::c_void, ptr::null_mut};

use windows_sys::{
    Wdk::Foundation::OBJECT_ATTRIBUTES,
    Win32::Foundation::{
        RtlNtStatusToDosError, BOOLEAN, GENERIC_READ, GENERIC_WRITE, HANDLE, NTSTATUS,
        STATUS_CANCELLED, STATUS_PENDING, STATUS_SUCCESS,
    },
};

use crate::{Error, OwnedHandle, Result};

#[link(name = "ntdll")]
extern "system" {
    fn NtCreateWaitCompletionPacket(
        WaitCompletionPacketHandle: *mut HANDLE,
        DesiredAccess: u32,
        ObjectAttributes: *mut OBJECT_ATTRIBUTES,
    ) -> NTSTATUS;

    fn NtAssociateWaitCompletionPacket(
        WaitCompletionPacketHandle: HANDLE,
        IoCompletionHandle: HANDLE,
        TargetObjectHandle: HANDLE,
        KeyContext: *mut c_void,
        ApcContext: *mut c_void,
        IoStatus: NTSTATUS,
        IoStatusInformation: usize,
        AlreadySignaled: *mut BOOLEAN,
    ) -> NTSTATUS;

    fn NtCancelWaitCompletionPacket(
        WaitCompletionPacketHandle: HANDLE,
        RemoveSignaledPacket: BOOLEAN,
    ) -> NTSTATUS;
}

/// Wrapper of NT WaitCompletionPacket.
#[derive(Debug)]
pub struct WaitCompletionPacket {
    handle: OwnedHandle,
}

fn check_status(status: NTSTATUS) -> Result<()> {
    if status == STATUS_SUCCESS {
        Ok(())
    } else {
        Err(Error(unsafe { RtlNtStatusToDosError(status) }))
    }
}

impl WaitCompletionPacket {
    pub fn new() -> Result<Self> {
        let mut handle = null_mut();
        check_status(unsafe {
            NtCreateWaitCompletionPacket(&mut handle, GENERIC_READ | GENERIC_WRITE, null_mut())
        })?;
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        Ok(Self { handle })
    }

    /// Associate waitable object to IOCP. The parameter `info` is the
    /// field `dwNumberOfBytesTransferred` in `OVERLAPPED_ENTRY`
    pub fn associate(
        &mut self,
        port: HANDLE,
        event: HANDLE,
        key: usize,
        info: usize,
    ) -> Result<()> {
        check_status(unsafe {
            NtAssociateWaitCompletionPacket(
                self.handle.as_raw_handle(),
                port,
                event,
                key as _,
                null_mut(),
                STATUS_SUCCESS,
                info,
                null_mut(),
            )
        })?;
        Ok(())
    }

    /// Cancels the completion packet. The return value means:
    /// - `Ok(true)`: cancellation is successful.
    /// - `Ok(false)`: cancellation failed, the packet is still in use.
    /// - `Err(e)`: other errors.
    pub fn cancel(&mut self) -> Result<bool> {
        let status = unsafe { NtCancelWaitCompletionPacket(self.handle.as_raw_handle(), 0) };
        match status {
            STATUS_SUCCESS | STATUS_CANCELLED => Ok(true),
            STATUS_PENDING => Ok(false),
            _ => Err(Error(unsafe { RtlNtStatusToDosError(status) })),
        }
    }
}
