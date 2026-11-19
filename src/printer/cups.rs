use super::{Failure, Printers};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn command(name: &str) -> Command {
    let mut command = Command::new(name);
    command.env("LC_ALL", "C");
    command
}

pub(super) fn list() -> Result<Printers, String> {
    // `-e` prints destination identifiers without localized prose. macOS can
    // localize both `lpstat -p` and `lpstat -d` even with LC_ALL set to C.
    let output = command("lpstat")
        .arg("-e")
        .output()
        .map_err(|_| "No se pudo consultar CUPS (lpstat).")?;
    if !output.status.success() {
        return Err("No se pudo obtener la lista de impresoras desde CUPS.".into());
    }
    let names: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    let default = command("lpstat")
        .arg("-d")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| default_destination(&String::from_utf8_lossy(&output.stdout), &names));
    Ok(Printers { default, names })
}

fn default_destination(output: &str, names: &[String]) -> Option<String> {
    let (_, name) = output.trim().split_once(':')?;
    names
        .iter()
        .find(|printer| printer.as_str() == name.trim())
        .cloned()
}

fn request_id(output: &str, printer: &str) -> Option<String> {
    let prefix = format!("{printer}-");
    output.split_whitespace().find_map(|word| {
        let candidate = word.trim_matches(['(', ')', '«', '»', '"', '\'', ',', '.']);
        let number = candidate.strip_prefix(&prefix)?;
        (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| candidate.to_owned())
    })
}

pub(super) fn raw(printer: &str, name: &str, bytes: &[u8]) -> Result<Option<String>, Failure> {
    send(printer, name, bytes, &["-o", "raw"])
}

pub(super) fn system(
    printer: &str,
    name: &str,
    lines: &[String],
    barcode: Option<&[u8]>,
    width: u16,
    test_page: bool,
) -> Result<Option<String>, Failure> {
    send(
        printer,
        name,
        &super::pdf(lines, barcode, width, test_page),
        &[],
    )
}

pub(super) fn formatted_pdf(
    printer: &str,
    name: &str,
    bytes: &[u8],
) -> Result<Option<String>, Failure> {
    send(printer, name, bytes, &["-o", "print-scaling=none"])
}

fn send(
    printer: &str,
    name: &str,
    bytes: &[u8],
    options: &[&str],
) -> Result<Option<String>, Failure> {
    let mut child = command("lp");
    child.args(["-d", printer, "-t", name]).args(options);
    let mut child = child
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Failure::failed("No se pudo abrir CUPS (lp)."))?;
    let written = child
        .stdin
        .take()
        .is_some_and(|mut input| input.write_all(bytes).is_ok());
    let result = child
        .wait_with_output()
        .map_err(|_| Failure::uncertain("CUPS no devolvió el resultado del envío."))?;
    if !written || !result.status.success() {
        let message = String::from_utf8_lossy(&result.stderr);
        if message.contains("The printer or class does not exist")
            || message.contains("Unknown destination")
        {
            return Err(Failure::failed(
                "La impresora seleccionada ya no está instalada o disponible en CUPS.",
            ));
        }
        return Err(Failure::uncertain(
            "CUPS no pudo confirmar el envío. Revisa la cola y el estado de la impresora.",
        ));
    }
    Ok(request_id(
        &String::from_utf8_lossy(&result.stdout),
        printer,
    ))
}

#[cfg(test)]
mod tests {
    use super::{default_destination, request_id};

    #[test]
    fn finds_the_default_printer_when_cups_localizes_its_message() {
        let names = vec!["Brother_DCP_T720DW".to_owned(), "Caja_02".to_owned()];
        assert_eq!(
            default_destination(
                "destino por omisión del sistema: Brother_DCP_T720DW\n",
                &names
            ),
            Some(names[0].clone()),
        );
        assert_eq!(
            default_destination("system default destination: Caja_02\n", &names),
            Some(names[1].clone())
        );
        assert_eq!(
            default_destination("no system default destination\n", &names),
            None
        );
        assert_eq!(
            default_destination("system default destination: Other\n", &names),
            None
        );
    }

    #[test]
    fn extracts_the_spool_job_from_localized_output() {
        assert_eq!(
            request_id("request id is Caja-42 (1 file(s))", "Caja"),
            Some("Caja-42".into())
        );
        assert_eq!(
            request_id("la id de la solicitud es «Caja-42» (1 archivo)", "Caja"),
            Some("Caja-42".into())
        );
        assert_eq!(request_id("request id is Caja_Otra-42", "Caja"), None);
        assert_eq!(request_id("sin identificador Caja-test", "Caja"), None);
    }
}
