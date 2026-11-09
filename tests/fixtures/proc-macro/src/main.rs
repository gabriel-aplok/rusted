#[hello_macro::tag]
fn value() -> u32 {
    7
}

fn main() {
    println!("{} {}", value(), hello_macro::hello!());
}
