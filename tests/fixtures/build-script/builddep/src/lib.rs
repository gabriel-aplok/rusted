include!(concat!(env!("OUT_DIR"), "/generated.rs"));

pub fn value() -> u32 {
    GENERATED
}

pub fn name() -> &'static str {
    env!("BUILD_NAME")
}

#[cfg(has_generated)]
pub fn has_generated() -> bool {
    true
}
