use crate::{
    printer::Receipt,
    settings::{self, Credential, DeviceIdentity, Settings},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use ureq::{
    Body,
    http::{HeaderValue, Response},
    tls::{RootCerts, TlsConfig},
};
use zeroize::Zeroizing;

const PREFIX: &str = "/api/printing/v1";

#[derive(Clone)]
pub struct Client {
    origin: String,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct Connection {
    station: Station,
}

#[derive(Deserialize)]
struct Registration {
    device: Station,
}

#[derive(Deserialize)]
pub struct Job {
    pub id: String,
    pub attempt_id: String,
    pub receipt: JobReceipt,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum JobReceipt {
    Pdf { pdf_base64: String },
    Text(Receipt),
}

#[derive(Deserialize)]
struct Claim {
    job: Job,
}

#[derive(Clone, Serialize)]
pub struct Report {
    pub attempt_id: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Error {
    pub status: Option<u16>,
    pub message: String,
}

impl Error {
    fn local(message: &str) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }
}

impl Client {
    pub fn new(url: &str) -> Result<Self, String> {
        let origin = settings::normalized_url(url)?;
        if origin.is_empty() {
            return Err("Configura el dominio de Tu Factura Ya.".into());
        }
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(12)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            );
        let config = if origin.starts_with("http://") {
            config.proxy(None)
        } else {
            config
        };
        let agent = config.build().new_agent();
        Ok(Self { origin, agent })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn connection(&self, secret: &str) -> Result<Station, Error> {
        let (header, value) = authentication(secret)?;
        let response = self
            .agent
            .get(self.endpoint("/connection"))
            .header("Accept", "application/json")
            .header(header, value)
            .call()
            .map_err(|_| transport_error())?;
        let data: Connection = read_json(require_success(response)?)?;
        verify_id(&data.station.id)?;
        Ok(data.station)
    }

    fn probe_setup_key(&self, setup_key: &str) -> Result<(), Error> {
        let response = self
            .agent
            .get(self.endpoint("/devices"))
            .header("Accept", "application/json")
            .header("X-API-KEY", credential(setup_key)?)
            .call()
            .map_err(|_| transport_error())?;
        require_success(response)?;
        Ok(())
    }

    fn enroll(&self, setup_key: &str, device: &DeviceIdentity, name: &str) -> Result<(), Error> {
        verify_id(&device.id)?;
        let response = self
            .agent
            .post(self.endpoint("/devices"))
            .header("Accept", "application/json")
            .header("X-API-KEY", credential(setup_key)?)
            .header("Authorization", bearer(&device.token)?)
            .send_json(serde_json::json!({"id": device.id, "name": name}))
            .map_err(|_| transport_error())?;
        let registration: Registration = read_json(require_success(response).map_err(|mut error| {
            if error.status == Some(409) {
                error.message = "La identidad del equipo ya no está disponible. Olvida la vinculación y vuelve a configurar el equipo.".into();
            }
            error
        })?)?;
        if registration.device.id != device.id {
            return Err(Error::local(
                "El servidor respondió con otro identificador de equipo.",
            ));
        }
        Ok(())
    }

    fn approve_browser_link(&self, secret: &str, challenge: &str) -> Result<(), Error> {
        let result = self.post(
            "/devices/browser-link",
            secret,
            &serde_json::json!({"challenge": challenge}),
        )?;
        if result.status().as_u16() != 204 {
            return Err(Error::local(
                "Tu Factura Ya no confirmó la vinculación del navegador.",
            ));
        }
        Ok(())
    }

    pub fn claim(&self, secret: &str) -> Result<Option<Job>, Error> {
        let response = self.post("/jobs/claim", secret, &serde_json::json!({}))?;
        if response.status().as_u16() == 204 {
            return Ok(None);
        }
        let claim: Claim = read_json(response)?;
        verify_id(&claim.job.id)?;
        verify_id(&claim.job.attempt_id)?;
        Ok(Some(claim.job))
    }

    pub fn start(&self, secret: &str, job_id: &str, attempt_id: &str) -> Result<(), Error> {
        verify_id(job_id)?;
        self.post(
            &format!("/jobs/{job_id}/start"),
            secret,
            &serde_json::json!({"attempt_id": attempt_id}),
        )?;
        Ok(())
    }

    pub fn report(&self, secret: &str, job_id: &str, report: &Report) -> Result<(), Error> {
        verify_id(job_id)?;
        self.post(&format!("/jobs/{job_id}/result"), secret, report)?;
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{PREFIX}{path}", self.origin)
    }

    fn post(
        &self,
        path: &str,
        secret: &str,
        data: &impl Serialize,
    ) -> Result<Response<Body>, Error> {
        let (header, value) = authentication(secret)?;
        let request = self
            .agent
            .post(self.endpoint(path))
            .header("Accept", "application/json")
            .header(header, value);
        let request = if path == "/jobs/claim" {
            request.header("X-TFY-Ticket-Format", "pdf")
        } else {
            request
        };
        let response = request.send_json(data).map_err(|_| transport_error())?;
        require_success(response)
    }
}

fn authentication(secret: &str) -> Result<(&'static str, HeaderValue), Error> {
    if secret.starts_with("tfyd_") {
        Ok(("Authorization", bearer(secret)?))
    } else {
        Ok(("X-API-KEY", credential(secret)?))
    }
}

fn bearer(secret: &str) -> Result<HeaderValue, Error> {
    settings::validate_key(secret).map_err(|message| Error {
        status: None,
        message,
    })?;
    let value = Zeroizing::new(format!("Bearer {secret}"));
    let mut header = HeaderValue::from_str(&value)
        .map_err(|_| Error::local("La credencial del equipo tiene un formato inválido."))?;
    header.set_sensitive(true);
    Ok(header)
}

fn credential(secret: &str) -> Result<HeaderValue, Error> {
    settings::validate_key(secret).map_err(|message| Error {
        status: None,
        message,
    })?;
    let mut value = HeaderValue::from_str(secret)
        .map_err(|_| Error::local("La clave de estación no tiene un formato válido."))?;
    value.set_sensitive(true);
    Ok(value)
}

fn transport_error() -> Error {
    Error::local(
        "No se pudo conectar con Tu Factura Ya. Comprueba la red, el dominio y su certificado HTTPS.",
    )
}

fn read_json<T: DeserializeOwned>(mut response: Response<Body>) -> Result<T, Error> {
    response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_json::<T>()
        .map_err(|_| Error::local("El servidor devolvió un formato de impresión inesperado."))
}

fn require_success(response: Response<Body>) -> Result<Response<Body>, Error> {
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(response);
    }
    let message = match status {
        301..=399 => "El servidor redirigió la solicitud. Configura la URL final de Tu Factura Ya.",
        401 | 403 => "La clave o la vinculación del equipo fue rechazada o está desactivada.",
        404 => "La ruta de impresión no está disponible en este servidor.",
        409 => "El intento de impresión ya venció o cambió en el servidor.",
        422 => "El servidor rechazó el formato del trabajo de impresión.",
        429 => "El servidor solicitó disminuir la frecuencia de consultas.",
        _ => "Tu Factura Ya devolvió un error en el servicio de impresión.",
    };
    Err(Error {
        status: Some(status),
        message: message.into(),
    })
}

fn verify_id(id: &str) -> Result<(), Error> {
    if id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Ok(());
    }
    Err(Error::local(
        "El servidor devolvió un identificador de impresión inválido.",
    ))
}

pub fn check(settings: &Settings, secret: Option<&str>) -> Result<String, String> {
    let client = Client::new(&settings.url)?;
    if let Some(key) = secret {
        if !key.starts_with("tfyp_") {
            reject_device_token(key)?;
            return client
                .probe_setup_key(key)
                .map(|()| {
                    "Clave del sistema validada. Guarda la configuración para registrar el equipo."
                        .into()
                })
                .map_err(|error| error.message);
        }
        return check_connection(&client, key);
    }

    let stored = settings::get_credential(client.origin())?
        .ok_or("Ingresa la clave del sistema para probar la conexión.")?;
    check_connection(&client, stored.token())
}

fn check_connection(client: &Client, key: &str) -> Result<String, String> {
    client
        .connection(key)
        .map(|station| format!("Conectado · {}", station.name))
        .map_err(|error| error.message)
}

pub fn save_settings(settings: &Settings, secret: Option<&str>) -> Result<(), String> {
    let Some(key) = secret else {
        return settings::save(settings, None);
    };
    if key.starts_with("tfyp_") {
        return settings::save(
            settings,
            Some(&Credential::Legacy(Zeroizing::new(key.into()))),
        );
    }
    reject_device_token(key)?;

    let client = Client::new(&settings.url)?;
    client.probe_setup_key(key).map_err(|error| error.message)?;
    let device = settings::device_for_enrollment(client.origin())?;
    client
        .enroll(key, &device, &device_name())
        .map_err(|error| error.message)?;
    settings::save(settings, Some(&Credential::Device(device)))?;
    let _ = settings::clear_pending_device(client.origin());
    Ok(())
}

pub fn approve_browser_link(config: &Settings, challenge: &str) -> Result<(), Error> {
    let client = Client::new(&config.url).map_err(|message| Error::local(&message))?;
    let credential =
        settings::get_credential(client.origin()).map_err(|message| Error::local(&message))?;
    let Some(Credential::Device(device)) = credential else {
        return Err(Error {
            status: Some(409),
            message: "Guarda una clave de Administración → Sistema → API Keys en Printer.".into(),
        });
    };
    client.approve_browser_link(&device.token, challenge)
}

fn reject_device_token(key: &str) -> Result<(), String> {
    if key.starts_with("tfyd_") {
        return Err("Ingresa la clave del sistema creada en Administración → Sistema.".into());
    }
    Ok(())
}

fn device_name() -> String {
    #[cfg(windows)]
    let hostname = std::env::var("COMPUTERNAME").ok();
    #[cfg(unix)]
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());

    hostname
        .as_deref()
        .and_then(|name| name.trim().split('.').next())
        .map(|name| {
            name.chars()
                .filter(|character| !character.is_control())
                .take(100)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Tu Factura Ya Printer".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn only_accepts_a_uuid_from_the_server() {
        assert!(verify_id("92606b59-245a-4103-96a4-ea9827b634ff").is_ok());
        assert!(verify_id("../adminaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
    }

    #[test]
    fn credentials_are_redacted_in_http_header_debug() {
        let secret = "private-fake-token-123456789";
        assert!(!format!("{:?}", credential(secret).unwrap()).contains(secret));
        let device = "tfyd_test_only_123456789";
        let (name, value) = authentication(device).unwrap();
        assert_eq!(name, "Authorization");
        assert!(!format!("{value:?}").contains(device));
    }

    #[test]
    fn exchanges_jobs_with_the_versioned_api_and_never_follows_redirects() {
        const SECRET: &str = "tfyp_test_only_123456789";
        const ID: &str = "92606b59-245a-4103-96a4-ea9827b634ff";
        const ATTEMPT: &str = "8da10da2-6d9e-41ac-b7c8-b2f92b2de9cb";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let requests = [
                ("GET /api/printing/v1/connection".into(), "200 OK", serde_json::json!({"station": {"id": ID, "name": "Caja prueba"}}).to_string()),
                ("POST /api/printing/v1/jobs/claim".into(), "200 OK", serde_json::json!({"job": {
                    "id": ID, "attempt_id": ATTEMPT, "receipt": {"title": "Nota", "lines": ["TOTAL $10,00"]}
                }}).to_string()),
                (format!("POST /api/printing/v1/jobs/{ID}/start"), "204 No Content", String::new()),
                (format!("POST /api/printing/v1/jobs/{ID}/result"), "204 No Content", String::new()),
                ("POST /api/printing/v1/jobs/claim".into(), "200 OK", serde_json::json!({"job": {
                    "id": ID, "attempt_id": ATTEMPT, "receipt": {"pdf_base64": "JVBERi0xLjc="}
                }}).to_string()),
                ("POST /api/printing/v1/jobs/claim".into(), "302 Found", String::new()),
            ];
            for (request, status, body) in requests {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut read = BufReader::new(&stream);
                let mut line = String::new();
                read.read_line(&mut line).unwrap();
                assert!(line.starts_with(&request));
                let mut length = 0;
                let mut valid_key = false;
                let mut requests_pdf = false;
                loop {
                    line.clear();
                    read.read_line(&mut line).unwrap();
                    assert!(!line.is_empty());
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("x-api-key") {
                            valid_key = value.trim() == SECRET;
                        }
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                        if name.eq_ignore_ascii_case("x-tfy-ticket-format") {
                            requests_pdf = value.trim() == "pdf";
                        }
                    }
                }
                assert!(valid_key);
                assert_eq!(requests_pdf, request.ends_with("/claim"));
                let mut payload = vec![0; length];
                read.read_exact(&mut payload).unwrap();
                if request.ends_with("/result") {
                    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    assert_eq!(value["attempt_id"], ATTEMPT);
                    assert_eq!(value["status"], "sent");
                    assert_eq!(value["os_job_id"], "test-12");
                }
                let location = if status.starts_with("302") {
                    "Location: http://127.0.0.1:1/\r\n"
                } else {
                    ""
                };
                write!(stream, "HTTP/1.1 {status}\r\n{location}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let client = Client::new(&origin).unwrap();
        assert_eq!(client.connection(SECRET).unwrap().name, "Caja prueba");
        let job = client.claim(SECRET).unwrap().unwrap();
        let JobReceipt::Text(receipt) = job.receipt else {
            panic!("expected the legacy receipt");
        };
        assert_eq!(receipt.lines, ["TOTAL $10,00"]);
        client.start(SECRET, &job.id, &job.attempt_id).unwrap();
        client
            .report(
                SECRET,
                &job.id,
                &Report {
                    attempt_id: job.attempt_id,
                    status: "sent",
                    os_job_id: Some("test-12".into()),
                    error: None,
                },
            )
            .unwrap();
        let job = client.claim(SECRET).unwrap().unwrap();
        let JobReceipt::Pdf { pdf_base64 } = job.receipt else {
            panic!("expected a formatted PDF ticket");
        };
        assert_eq!(pdf_base64, "JVBERi0xLjc=");
        match client.claim(SECRET) {
            Err(error) => assert_eq!(error.status, Some(302)),
            Ok(_) => panic!("the agent accepted a redirect"),
        }
        server.join().unwrap();
    }

    #[test]
    fn checks_the_system_key_then_registers_and_pairs_one_authenticated_device() {
        const CENTRAL: &str = "central-key-only-for-test";
        const TOKEN: &str = "tfyd_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const ID: &str = "92606b59-245a-4103-96a4-ea9827b634ff";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let device = serde_json::json!({"id": ID, "name": "Caja local"});
            let requests = [
                (
                    "GET /api/printing/v1/devices",
                    true,
                    false,
                    "204 No Content",
                    String::new(),
                ),
                (
                    "POST /api/printing/v1/devices",
                    true,
                    true,
                    "201 Created",
                    serde_json::json!({"device": device}).to_string(),
                ),
                (
                    "POST /api/printing/v1/devices",
                    true,
                    true,
                    "200 OK",
                    serde_json::json!({"device": device}).to_string(),
                ),
                (
                    "GET /api/printing/v1/connection",
                    false,
                    true,
                    "200 OK",
                    serde_json::json!({"station": device}).to_string(),
                ),
                (
                    "POST /api/printing/v1/devices/browser-link",
                    false,
                    true,
                    "204 No Content",
                    String::new(),
                ),
                (
                    "POST /api/printing/v1/jobs/claim",
                    false,
                    true,
                    "204 No Content",
                    String::new(),
                ),
            ];

            for (path, setup_key, device_token, status, body) in requests {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    request.push_str(&line);
                }
                assert!(request.starts_with(path));
                let headers = request.to_ascii_lowercase();
                assert_eq!(
                    headers.contains(&format!("x-api-key: {CENTRAL}\r\n")),
                    setup_key
                );
                assert_eq!(
                    headers.contains(&format!("authorization: bearer {TOKEN}\r\n")),
                    device_token
                );
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map(|value| value.parse().unwrap())
                    .unwrap_or_default();
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).unwrap();
                if path.starts_with("POST /api/printing/v1/devices") {
                    let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    if path.ends_with("/browser-link") {
                        assert_eq!(value, serde_json::json!({"challenge": "a".repeat(64)}));
                    } else {
                        assert_eq!(value, device);
                    }
                }
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let config = Settings {
            url: origin.clone(),
            ..Settings::default()
        };
        assert!(check(&config, Some(CENTRAL)).unwrap().contains("validada"));

        let client = Client::new(&origin).unwrap();
        let device = DeviceIdentity {
            id: ID.into(),
            token: Zeroizing::new(TOKEN.into()),
        };
        client.enroll(CENTRAL, &device, "Caja local").unwrap();
        client.enroll(CENTRAL, &device, "Caja local").unwrap();
        assert_eq!(client.connection(TOKEN).unwrap().id, ID);
        client.approve_browser_link(TOKEN, &"a".repeat(64)).unwrap();
        assert!(client.claim(TOKEN).unwrap().is_none());
        server.join().unwrap();
    }
}
