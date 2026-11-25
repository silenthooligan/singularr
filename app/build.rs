use std::time::{SystemTime, UNIX_EPOCH};

// SINGULARR_BUILD_ID names the build that assessed a copy. A copy stored by an
// older build is parsed and scored again at startup, because the parser may
// have changed between builds.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=migrations");
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=SINGULARR_BUILD_ID={secs}");
}
