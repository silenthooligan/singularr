//! The `singularrd` binary: the server, and the `doctor` subcommand.
//!
//!   singularrd           start the server
//!   singularrd doctor    check the database, the migrations, the policy file and every engine
//!   singularrd version   print the version
#![forbid(unsafe_code)]
#![recursion_limit = "512"]

#[cfg(feature = "ssr")]
mod boot;
#[cfg(feature = "ssr")]
mod doctor;

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    let command = std::env::args().nth(1);
    match command.as_deref() {
        None | Some("serve") => match boot::serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("singularrd: {message}");
                ExitCode::FAILURE
            }
        },
        Some("doctor") => doctor::run().await,
        Some("version") | Some("--version") | Some("-V") => {
            println!(
                "singularrd {} (build {})",
                env!("CARGO_PKG_VERSION"),
                env!("SINGULARR_BUILD_ID")
            );
            ExitCode::SUCCESS
        }
        Some("help") | Some("--help") | Some("-h") => {
            println!("singularrd           start the server");
            println!(
                "singularrd doctor    check the database, the migrations, the policy file and every engine"
            );
            println!("singularrd version   print the version");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("singularrd: `{other}` is not a command. Try `singularrd help`.");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // The browser side is the library, started by `hydrate()`.
}
