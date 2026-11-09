$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force target/bootstrap | Out-Null
rustc --edition 2021 --crate-name rusted -C opt-level=0 -C debuginfo=0 -C strip=debuginfo -C codegen-units=16 src/main.rs -o target/bootstrap/rusted.exe
exit $LASTEXITCODE
