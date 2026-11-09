use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let special = env::var_os("CARGO_FEATURE_SPECIAL").is_some();
    let value = if special { 100 } else { 7 };
    fs::write(
        Path::new(&out_dir).join("generated.rs"),
        format!("pub const GENERATED: u32 = {value};\n"),
    )
    .unwrap();

    println!("cargo::rustc-cfg=has_generated");
    println!("cargo:rustc-env=BUILD_NAME={}", env::var("CARGO_PKG_NAME").unwrap());
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-arg=-Wl,--as-needed");
    println!("cargo:rustc-flags=-L native={out_dir}");
    println!("cargo:metadata=VALUE={value}");
    println!("cargo:warning=build-script-ran");
    println!("cargo:rerun-if-changed=trigger.txt");
    println!("cargo:rerun-if-changed=../builddep-aux.txt");
    println!("cargo:rerun-if-env-changed=BUILD_EXTRA");
}
