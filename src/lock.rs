use core::cell::UnsafeCell;

use lock_api::{GuardSend, RawRwLock};
use windows_sys::Win32::System::Threading::{
    AcquireSRWLockExclusive, AcquireSRWLockShared, ReleaseSRWLockExclusive, ReleaseSRWLockShared,
    SRWLOCK, TryAcquireSRWLockExclusive, TryAcquireSRWLockShared,
};

pub type RwLock<T> = lock_api::RwLock<SRWLock, T>;

pub struct SRWLock(UnsafeCell<SRWLOCK>);

unsafe impl RawRwLock for SRWLock {
    type GuardMarker = GuardSend;

    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = Self(UnsafeCell::new(SRWLOCK { Ptr: 0 as _ }));

    fn lock_shared(&self) {
        unsafe { AcquireSRWLockShared(self.0.get()) }
    }

    fn try_lock_shared(&self) -> bool {
        unsafe { TryAcquireSRWLockShared(self.0.get()) != 0 }
    }

    unsafe fn unlock_shared(&self) {
        unsafe { ReleaseSRWLockShared(self.0.get()) }
    }

    fn lock_exclusive(&self) {
        unsafe { AcquireSRWLockExclusive(self.0.get()) }
    }

    fn try_lock_exclusive(&self) -> bool {
        unsafe { TryAcquireSRWLockExclusive(self.0.get()) != 0 }
    }

    unsafe fn unlock_exclusive(&self) {
        unsafe { ReleaseSRWLockExclusive(self.0.get()) }
    }
}

unsafe impl Send for SRWLock {}
unsafe impl Sync for SRWLock {}
