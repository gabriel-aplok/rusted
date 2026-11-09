fn main() {
    if std::env::args().nth(1).as_deref() == Some("fail") {
        std::process::exit(3);
    }
    println!(
        "answer={} left={} right={}",
        core::answer(),
        left::value(),
        right::value()
    );
}
