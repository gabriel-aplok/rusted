$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force target/tests | Out-Null
rustc --edition 2021 --test src/main.rs -o target/tests/rusted-unit.exe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& .\target\tests\rusted-unit.exe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if (-not (Test-Path target/bootstrap/rusted.exe)) {
    & .\scripts\bootstrap.ps1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
$cacheRoot = Join-Path $env:TEMP "rusted-test-cache-$PID"
$targetRoot = Join-Path $env:TEMP "rusted-test-target-$PID"
$env:RUSTED_CACHE_DIR = $cacheRoot
& .\target\bootstrap\rusted.exe run --manifest-path tests/fixtures/local-workspace/Cargo.toml --target-dir $targetRoot | Tee-Object -Variable runOutput
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if (-not (($runOutput -join "`n") -like "*answer=42 left=11 right=31*")) {
    Write-Error "local-workspace output missing answer=42 left=11 right=31"
}
& .\target\bootstrap\rusted.exe run --manifest-path tests/fixtures/proc-macro/Cargo.toml --target-dir $targetRoot | Tee-Object -Variable macroOutput
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if (-not (($macroOutput -join "`n") -like "*7 9*")) {
    Write-Error "proc-macro output missing 7 9"
}
