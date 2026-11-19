#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod api;
mod browser_bridge;
mod desktop;
mod printer;
mod settings;
mod startup;
mod worker;

fn development_mode() -> bool {
    cfg!(debug_assertions)
        && (std::env::args_os().any(|arg| arg == "--dev")
            || std::env::var_os("TFY_PRINTER_DEV").is_some_and(|value| value == "1"))
}

fn main() -> eframe::Result<()> {
    if std::env::args_os().any(|arg| arg == "--list-printers") {
        let printers = printer::list().map_err(|error| eframe::Error::AppCreation(error.into()))?;
        if let Some(default) = printers.default {
            println!("Predeterminada\t{default}");
        }
        for name in printers.names {
            println!("{name}");
        }
        return Ok(());
    }
    let background = std::env::args_os().any(|arg| arg == "--background");
    let instance = desktop::Instance::acquire(background)
        .map_err(|error| eframe::Error::AppCreation(error.into()))?;
    match instance {
        Some(instance) => desktop::run(instance, background),
        None => Ok(()),
    }
}
