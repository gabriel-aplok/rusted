fn main() {
    println!(
        "alpha_on={} beta_loud={} unix={}",
        alpha_on(),
        beta_loud(),
        cfg!(feature = "unix")
    );
}

#[cfg(feature = "basic")]
fn alpha_on() -> bool {
    alpha::on()
}

#[cfg(not(feature = "basic"))]
fn alpha_on() -> bool {
    false
}

#[cfg(feature = "full")]
fn beta_loud() -> bool {
    beta::loud()
}

#[cfg(not(feature = "full"))]
fn beta_loud() -> bool {
    false
}
