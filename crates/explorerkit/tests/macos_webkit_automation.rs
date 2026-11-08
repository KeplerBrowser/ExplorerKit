//! Native public-API acceptance. Rerun when the macOS adapter, WRY/WebKit runtime,
//! compatible minidriver, or view/prompt ownership changes. The helper is supplied
//! by the host; ordinary quick tests do not download or build it.
#![allow(deprecated)]
#[cfg(target_os = "macos")]
#[path = "../examples/webkit-automation/app.rs"]
mod app;
fn main() {
    #[cfg(target_os = "macos")]
    {
        if std::env::args().any(|arg| arg == "--native-host") {
            app::run();
            return;
        }
        let Some(helper) = std::env::var_os("EXPLORERKIT_MINIDRIVER") else {
            eprintln!("native automation acceptance skipped: set EXPLORERKIT_MINIDRIVER to a compatible local helper");
            return;
        };
        let client = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/macos_webkit_automation/client.ts");
        for mode in ["drop", "shutdown"] {
            let mut command = std::process::Command::new("bun");
            command
                .arg(&client)
                .arg(std::env::current_exe().unwrap())
                .arg(&helper)
                .env("EXPLORERKIT_AUTOMATION_TEARDOWN", mode);
            if let Some(root) = std::env::var_os("EXPLORERKIT_AUTOMATION_EVIDENCE") {
                command.env(
                    "EXPLORERKIT_AUTOMATION_EVIDENCE",
                    std::path::PathBuf::from(root).join(mode),
                );
            }
            let status = command
                .status()
                .expect("Bun is required for the native standard client");
            assert!(
                status.success(),
                "native public-API automation acceptance failed ({mode})"
            );
        }
    }
}
