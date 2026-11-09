fn main() {
    println!(
        "{} {} {} {}",
        builddep::value(),
        builddep::name(),
        linkuser::dep_value(),
        builddep::has_generated()
    );
}
