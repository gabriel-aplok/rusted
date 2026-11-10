fn main() {
    let mut buffer = itoa::Buffer::new();
    let printed = buffer.format(42u64);
    println!("{printed} {}", libc::RUSTED_FORK_VALUE);
}
