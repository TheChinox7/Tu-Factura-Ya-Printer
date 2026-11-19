use super::{Failure, Printers};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, GetLastError},
    Graphics::{Gdi::*, Printing::*},
    Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage},
};

mod pdf;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

pub(super) fn list() -> Result<Printers, String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut required = 0;
    let mut returned = 0;
    let mut names = Vec::new();
    unsafe {
        EnumPrintersW(
            flags,
            null(),
            4,
            null_mut(),
            0,
            &mut required,
            &mut returned,
        );
        if required > 0 {
            // Use storage aligned for the native printer information structures.
            let mut buffer = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
            let size = (buffer.len() * size_of::<usize>()) as u32;
            if EnumPrintersW(
                flags,
                null(),
                4,
                buffer.as_mut_ptr().cast(),
                size,
                &mut required,
                &mut returned,
            ) == 0
            {
                return Err("No se pudo obtener la lista de impresoras de Windows.".into());
            }
            let entries: &[PRINTER_INFO_4W] =
                std::slice::from_raw_parts(buffer.as_ptr().cast(), returned as usize);
            for entry in entries {
                if !entry.pPrinterName.is_null() {
                    let mut len = 0;
                    while *entry.pPrinterName.add(len) != 0 {
                        len += 1;
                    }
                    names.push(String::from_utf16_lossy(std::slice::from_raw_parts(
                        entry.pPrinterName,
                        len,
                    )));
                }
            }
        }
    }
    let mut length = 0;
    let default = unsafe {
        GetDefaultPrinterW(null_mut(), &mut length);
        if length == 0 || GetLastError() == ERROR_FILE_NOT_FOUND {
            None
        } else {
            let mut name = vec![0u16; length as usize];
            (GetDefaultPrinterW(name.as_mut_ptr(), &mut length) != 0)
                .then(|| String::from_utf16_lossy(&name[..length.saturating_sub(1) as usize]))
        }
    };
    Ok(Printers { default, names })
}

struct Printer(PRINTER_HANDLE);
impl Drop for Printer {
    fn drop(&mut self) {
        unsafe {
            ClosePrinter(self.0);
        }
    }
}

pub(super) fn raw(printer: &str, name: &str, bytes: &[u8]) -> Result<Option<String>, Failure> {
    let mut handle = PRINTER_HANDLE::default();
    let printer = wide(printer);
    let mut doc_name = wide(name);
    let mut mode = wide("RAW");
    let document = DOC_INFO_1W {
        pDocName: doc_name.as_mut_ptr(),
        pDatatype: mode.as_mut_ptr(),
        pOutputFile: null_mut(),
    };
    unsafe {
        if OpenPrinterW(printer.as_ptr(), &mut handle, null()) == 0 {
            return Err(Failure::failed(
                "Windows no pudo abrir la impresora seleccionada.",
            ));
        }
        let handle = Printer(handle);
        let job = StartDocPrinterW(handle.0, 1, &document);
        if job == 0 {
            return Err(Failure::failed("Windows no pudo crear el trabajo RAW."));
        }
        if StartPagePrinter(handle.0) == 0 {
            AbortPrinter(handle.0);
            return Err(Failure::failed("Windows no pudo preparar el trabajo RAW."));
        }
        for chunk in bytes.chunks(64 * 1024) {
            let mut written = 0;
            if WritePrinter(
                handle.0,
                chunk.as_ptr().cast(),
                chunk.len() as u32,
                &mut written,
            ) == 0
                || written != chunk.len() as u32
            {
                AbortPrinter(handle.0);
                return Err(Failure::uncertain(
                    "Windows no confirmó la escritura completa del ticket.",
                ));
            }
        }
        if EndPagePrinter(handle.0) == 0 || EndDocPrinter(handle.0) == 0 {
            AbortPrinter(handle.0);
            return Err(Failure::uncertain(
                "Windows no confirmó el cierre del trabajo RAW.",
            ));
        }
        Ok(Some(job.to_string()))
    }
}

struct Device(HDC);
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.0);
        }
    }
}
struct Font(HFONT);
impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.0);
        }
    }
}

pub(super) fn system(
    printer: &str,
    name: &str,
    lines: &[String],
    barcode: Option<&[u8]>,
    width: u16,
    test_page: bool,
) -> Result<Option<String>, Failure> {
    let printer = wide(printer);
    let driver = wide("WINSPOOL");
    let name = wide(name);
    let doc = DOCINFOW {
        cbSize: size_of::<DOCINFOW>() as i32,
        lpszDocName: name.as_ptr(),
        ..Default::default()
    };
    unsafe {
        let dc = Device(CreateDCW(driver.as_ptr(), printer.as_ptr(), null(), null()));
        if dc.0.is_null() {
            return Err(Failure::failed(
                "Windows no pudo abrir el controlador de impresión.",
            ));
        }
        let dpi_x = GetDeviceCaps(dc.0, LOGPIXELSX as i32).max(72);
        let dpi_y = GetDeviceCaps(dc.0, LOGPIXELSY as i32).max(72);
        let page_width = GetDeviceCaps(dc.0, HORZRES as i32);
        let page_height = GetDeviceCaps(dc.0, VERTRES as i32);
        let margin_x = (dpi_x / 12).max(1);
        let margin_y = (dpi_y / 10).max(1);
        let step = (dpi_y / 6).max(1);
        let columns = if width == 58 { 32 } else { 42 };
        let max_size = ((page_width - margin_x * 2) * 5 / (columns * 3)).max(1);
        let text_size = -(dpi_y * if test_page { 11 } else { 8 } / 72).min(max_size);
        let face = wide("Courier New");
        let font = Font(CreateFontW(
            text_size,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            0,
            0,
            0,
            0,
            face.as_ptr(),
        ));
        if font.0.is_null() || page_width <= margin_x * 2 || page_height <= step * 6 {
            return Err(Failure::failed(
                "El controlador de Windows informó un tamaño de papel inválido.",
            ));
        }
        let job = StartDocW(dc.0, &doc);
        if job <= 0 {
            return Err(Failure::failed(
                "Windows no pudo crear el trabajo de impresión.",
            ));
        }
        let page_lines = ((page_height - margin_y * 2) / step - 4).max(1) as usize;
        let page_count = lines.len().div_ceil(page_lines);
        let rendered = lines
            .chunks(page_lines)
            .enumerate()
            .all(|(page, page_text)| {
                if StartPage(dc.0) <= 0 {
                    return false;
                }
                let previous = SelectObject(dc.0, font.0);
                let drew = page_text.iter().enumerate().all(|(index, line)| {
                    let text: Vec<u16> = line.encode_utf16().collect();
                    text.is_empty()
                        || TextOutW(
                            dc.0,
                            margin_x,
                            margin_y + index as i32 * step,
                            text.as_ptr(),
                            text.len() as i32,
                        ) != 0
                });
                let drew_code = if let Some(bits) = barcode.filter(|_| page + 1 == page_count) {
                    let drawable = (page_width - margin_x * 2).min(dpi_x * 3);
                    let module = drawable / (bits.len() as i32 + 20);
                    module < 1
                        || bits
                            .iter()
                            .enumerate()
                            .filter(|(_, value)| **value == 1)
                            .all(|(index, _)| {
                                PatBlt(
                                    dc.0,
                                    margin_x + (index as i32 + 10) * module,
                                    margin_y + step * (page_text.len() as i32 + 1),
                                    module,
                                    step * 2,
                                    BLACKNESS,
                                ) != 0
                            })
                } else {
                    true
                };
                SelectObject(dc.0, previous);
                drew && drew_code && EndPage(dc.0) > 0
            });
        if !rendered || EndDoc(dc.0) <= 0 {
            AbortDoc(dc.0);
            return Err(Failure::uncertain(
                "Windows no confirmó el envío completo al controlador.",
            ));
        }
        Ok(Some(job.to_string()))
    }
}

pub(super) fn formatted_pdf(
    printer: &str,
    name: &str,
    bytes: &[u8],
) -> Result<Option<String>, Failure> {
    let _apartment = pdf::Apartment::initialize()?;
    let document = pdf::load(bytes)?;
    let pages = document.PageCount().map_err(|_| pdf::invalid())?;
    if !(1..=128).contains(&pages) {
        return Err(pdf::invalid());
    }

    let printer = wide(printer);
    let driver = wide("WINSPOOL");
    let name = wide(name);
    let doc = DOCINFOW {
        cbSize: size_of::<DOCINFOW>() as i32,
        lpszDocName: name.as_ptr(),
        ..Default::default()
    };
    unsafe {
        let dc = Device(CreateDCW(driver.as_ptr(), printer.as_ptr(), null(), null()));
        if dc.0.is_null() {
            return Err(Failure::failed(
                "Windows no pudo abrir el controlador de impresión.",
            ));
        }
        let media = pdf::Media::from_device(dc.0)?;
        let first = pdf::render(&document, 0, &media)?;
        let job = StartDocW(dc.0, &doc);
        if job <= 0 {
            return Err(Failure::failed(
                "Windows no pudo crear el trabajo de impresión.",
            ));
        }
        let rendered = pdf::draw(dc.0, &first, &media)
            && (1..pages).all(|index| {
                pdf::render(&document, index, &media)
                    .is_ok_and(|page| pdf::draw(dc.0, &page, &media))
            });
        if !rendered || EndDoc(dc.0) <= 0 {
            AbortDoc(dc.0);
            return Err(Failure::uncertain(
                "Windows no confirmó el envío completo del ticket PDF.",
            ));
        }
        Ok(Some(job.to_string()))
    }
}
