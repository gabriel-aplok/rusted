use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(crate) struct Lock {
    file: File,
}

impl Lock {
    pub(crate) fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        if try_lock(&file)? {
            Ok(Some(Self { file }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn acquire(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        lock_blocking(&file)?;
        Ok(Self { file })
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::os::raw::c_int;
    use std::os::unix::io::AsRawFd;
    use std::fs::File;

    const LOCK_EX: c_int = 2;
    const LOCK_NB: c_int = 4;
    const LOCK_UN: c_int = 8;

    extern "C" {
        fn flock(fd: c_int, operation: c_int) -> c_int;
    }

    pub(super) fn try_lock(file: &File) -> io::Result<bool> {
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn lock_blocking(file: &File) -> io::Result<()> {
        loop {
            if unsafe { flock(file.as_raw_fd(), LOCK_EX) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub(super) fn unlock(file: &File) {
        unsafe {
            flock(file.as_raw_fd(), LOCK_UN);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::mem::zeroed;
    use std::os::raw::c_void;
    use std::os::windows::io::AsRawHandle;

    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;
    const MAX_LOCK_RANGE: u32 = u32::MAX;
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            count_low: u32,
            count_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            file: *mut c_void,
            reserved: u32,
            count_low: u32,
            count_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    fn lock_range(file: &File, flags: u32) -> io::Result<bool> {
        let overlapped: Overlapped = unsafe { zeroed() };
        let result = unsafe {
            LockFileEx(
                file.as_raw_handle() as *mut c_void,
                flags,
                0,
                MAX_LOCK_RANGE,
                0,
                &overlapped as *const Overlapped as *mut Overlapped,
            )
        };
        if result != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub(super) fn try_lock(file: &File) -> io::Result<bool> {
        lock_range(
            file,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
        )
    }

    pub(super) fn lock_blocking(file: &File) -> io::Result<()> {
        loop {
            if lock_range(file, LOCKFILE_EXCLUSIVE_LOCK)? {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub(super) fn unlock(file: &File) {
        let overlapped: Overlapped = unsafe { zeroed() };
        unsafe {
            UnlockFileEx(
                file.as_raw_handle() as *mut c_void,
                0,
                MAX_LOCK_RANGE,
                0,
                &overlapped as *const Overlapped as *mut Overlapped,
            );
        }
    }
}

use platform::{lock_blocking, try_lock, unlock};

impl Drop for Lock {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::Lock;

    #[test]
    fn lock_is_exclusive_across_threads() {
        let directory = std::env::temp_dir().join(format!(
            "rusted-lock-{}-{}",
            std::process::id(),
            "exclusive"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("unit.lock");

        let lock = Lock::acquire(&path).unwrap();
        let (sender, receiver) = mpsc::channel();
        let thread_path = path.clone();
        let handle = std::thread::spawn(move || {
            let second = Lock::acquire(&thread_path).unwrap();
            sender.send(()).unwrap();
            drop(second);
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(lock);
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        let _ = std::fs::remove_dir_all(&directory);
    }
}
