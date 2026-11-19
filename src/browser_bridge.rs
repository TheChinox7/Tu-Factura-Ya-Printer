use crate::{
    api,
    settings::{self, Settings},
};
use serde::Deserialize;
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::mpsc::SyncSender,
    thread,
    time::{Duration, Instant},
};
use ureq::http::StatusCode;

const PORT: u16 = 17881;
const HOST: &str = "127.0.0.1:17881";
const LINK_PATH: &str = "/v1/link";
const WAKE_PATH: &str = "/v1/wake";
const HEAD_LIMIT: usize = 8192;
const BODY_LIMIT: usize = 512;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserLink {
    challenge: String,
}

// Device credentials stay in native storage. The local queue notification contains no job data.
pub fn start(wake: SyncSender<()>) -> io::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, PORT))?;
    thread::Builder::new()
        .name("browser-association".into())
        .spawn(move || {
            for connection in listener.incoming() {
                let mut stream = match connection {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let _ = handle(
                    &mut stream,
                    settings::load,
                    api::approve_browser_link,
                    || {
                        let _ = wake.try_send(());
                    },
                );
            }
        })?;
    Ok(())
}

fn handle(
    stream: &mut TcpStream,
    load: impl FnOnce() -> Result<Settings, String>,
    approve: impl FnOnce(&Settings, &str) -> Result<(), api::Error>,
    wake: impl FnOnce(),
) -> io::Result<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut buffer = [0; HEAD_LIMIT + BODY_LIMIT];
    let mut received = 0;
    let head_length = loop {
        let count = read_before(stream, &mut buffer[received..HEAD_LIMIT], deadline)?;
        received += count;
        if let Some(end) = buffer[..received]
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        {
            break end + 4;
        }
        if count == 0 || received == HEAD_LIMIT {
            return reply(stream, None, 400, "La solicitud local es inválida.");
        }
    };

    let mut slots = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut slots);
    if !matches!(
        request.parse(&buffer[..head_length]),
        Ok(httparse::Status::Complete(_))
    ) || request.version != Some(1)
    {
        return reply(stream, None, 400, "La solicitud local es inválida.");
    }
    if header(request.headers, "host") != Some(HOST) {
        return reply(stream, None, 403, "Este destino local no está autorizado.");
    }
    let Some(origin) = header(request.headers, "origin") else {
        return reply(stream, None, 403, "Este origen no está autorizado.");
    };
    let config = load().ok().filter(|config| {
        url::Url::parse(&config.url).is_ok_and(|url| url.origin().ascii_serialization() == origin)
    });
    let Some(config) = config else {
        return reply(stream, None, 403, "Este origen no está autorizado.");
    };
    let origin = origin.to_owned();
    let cors = Some(origin.as_str());
    let wakes_worker = request.path == Some(WAKE_PATH);
    if !wakes_worker && request.path != Some(LINK_PATH) {
        return reply(stream, cors, 404, "La ruta local no existe.");
    }

    if request.method == Some("OPTIONS") {
        if header(request.headers, "access-control-request-method") != Some("POST")
            || !header(request.headers, "access-control-request-headers")
                .is_some_and(|value| value.eq_ignore_ascii_case("content-type"))
        {
            return reply(
                stream,
                cors,
                403,
                "La solicitud de acceso local es inválida.",
            );
        }
        return reply(stream, cors, 204, "");
    }
    if request.method != Some("POST") {
        return reply(stream, cors, 405, "El método de vinculación es inválido.");
    }
    if !header(request.headers, "content-type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return reply(stream, cors, 415, "La solicitud debe enviarse como JSON.");
    }

    let size = header(request.headers, "content-length")
        .filter(|length| length.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|length| length.parse::<usize>().ok())
        .filter(|length| (1..=BODY_LIMIT).contains(length));
    let Some(size) = size else {
        return reply(
            stream,
            cors,
            400,
            "El tamaño de la solicitud local es inválido.",
        );
    };
    let end = head_length + size;
    if request
        .headers
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("transfer-encoding"))
        || received > end
    {
        return reply(stream, cors, 400, "La solicitud local es inválida.");
    }
    while received < end {
        let count = read_before(stream, &mut buffer[received..end], deadline)?;
        if count == 0 {
            return reply(stream, cors, 400, "La solicitud local está incompleta.");
        }
        received += count;
    }
    let body = &buffer[head_length..end];
    if wakes_worker {
        if !matches!(serde_json::from_slice(body), Ok(serde_json::Value::Object(fields)) if fields.is_empty())
        {
            return reply(
                stream,
                cors,
                400,
                "La notificación de impresión es inválida.",
            );
        }
        wake();
        return reply(stream, cors, 204, "");
    }
    let link = serde_json::from_slice::<BrowserLink>(body);
    let Ok(link) = link else {
        return reply(
            stream,
            cors,
            400,
            "La solicitud de vinculación es inválida.",
        );
    };
    if !valid_challenge(&link.challenge) {
        return reply(
            stream,
            cors,
            400,
            "La solicitud de vinculación es inválida.",
        );
    }
    if config.printer.trim().is_empty() {
        return reply(
            stream,
            cors,
            409,
            "Selecciona una impresora y guarda la configuración en Printer.",
        );
    }

    match approve(&config, &link.challenge) {
        Ok(()) => reply(stream, cors, 204, ""),
        Err(error) => {
            let (status, message) = match error.status {
                Some(401 | 403) => (
                    403,
                    "Tu Factura Ya no autorizó este equipo. Revisa la conexión en Printer.",
                ),
                Some(404) => (
                    404,
                    "Actualiza Tu Factura Ya para activar la detección de Printer.",
                ),
                Some(409) => (409, error.message.as_str()),
                Some(410) => (
                    410,
                    "La solicitud de vinculación venció. Vuelve a imprimir.",
                ),
                Some(429) => (429, "Espera unos segundos antes de volver a imprimir."),
                _ => (
                    502,
                    "Printer no pudo vincularse con Tu Factura Ya. Revisa la conexión en Printer.",
                ),
            };
            reply(stream, cors, status, message)
        }
    }
}

fn read_before(stream: &mut TcpStream, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
    let wait = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(io::ErrorKind::TimedOut)?;
    stream.set_read_timeout(Some(wait))?;
    stream.read(bytes)
}

fn header<'a>(headers: &[httparse::Header<'a>], name: &str) -> Option<&'a str> {
    let mut values = headers
        .iter()
        .filter(|field| field.name.eq_ignore_ascii_case(name));
    let field = values.next()?;
    if values.next().is_some() {
        return None;
    }
    std::str::from_utf8(field.value).ok().map(str::trim)
}

fn valid_challenge(challenge: &str) -> bool {
    challenge.len() == 64
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn reply(
    stream: &mut TcpStream,
    origin: Option<&str>,
    status: u16,
    message: &str,
) -> io::Result<()> {
    let code = StatusCode::from_u16(status).expect("supported HTTP status");
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        code.canonical_reason().unwrap_or_default()
    )?;
    if let Some(origin) = origin {
        write!(
            stream,
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers\r\nAccess-Control-Allow-Methods: POST\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Allow-Private-Network: true\r\n"
        )?;
    }
    if status == 204 {
        return write!(stream, "\r\n");
    }
    let body = serde_json::json!({"message": message}).to_string();
    write!(
        stream,
        "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://caja.example.com:444";

    fn exchange(request: String, expect_forward: bool, expect_wake: bool) -> String {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut forwarded = false;
            let mut signaled = false;
            handle(
                &mut client,
                || {
                    Ok(Settings {
                        url: format!("{ORIGIN}/facturacion"),
                        printer: "Test only".into(),
                        ..Settings::default()
                    })
                },
                |config, challenge| {
                    assert_eq!(config.url, format!("{ORIGIN}/facturacion"));
                    assert_eq!(challenge, "0a".repeat(32));
                    forwarded = true;
                    Ok(())
                },
                || signaled = true,
            )
            .unwrap();
            assert_eq!(forwarded, expect_forward);
            assert_eq!(signaled, expect_wake);
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        response
    }

    fn post(path: &str, host: &str, origin: &str, body: &str, content_type: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn allows_only_configured_origin_with_loopback_host_and_valid_payload() {
        let make_link = |host, origin, challenge: &str, content_type| {
            let body = serde_json::json!({"challenge": challenge}).to_string();
            post(LINK_PATH, host, origin, &body, content_type)
        };
        let nonce = "0a".repeat(32);
        let response = exchange(
            make_link(HOST, ORIGIN, &nonce, "application/json"),
            true,
            false,
        );
        assert!(response.starts_with("HTTP/1.1 204"));
        assert!(response.contains(&format!("Access-Control-Allow-Origin: {ORIGIN}\r\n")));
        for (host, origin) in [
            ("caja.example.com:17881", ORIGIN),
            (HOST, "https://otra.example.com:444"),
            (HOST, "null"),
        ] {
            let response = exchange(
                make_link(host, origin, &nonce, "application/json"),
                false,
                false,
            );
            assert!(response.starts_with("HTTP/1.1 403"));
            assert!(!response.contains("Access-Control-Allow-Origin"));
        }
        let response = exchange(
            make_link(HOST, ORIGIN, "AB", "application/json"),
            false,
            false,
        );
        assert!(response.starts_with("HTTP/1.1 400"));
        let response = exchange(make_link(HOST, ORIGIN, &nonce, "text/plain"), false, false);
        assert!(response.starts_with("HTTP/1.1 415"));
    }

    #[test]
    fn preflight_does_not_associate_or_accept_other_methods() {
        let request = |path, method| {
            format!(
                "OPTIONS {path} HTTP/1.1\r\nHost: {HOST}\r\nOrigin: {ORIGIN}\r\nAccess-Control-Request-Method: {method}\r\nAccess-Control-Request-Headers: content-type\r\nAccess-Control-Request-Private-Network: true\r\n\r\n"
            )
        };
        for path in [LINK_PATH, WAKE_PATH] {
            let response = exchange(request(path, "POST"), false, false);
            assert!(response.starts_with("HTTP/1.1 204"));
            assert!(response.contains("Access-Control-Allow-Private-Network: true"));
            let response = exchange(request(path, "GET"), false, false);
            assert!(response.starts_with("HTTP/1.1 403"));
        }
    }

    #[test]
    fn queue_notification_wakes_the_worker_only_for_the_saved_origin_and_an_empty_object() {
        let request = |origin, body| post(WAKE_PATH, HOST, origin, body, "application/json");
        let response = exchange(request(ORIGIN, "{}"), false, true);
        assert!(response.starts_with("HTTP/1.1 204"));
        assert_eq!(response.split_once("\r\n\r\n").unwrap().1, "");

        let response = exchange(request("https://otra.example.com:444", "{}"), false, false);
        assert!(response.starts_with("HTTP/1.1 403"));
        assert!(!response.contains("Access-Control-Allow-Origin"));

        for invalid in ["[]", "{\"job\":1}"] {
            let response = exchange(request(ORIGIN, invalid), false, false);
            assert!(response.starts_with("HTTP/1.1 400"));
        }
    }
}
