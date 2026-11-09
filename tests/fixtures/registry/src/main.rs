cfg_if::cfg_if! {
    if #[cfg(unix)] {
        fn platform() -> &'static str {
            "unix"
        }
    } else {
        fn platform() -> &'static str {
            "other"
        }
    }
}

fn main() {
    let _: libc::c_int = 42;
    let mut buffer = itoa::Buffer::new();
    println!(
        "{} {} {}",
        platform(),
        buffer.format(42),
        memchr::memchr(b'c', b"abc").unwrap()
    );
}
