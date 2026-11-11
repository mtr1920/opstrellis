// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Answered before Tauri starts, so it works headless and doesn't reach a running instance.
    if std::env::args().skip(1).any(|a| a == "--version" || a == "-V") {
        println!("opstrellis {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // `mcp`, the agent-access commands and --help also run without starting the app.
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if let Some(code) = opstrellis_lib::cli::dispatch(&args) {
        std::process::exit(code);
    }
    opstrellis_lib::run()
}
