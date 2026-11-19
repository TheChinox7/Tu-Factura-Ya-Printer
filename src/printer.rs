use crate::settings::{PrintMode, Settings};
use barcoders::sym::code128::Code128;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::sync::Mutex;

#[cfg(unix)]
mod cups;
#[cfg(windows)]
mod winspool;
#[cfg(unix)]
use cups as platform;
#[cfg(windows)]
use winspool as platform;

static DEVICE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
pub struct Receipt {
    pub title: String,
    pub lines: Vec<String>,
    #[serde(default)]
    pub barcode: Option<String>,
}

pub struct Printers {
    pub default: Option<String>,
    pub names: Vec<String>,
}

pub struct Failure {
    pub message: String,
    pub uncertain: bool,
}

impl Failure {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            uncertain: false,
        }
    }
    fn uncertain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            uncertain: true,
        }
    }
}

pub fn list() -> Result<Printers, String> {
    let mut printers = platform::list()?;
    printers
        .names
        .sort_unstable_by_key(|name| name.to_lowercase());
    printers.names.dedup();
    Ok(printers)
}

pub fn test(config: &Settings) -> Result<String, String> {
    let receipt = Receipt {
        title: "TU FACTURA YA PRINTER".into(),
        lines: vec![
            concat!("Prueba de impresión · ", env!("CARGO_PKG_VERSION")).into(),
            format!("Impresora: {}", config.printer),
            format!("Perfil de papel: {} mm", config.width),
            "".into(),
            "Texto: ÁÉÍÓÚ · ñ · − · $12,34".into(),
            "Si lees este ticket, la cola está configurada.".into(),
        ],
        barcode: Some("123456789012".into()),
    };
    submit(config, &receipt, "Tu Factura Ya - prueba", true)
        .map(|id| match id {
            Some(id) => format!("Prueba enviada a la impresora. Trabajo {id}."),
            None => "Prueba enviada a la impresora.".into(),
        })
        .map_err(|error| error.message)
}

pub fn submit(
    config: &Settings,
    receipt: &Receipt,
    name: &str,
    test_page: bool,
) -> Result<Option<String>, Failure> {
    if config.printer.is_empty() || config.printer.chars().any(char::is_control) {
        return Err(Failure::failed("Selecciona una impresora instalada."));
    }
    if !matches!(config.width, 58 | 80)
        || receipt.lines.len() > 3000
        || receipt.title.len() > 240
        || receipt.lines.iter().any(|line| line.len() > 8192)
    {
        return Err(Failure::failed(
            "El trabajo supera el tamaño admitido por el agente.",
        ));
    }
    let lines = layout(receipt, config.width);
    let code = receipt.barcode.as_deref().and_then(barcode);
    let _lock = DEVICE_LOCK
        .lock()
        .map_err(|_| Failure::failed("Error al reservar la impresora local."))?;
    match config.mode {
        PrintMode::EscPos => platform::raw(
            &config.printer,
            name,
            &esc_pos(&lines, code.as_deref(), config.width),
        ),
        PrintMode::System => platform::system(
            &config.printer,
            name,
            &lines,
            code.as_deref(),
            config.width,
            test_page,
        ),
    }
}

pub fn submit_pdf(config: &Settings, ticket: &str, name: &str) -> Result<Option<String>, Failure> {
    if config.printer.is_empty() || config.printer.chars().any(char::is_control) {
        return Err(Failure::failed("Selecciona una impresora instalada."));
    }
    if config.mode != PrintMode::System {
        return Err(Failure::failed(
            "El ticket con diseño requiere Modo del sistema y una impresora con controlador instalado.",
        ));
    }

    const MAX_PDF_BYTES: usize = 5 * 1024 * 1024;
    let invalid_pdf = || Failure::failed("El servidor devolvió un PDF de impresión inválido.");
    if ticket.len() > MAX_PDF_BYTES.div_ceil(3) * 4 {
        return Err(invalid_pdf());
    }
    let bytes = STANDARD.decode(ticket).map_err(|_| invalid_pdf())?;
    if bytes.len() > MAX_PDF_BYTES || !bytes.starts_with(b"%PDF-") {
        return Err(invalid_pdf());
    }

    let _lock = DEVICE_LOCK
        .lock()
        .map_err(|_| Failure::failed("Error al reservar la impresora local."))?;
    platform::formatted_pdf(&config.printer, name, &bytes)
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else if character == '−' {
                '-'
            } else {
                character
            }
        })
        .collect()
}

fn layout(receipt: &Receipt, width: u16) -> Vec<String> {
    let columns = if width == 58 { 32 } else { 42 };
    std::iter::once(&receipt.title)
        .chain(&receipt.lines)
        .flat_map(|text| {
            let text: Vec<char> = clean(text).chars().collect();
            if text.is_empty() {
                return vec![String::new()];
            }
            text.chunks(columns)
                .map(|line| line.iter().collect())
                .collect()
        })
        .collect()
}

fn barcode(value: &str) -> Option<Vec<u8>> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    {
        return None;
    }
    let value = if value.bytes().all(|byte| byte.is_ascii_digit()) && value.len() > 1 {
        if value.len().is_multiple_of(2) {
            format!("\u{0106}{value}")
        } else {
            format!("\u{0181}{}\u{0106}{}", &value[..1], &value[1..])
        }
    } else {
        format!("\u{0181}{value}")
    };
    Code128::new(value).ok().map(|value| value.encode())
}

fn western(text: &str) -> Vec<u8> {
    text.chars()
        .map(|character| {
            let mut utf8 = [0; 4];
            let (encoded, _, invalid) =
                encoding_rs::WINDOWS_1252.encode(character.encode_utf8(&mut utf8));
            if invalid { b'?' } else { encoded[0] }
        })
        .collect()
}

fn esc_pos(lines: &[String], barcode: Option<&[u8]>, width: u16) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"\x1b@\x1bt\x10\x1ba\x01\x1bE\x01");
    for (index, line) in lines.iter().enumerate() {
        data.extend(western(line));
        data.push(b'\n');
        if index == 0 {
            data.extend_from_slice(b"\x1bE\x00\x1ba\x00");
        }
    }
    if let Some(bits) = barcode {
        let max: usize = if width == 58 { 384 } else { 512 };
        if (bits.len() + 20).next_multiple_of(8) <= max {
            let scale = if ((bits.len() + 20) * 2).next_multiple_of(8) <= max {
                2
            } else {
                1
            };
            let bytes = ((bits.len() + 20) * scale).div_ceil(8);
            let mut row = vec![0u8; bytes];
            for (index, _) in bits.iter().enumerate().filter(|(_, value)| **value == 1) {
                for i in 0..scale {
                    let x = (index + 10) * scale + i;
                    row[x / 8] |= 1 << (7 - x % 8);
                }
            }
            data.extend_from_slice(b"\n\x1ba\x01\x1dv0\x00");
            data.extend_from_slice(&(bytes as u16).to_le_bytes());
            data.extend_from_slice(&56u16.to_le_bytes());
            for _ in 0..56 {
                data.extend_from_slice(&row);
            }
            data.push(b'\n');
        }
    }
    data.extend_from_slice(b"\n\n\n\x1dV\x01");
    data
}

#[cfg(unix)]
fn pdf(lines: &[String], barcode: Option<&[u8]>, width: u16, test_page: bool) -> Vec<u8> {
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
    const PAGE_LINES: usize = 125;
    const FONT: Name<'static> = Name(b"Text");
    let mut pdf = Pdf::new();
    let page_tree = Ref::new(2);
    let font = Ref::new(3);
    let pages = lines.len().div_ceil(PAGE_LINES).max(1);
    pdf.catalog(Ref::new(1)).pages(page_tree);
    pdf.pages(page_tree)
        .kids((0..pages).map(|i| Ref::new(4 + i as i32 * 2)))
        .count(pages as i32);
    pdf.type1_font(font)
        .base_font(Name(b"Courier"))
        .encoding_predefined(Name(b"WinAnsiEncoding"));
    for (index, page_lines) in lines.chunks(PAGE_LINES).enumerate() {
        let page_id = Ref::new(4 + index as i32 * 2);
        let stream = Ref::new(5 + index as i32 * 2);
        let show_barcode = barcode.filter(|_| index + 1 == pages);
        let width = if test_page {
            595.0
        } else {
            width as f32 * 72.0 / 25.4
        };
        let height = if test_page {
            842.0
        } else {
            32.0 + page_lines.len() as f32 * 12.0 + if show_barcode.is_some() { 40.0 } else { 0.0 }
        };
        let margin = if test_page { 30.0 } else { 7.0 };
        let top = height - if test_page { 38.0 } else { 18.0 };
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, width, height))
            .parent(page_tree)
            .contents(stream);
        page.resources().fonts().pair(FONT, font);
        page.finish();
        let mut content = Content::new();
        let font_size = if test_page { 12.0 } else { 7.5 };
        content
            .begin_text()
            .set_font(FONT, font_size)
            .next_line(margin, top);
        for (line, text) in page_lines.iter().enumerate() {
            if line > 0 {
                content.next_line(0.0, -12.0);
            }
            content.show(Str(&western(text)));
        }
        content.end_text();
        if let Some(bits) = show_barcode {
            let usable = if test_page {
                185.0
            } else {
                width - 2.0 * margin
            };
            let module = usable / (bits.len() + 20) as f32;
            let y = top - page_lines.len() as f32 * 12.0 - 29.0;
            for (x, bit) in bits.iter().enumerate() {
                if *bit == 1 {
                    content.rect(margin + (x + 10) as f32 * module, y, module, 25.0);
                }
            }
            content.fill_nonzero();
        }
        pdf.stream(stream, &content.finish());
    }
    pdf.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_removes_control_codes_and_wraps_spanish_text() {
        let receipt = Receipt {
            title: "Factura".into(),
            lines: vec!["ñ".repeat(35) + "\x1bp"],
            barcode: None,
        };
        let lines = layout(&receipt, 58);
        assert_eq!(lines[1].chars().count(), 32);
        assert_eq!(lines[2], "ñññ p");
        assert!(
            !esc_pos(&lines, None, 58)
                .windows(2)
                .any(|bytes| bytes == b"\x1bp")
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepares_a_readable_pdf_and_a_compact_fiscal_barcode() {
        let bars = barcode(&"1".repeat(49)).unwrap();
        assert!((bars.len() + 20).next_multiple_of(8) <= 384);
        let pages = pdf(&["Prueba de impresión".into()], Some(&bars), 58, false);
        assert!(pages.starts_with(b"%PDF-"));
        assert!(pages.ends_with(b"%%EOF"));
    }
}
