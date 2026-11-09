fn main() {
    let value = std::env::var("DEP_BUILDDEP_VALUE").unwrap_or_else(|_| "missing".to_string());
    println!("cargo:rustc-env=DEP_VALUE={value}");
}
