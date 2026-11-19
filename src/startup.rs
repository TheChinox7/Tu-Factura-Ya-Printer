use std::path::PathBuf;

pub fn available() -> bool {
    cfg!(target_os = "macos") || !crate::development_mode()
}

pub fn is_enabled() -> Result<bool, String> {
    if available() {
        platform::is_enabled()
    } else {
        Ok(false)
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if available() {
        platform::set_enabled(enabled)
    } else {
        Err("El inicio automático está disponible en la aplicación instalada.".into())
    }
}

fn executable() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|_| "No se pudo localizar la aplicación instalada.".into())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::process::Command;

    fn label() -> &'static str {
        if crate::development_mode() {
            "ec.tufacturaya.printer.dev"
        } else {
            "ec.tufacturaya.printer"
        }
    }

    pub fn is_enabled() -> Result<bool, String> {
        let label = label();
        match fs::metadata(agent_path(label)?) {
            Ok(info) if info.is_file() => is_registered(&format!("{}/{label}", login_domain()?)),
            Ok(_) => Ok(false),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(_) => Err("No se pudo comprobar el archivo de inicio automático de macOS.".into()),
        }
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let label = label();
        let agent = agent_path(label)?;
        let domain = login_domain()?;
        let service = format!("{domain}/{label}");
        let registered = is_registered(&service)?;
        if !enabled {
            if registered {
                unload(&service)?;
            }
            if let Err(error) = remove_agent(&agent) {
                if registered && load(&domain, &service, &agent).is_err() {
                    return Err(
                        "No se pudo eliminar ni restaurar el inicio automático de macOS.".into(),
                    );
                }
                return Err(error);
            }
            return Ok(());
        }

        let previous = match fs::read(&agent) {
            Ok(_) if registered => return Ok(()),
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => {
                return Err("No se pudo leer el archivo de inicio automático de macOS.".into());
            }
        };
        let document = agent_document(label)?;
        let parent = agent
            .parent()
            .ok_or("La ruta de LaunchAgents es inválida.")?;
        fs::create_dir_all(parent).map_err(|_| "No se pudo crear la carpeta LaunchAgents.")?;
        write_agent(&agent, document.as_bytes())?;
        if !registered && let Err(error) = load(&domain, &service, &agent) {
            let restored = match previous {
                Some(bytes) => write_agent(&agent, &bytes),
                None => remove_agent(&agent),
            };
            restored.map_err(
                |_| "No se pudo habilitar el inicio automático de macOS ni restaurar su archivo.",
            )?;
            return Err(error);
        }
        Ok(())
    }

    fn agent_document(label: &str) -> Result<String, String> {
        let executable = executable()?;
        let bundle = executable
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            .and_then(|path| path.to_str())
            .ok_or("Abre el archivo .app instalado para activar el inicio automático.")?;
        let path = bundle
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let development_arg = if crate::development_mode() {
            "<string>--dev</string>"
        } else {
            ""
        };
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array>
    <string>/usr/bin/open</string><string>-g</string><string>-j</string>
    <string>-a</string><string>{path}</string><string>--args</string>{development_arg}<string>--background</string>
  </array>
  <key>RunAtLoad</key><true/>
</dict></plist>
"#
        ))
    }

    fn write_agent(agent: &Path, bytes: &[u8]) -> Result<(), String> {
        let temp = agent.with_extension("plist.tmp");
        fs::write(&temp, bytes)
            .and_then(|()| fs::rename(&temp, agent))
            .map_err(|_| "No se pudo guardar el inicio automático de macOS.".into())
    }

    fn remove_agent(agent: &Path) -> Result<(), String> {
        match fs::remove_file(agent) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err("No se pudo eliminar el inicio automático de macOS.".into()),
        }
    }

    fn agent_path(label: &str) -> Result<PathBuf, String> {
        Ok(directories::BaseDirs::new()
            .ok_or("No se encontró la carpeta del usuario.")?
            .home_dir()
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist")))
    }

    fn login_domain() -> Result<String, String> {
        let output = Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .map_err(|_| "No se pudo identificar la sesión de macOS.")?;
        let uid = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .map_err(|_| "No se pudo identificar la sesión de macOS.")?;
        Ok(format!("gui/{uid}"))
    }

    fn is_registered(service: &str) -> Result<bool, String> {
        let output = Command::new("/bin/launchctl")
            .args(["print", service])
            .output()
            .map_err(|_| "No se pudo comprobar el inicio automático de macOS.")?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(113) => Ok(false),
            _ => Err("No se pudo comprobar el inicio automático de macOS.".into()),
        }
    }

    fn load(domain: &str, service: &str, agent: &Path) -> Result<(), String> {
        let output = Command::new("/bin/launchctl")
            .args(["bootstrap", domain])
            .arg(agent)
            .output()
            .map_err(|_| "macOS no habilitó el inicio automático en esta sesión.")?;
        if output.status.success() || is_registered(service)? {
            Ok(())
        } else {
            Err("macOS no habilitó el inicio automático en esta sesión.".into())
        }
    }

    fn unload(service: &str) -> Result<(), String> {
        let output = Command::new("/bin/launchctl")
            .args(["bootout", service])
            .output()
            .map_err(|_| "macOS no deshabilitó el inicio automático en esta sesión.")?;
        if output.status.success() || !is_registered(service)? {
            Ok(())
        } else {
            Err("macOS no deshabilitó el inicio automático en esta sesión.".into())
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Tu Factura Ya Printer";

    pub fn is_enabled() -> Result<bool, String> {
        reg()
            .args(["query", KEY, "/v", NAME])
            .output()
            .map(|output| output.status.success())
            .map_err(|_| "No se pudo comprobar el inicio automático de Windows.".into())
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let mut command = reg();
        if enabled {
            let executable = executable()?;
            let executable = executable
                .to_str()
                .ok_or("La ruta de la aplicación es inválida.")?;
            command.args([
                "add",
                KEY,
                "/v",
                NAME,
                "/t",
                "REG_SZ",
                "/d",
                &format!(r#""{executable}" --background"#),
                "/f",
            ]);
        } else if is_enabled()? {
            command.args(["delete", KEY, "/v", NAME, "/f"]);
        } else {
            return Ok(());
        }
        match command.output() {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err("Windows no pudo cambiar el inicio automático.".into()),
        }
    }

    fn reg() -> Command {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("reg.exe");
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;

    pub fn is_enabled() -> Result<bool, String> {
        Ok(entry_path()?.is_file())
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let entry = entry_path()?;
        if !enabled {
            return match fs::remove_file(&entry) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err("No se pudo eliminar el inicio automático.".into()),
            };
        }

        let executable = executable()?;
        let program = executable
            .to_str()
            .filter(|path| !path.contains(['\n', '\r']))
            .ok_or("La ruta de la aplicación es inválida.")?;
        let program = program
            .replace('\\', "\\\\\\\\")
            .replace('"', "\\\\\"")
            .replace('`', "\\\\`")
            .replace('$', "\\\\$")
            .replace('%', "%%");
        let definition = format!(
            "[Desktop Entry]\nType=Application\nName=Tu Factura Ya Printer\nExec=\"{program}\" --background\nTerminal=false\n"
        );
        let parent = entry
            .parent()
            .ok_or("La ruta de inicio automático es inválida.")?;
        fs::create_dir_all(parent)
            .map_err(|_| "No se pudo crear la carpeta de inicio automático.")?;
        let temp = entry.with_extension("desktop.tmp");
        fs::write(&temp, definition)
            .and_then(|()| fs::rename(&temp, &entry))
            .map_err(|_| "No se pudo guardar el inicio automático.".into())
    }

    fn entry_path() -> Result<PathBuf, String> {
        Ok(directories::BaseDirs::new()
            .ok_or("No se encontró la carpeta del usuario.")?
            .config_dir()
            .join("autostart/ec.tufacturaya.printer.desktop"))
    }
}
