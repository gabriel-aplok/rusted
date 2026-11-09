use std::fs::{File, OpenOptions};
use std::io;
use std::os::raw::c_int;
use std::os::unix::io::AsRawFd;
use std::path::Path;

const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const LOCK_UN: c_int = 8;

extern "C" {
    fn flock(fd: c_int, operation: c_int) -> c_int;
}

pub(crate) struct Lock {
    file: File,
}

impl Lock {
    pub(crate) fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if result == 0 {
            return Ok(Some(Self { file }));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }

    pub(crate) fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;

        loop {
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
            if result == 0 {
                return Ok(Self { file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            flock(self.file.as_raw_fd(), LOCK_UN);
        }
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
