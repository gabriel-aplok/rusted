pub fn value() -> u32 {
    if cfg!(feature = "normal") { 1 } else { 0 }
}
