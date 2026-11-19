use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use url::Url;
use zeroize::Zeroizing;

#[derive(Deserialize, Serialize)]
pub struct DeviceIdentity {
    pub id: String,
    pub token: Zeroizing<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Legacy,
    Device,
}

pub enum Credential {
    Legacy(Zeroizing<String>),
    Device(DeviceIdentity),
}

impl Credential {
    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::Legacy(_) => CredentialKind::Legacy,
            Self::Device(_) => CredentialKind::Device,
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Self::Legacy(key) => key,
            Self::Device(device) => &device.token,
        }
    }
}

static REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrintMode {
    #[default]
    System,
    EscPos,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub url: String,
    pub printer: String,
    pub width: u16,
    pub mode: PrintMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: String::new(),
            printer: String::new(),
            width: 80,
            mode: PrintMode::System,
        }
    }
}

fn project_dirs() -> Result<ProjectDirs, String> {
    let application = if crate::development_mode() {
        "Printer Dev"
    } else {
        "Printer"
    };
    ProjectDirs::from("ec", "TuFacturaYa", application)
        .ok_or_else(|| "No se pudo ubicar la carpeta de ajustes de este usuario.".into())
}

fn credential_service() -> &'static str {
    if crate::development_mode() {
        "ec.tufacturaya.printer.dev"
    } else {
        "ec.tufacturaya.printer"
    }
}

pub fn data_dir() -> Result<PathBuf, String> {
    let dir = project_dirs()?.data_local_dir().to_path_buf();
    fs::create_dir_all(&dir).map_err(|_| "No se pudo crear la carpeta de datos del agente.")?;
    Ok(dir)
}

fn config_path() -> Result<PathBuf, String> {
    Ok(project_dirs()?.config_dir().join("settings.json"))
}

pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

pub fn load() -> Result<Settings, String> {
    match fs::read(config_path()?) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "El archivo de ajustes de la impresora es inválido.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(_) => Err("No se pudieron leer los ajustes de la impresora.".into()),
    }
}

pub fn normalized_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(String::new());
    }
    let url =
        Url::parse(input).map_err(|_| "Ingresa la URL completa de Tu Factura Ya, con https://.")?;
    let loopback = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
        || url.host_str() == Some("localhost");
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Usa HTTPS y una URL sin credenciales, parámetros ni fragmentos. HTTP solo se admite en localhost.".into());
    }
    Ok(url.as_str().trim_end_matches('/').into())
}

pub fn validate_key(key: &str) -> Result<(), String> {
    if key.len() < 16 || key.len() > 192 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("La clave de estación no tiene un formato válido.".into());
    }
    Ok(())
}

pub fn save(settings: &Settings, credential: Option<&Credential>) -> Result<(), String> {
    let mut settings = settings.clone();
    settings.url = normalized_url(&settings.url)?;
    if !matches!(settings.width, 58 | 80) {
        return Err("Selecciona un ancho de 58 u 80 mm.".into());
    }
    let secret = if let Some(credential) = credential {
        if settings.url.is_empty() {
            return Err("Ingresa el dominio antes de guardar la clave.".into());
        }
        validate_key(credential.token())?;
        Some(match credential {
            Credential::Legacy(key) => key.clone(),
            Credential::Device(device) => encode_device(device)?,
        })
    } else {
        None
    };
    let path = config_path()?;
    fs::create_dir_all(path.parent().ok_or("Carpeta de ajustes inválida.")?)
        .map_err(|_| "No se pudo crear la carpeta de ajustes.")?;
    let temporary = path.with_extension("tmp");
    let bytes =
        serde_json::to_vec_pretty(&settings).map_err(|_| "No se pudieron preparar los ajustes.")?;
    let result = (|| {
        let mut file =
            fs::File::create(&temporary).map_err(|_| "No se pudieron crear los ajustes.")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "No se pudieron guardar los ajustes.")?;
        drop(file);
        if let Some(secret) = &secret {
            platform::put(&settings.url, secret)?;
        }
        fs::rename(&temporary, &path)
            .map_err(|_| "No se pudieron activar los ajustes guardados.".to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result?;
    REVISION.fetch_add(1, Ordering::Release);
    Ok(())
}

pub fn get_credential(url: &str) -> Result<Option<Credential>, String> {
    if url.is_empty() {
        return Ok(None);
    }
    platform::get(url)?.map(decode_credential).transpose()
}

pub fn credential_kind(url: &str) -> Result<Option<CredentialKind>, String> {
    Ok(get_credential(url)?.map(|value| value.kind()))
}

pub fn clear_key() -> Result<(), String> {
    let url = normalized_url(&load()?.url)?;
    if !url.is_empty() {
        platform::remove(&pending_device_account(&url))?;
        platform::remove(&url)?;
    }
    REVISION.fetch_add(1, Ordering::Release);
    Ok(())
}

pub fn device_for_enrollment(origin: &str) -> Result<DeviceIdentity, String> {
    if let Some(Credential::Device(device)) = get_credential(origin)? {
        return Ok(device);
    }

    let account = pending_device_account(origin);
    if let Some(secret) = platform::get(&account)? {
        return decode_device(&secret);
    }

    let device = new_device_identity()?;
    platform::put(&account, &encode_device(&device)?)?;
    Ok(device)
}

pub fn clear_pending_device(origin: &str) -> Result<(), String> {
    platform::remove(&pending_device_account(origin))
}

fn pending_device_account(origin: &str) -> String {
    format!("{origin}#print-device-registration")
}

fn new_device_identity() -> Result<DeviceIdentity, String> {
    use std::fmt::Write as _;

    let mut random = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *random)
        .map_err(|_| "No se pudo crear una identidad segura para este equipo.")?;
    let mut token = Zeroizing::new(String::with_capacity(69));
    token.push_str("tfyd_");
    for value in random.iter() {
        write!(&mut *token, "{value:02x}").expect("writing to a string cannot fail");
    }
    Ok(DeviceIdentity {
        id: uuid::Uuid::new_v4().to_string(),
        token,
    })
}

fn encode_device(device: &DeviceIdentity) -> Result<Zeroizing<String>, String> {
    serde_json::to_string(device)
        .map(Zeroizing::new)
        .map_err(|_| "No se pudo preparar la credencial del equipo.".into())
}

fn decode_device(secret: &str) -> Result<DeviceIdentity, String> {
    let error = "La identidad guardada es inválida. Vuelve a vincular el equipo.";
    let device: DeviceIdentity = serde_json::from_str(secret).map_err(|_| error)?;
    if uuid::Uuid::try_parse(&device.id).is_err()
        || device.token.strip_prefix("tfyd_").is_none_or(|value| {
            value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(error.into());
    }
    Ok(device)
}

fn decode_credential(secret: Zeroizing<String>) -> Result<Credential, String> {
    if secret.starts_with('{') {
        decode_device(&secret).map(Credential::Device)
    } else {
        validate_key(&secret)?;
        Ok(Credential::Legacy(secret))
    }
}

fn secret_from_bytes(bytes: &mut [u8]) -> Result<Option<Zeroizing<String>>, String> {
    use zeroize::Zeroize;
    let result = std::str::from_utf8(bytes)
        .map(|value| Some(Zeroizing::new(value.to_string())))
        .map_err(|_| "La credencial guardada es inválida. Vuelve a vincular el equipo.".into());
    bytes.zeroize();
    result
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    pub fn get(account: &str) -> Result<Option<Zeroizing<String>>, String> {
        match get_generic_password(credential_service(), account) {
            Ok(mut bytes) => secret_from_bytes(&mut bytes),
            Err(error) if error.code() == -25300 => Ok(None),
            Err(_) => Err("No se pudo leer la clave del Llavero de macOS.".into()),
        }
    }
    pub fn put(account: &str, secret: &str) -> Result<(), String> {
        set_generic_password(credential_service(), account, secret.as_bytes())
            .map_err(|_| "No se pudo guardar la clave en el Llavero de macOS.".into())
    }
    pub fn remove(account: &str) -> Result<(), String> {
        match delete_generic_password(credential_service(), account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(_) => Err("No se pudo retirar la clave del Llavero de macOS.".into()),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{ERROR_NOT_FOUND, GetLastError},
        Security::Credentials::*,
    };

    fn target(account: &str) -> Vec<u16> {
        format!("{}/{account}", credential_service())
            .encode_utf16()
            .chain([0])
            .collect()
    }

    pub fn get(account: &str) -> Result<Option<Zeroizing<String>>, String> {
        let target = target(account);
        let mut credential = std::ptr::null_mut();
        unsafe {
            if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) == 0 {
                return if GetLastError() == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(
                        "No se pudo leer la clave del Administrador de credenciales de Windows."
                            .into(),
                    )
                };
            }
            let mut bytes = if (*credential).CredentialBlobSize == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(
                    (*credential).CredentialBlob,
                    (*credential).CredentialBlobSize as usize,
                )
                .to_vec()
            };
            CredFree(credential.cast());
            secret_from_bytes(&mut bytes)
        }
    }

    pub fn put(account: &str, secret: &str) -> Result<(), String> {
        let mut target = target(account);
        let mut username: Vec<u16> = credential_service().encode_utf16().chain([0]).collect();
        let mut bytes = Zeroizing::new(secret.as_bytes().to_vec());
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: bytes.len() as u32,
            CredentialBlob: bytes.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: username.as_mut_ptr(),
            ..Default::default()
        };
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(
                "No se pudo guardar la clave en el Administrador de credenciales de Windows."
                    .into(),
            );
        }
        Ok(())
    }

    pub fn remove(account: &str) -> Result<(), String> {
        let target = target(account);
        if unsafe {
            CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) == 0
                && GetLastError() != ERROR_NOT_FOUND
        } {
            return Err(
                "No se pudo retirar la clave del Administrador de credenciales de Windows.".into(),
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::process::{Command, Stdio};

    const ERROR: &str = "El almacén de secretos de Linux no está disponible. Comprueba Secret Service y secret-tool.";

    pub fn get(account: &str) -> Result<Option<Zeroizing<String>>, String> {
        let result = Command::new("secret-tool")
            .args([
                "lookup",
                "service",
                credential_service(),
                "account",
                account,
            ])
            .output()
            .map_err(|_| ERROR)?;
        if !result.status.success() {
            return if result.status.code() == Some(1) && result.stderr.is_empty() {
                Ok(None)
            } else {
                Err(ERROR.into())
            };
        }
        let mut bytes = result.stdout;
        if bytes.ends_with(b"\n") {
            bytes.pop();
        }
        secret_from_bytes(&mut bytes)
    }

    pub fn put(account: &str, secret: &str) -> Result<(), String> {
        let mut command = Command::new("secret-tool")
            .args([
                "store",
                "--label=Tu Factura Ya Printer",
                "service",
                credential_service(),
                "account",
                account,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ERROR)?;
        let write = command
            .stdin
            .take()
            .ok_or(ERROR)?
            .write_all(secret.as_bytes());
        let status = command.wait().map_err(|_| ERROR)?;
        if write.is_err() || !status.success() {
            return Err(ERROR.into());
        }
        Ok(())
    }

    pub fn remove(account: &str) -> Result<(), String> {
        if get(account)?.is_none() {
            return Ok(());
        }
        let status = Command::new("secret-tool")
            .args(["clear", "service", credential_service(), "account", account])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| ERROR)?;
        if !status.success() {
            return Err(ERROR.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_the_domain_without_weakening_tls() {
        assert_eq!(
            normalized_url(" https://POS.example.com/ ").unwrap(),
            "https://pos.example.com"
        );
        assert_eq!(
            normalized_url("http://127.0.0.1:8000").unwrap(),
            "http://127.0.0.1:8000"
        );
        assert!(normalized_url("http://localhost.example.com").is_err());
        assert!(normalized_url("https://user:pass@pos.example.com").is_err());
        assert!(normalized_url("https://pos.example.com/?key=secret").is_err());
    }

    #[test]
    fn serialized_settings_do_not_include_a_key() {
        assert!(
            !serde_json::to_string(&Settings::default())
                .unwrap()
                .contains("key")
        );
        assert!(validate_key("01234567890123456\n").is_err());
    }

    #[test]
    fn preserves_device_identity_when_reading_native_store_entries() {
        let identity = new_device_identity().unwrap();
        let other = new_device_identity().unwrap();
        assert_ne!(identity.id, other.id);
        assert!(identity.token.as_str() != other.token.as_str());

        let restored = decode_credential(encode_device(&identity).unwrap()).unwrap();
        let Credential::Device(restored) = restored else {
            panic!("identity was decoded as a legacy credential")
        };
        assert_eq!(restored.id, identity.id);
        assert!(restored.token.as_str() == identity.token.as_str());
        assert!(decode_device(r#"{"id":"not-a-uuid","token":"not-a-device-key"}"#).is_err());
        let legacy = decode_credential(Zeroizing::new("tfyp_legacy_example".into())).unwrap();
        assert!(matches!(legacy.kind(), CredentialKind::Legacy));
        assert_ne!(
            pending_device_account("https://pos.one"),
            pending_device_account("https://pos.two")
        );
    }
}
