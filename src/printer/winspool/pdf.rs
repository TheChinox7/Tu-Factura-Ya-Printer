use super::Failure;
use image::ImageFormat;
use std::mem::size_of;
use windows::{
    Data::Pdf::{PdfDocument, PdfPageRenderOptions},
    Graphics::Imaging::BitmapEncoder,
    Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream},
    UI::Color,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
};
use windows_sys::Win32::{
    Graphics::Gdi::*,
    Storage::Xps::{EndPage, StartPage},
};

const MAX_PIXELS: f64 = 12_000_000.0;

pub(super) struct Apartment;

impl Apartment {
    pub(super) fn initialize() -> Result<Self, Failure> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|_| Failure::failed("No se pudo iniciar la lectura de PDF de Windows."))?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}

pub(super) fn invalid() -> Failure {
    Failure::failed("Windows no pudo leer o preparar el ticket PDF.")
}

pub(super) fn load(bytes: &[u8]) -> Result<PdfDocument, Failure> {
    let load = || -> windows::core::Result<PdfDocument> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream.GetOutputStreamAt(0)?)?;
        writer.WriteBytes(bytes)?;
        writer.StoreAsync()?.join()?;
        writer.DetachStream()?;
        PdfDocument::LoadFromStreamAsync(&stream)?.join()
    };
    load().map_err(|_| invalid())
}

pub(super) struct Media {
    width: i32,
    height: i32,
    offset_x: i32,
    offset_y: i32,
    dpi_x: i32,
    dpi_y: i32,
}

impl Media {
    pub(super) unsafe fn from_device(device: HDC) -> Result<Self, Failure> {
        let caps = |field| unsafe { GetDeviceCaps(device, field as i32) };
        let media = Self {
            width: caps(PHYSICALWIDTH).max(caps(HORZRES)),
            height: caps(PHYSICALHEIGHT).max(caps(VERTRES)),
            offset_x: caps(PHYSICALOFFSETX).max(0),
            offset_y: caps(PHYSICALOFFSETY).max(0),
            dpi_x: caps(LOGPIXELSX),
            dpi_y: caps(LOGPIXELSY),
        };
        if media.width <= 0 || media.height <= 0 || media.dpi_x <= 0 || media.dpi_y <= 0 {
            return Err(Failure::failed(
                "El controlador de Windows informó un tamaño de papel inválido.",
            ));
        }
        Ok(media)
    }
}

pub(super) struct Page {
    width: i32,
    height: i32,
    output_width: i32,
    output_height: i32,
    bgra: Vec<u8>,
}

pub(super) fn render(document: &PdfDocument, index: u32, media: &Media) -> Result<Page, Failure> {
    let page = document.GetPage(index).map_err(|_| invalid())?;
    let size = page.Size().map_err(|_| invalid())?;
    // Windows.Data.Pdf dimensions are effective pixels at 96 DPI.
    let width = f64::from(size.Width) * f64::from(media.dpi_x) / 96.0;
    let height = f64::from(size.Height) * f64::from(media.dpi_y) / 96.0;
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return Err(invalid());
    }
    let fit = 1.0_f64
        .min(f64::from(media.width) / width)
        .min(f64::from(media.height) / height);
    let width = width * fit;
    let height = height * fit;
    let raster_scale = 1.0_f64.min((MAX_PIXELS / (width * height)).sqrt());
    let raster_width = (width * raster_scale).floor().max(1.0) as u32;
    let raster_height = (height * raster_scale).floor().max(1.0) as u32;

    let render_bmp = || -> windows::core::Result<Vec<u8>> {
        let output = InMemoryRandomAccessStream::new()?;
        let options = PdfPageRenderOptions::new()?;
        options.SetBitmapEncoderId(BitmapEncoder::BmpEncoderId()?)?;
        options.SetBackgroundColor(Color {
            A: 255,
            R: 255,
            G: 255,
            B: 255,
        })?;
        options.SetDestinationWidth(raster_width)?;
        options.SetDestinationHeight(raster_height)?;
        page.RenderWithOptionsToStreamAsync(&output, &options)?
            .join()?;
        let size = output.Size()?;
        if size == 0 || size > (MAX_PIXELS as u64 * 4 + 64 * 1024) {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                0x80004005_u32 as i32,
            )));
        }
        let reader = DataReader::CreateDataReader(&output.GetInputStreamAt(0)?)?;
        let mut bmp = vec![0; size as usize];
        reader.LoadAsync(size as u32)?.join()?;
        reader.ReadBytes(&mut bmp)?;
        Ok(bmp)
    };
    let bmp = render_bmp().map_err(|_| invalid())?;
    let bitmap = image::load_from_memory_with_format(&bmp, ImageFormat::Bmp)
        .map_err(|_| invalid())?
        .into_rgba8();
    let (raster_width, raster_height) = bitmap.dimensions();
    let mut bgra = bitmap.into_raw();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Page {
        width: raster_width as i32,
        height: raster_height as i32,
        output_width: width.round().max(1.0) as i32,
        output_height: height.round().max(1.0) as i32,
        bgra,
    })
}

pub(super) unsafe fn draw(device: HDC, page: &Page, media: &Media) -> bool {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: page.width,
            biHeight: -page.height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        if StartPage(device) <= 0 {
            return false;
        }
        let rows = StretchDIBits(
            device,
            -media.offset_x,
            -media.offset_y,
            page.output_width,
            page.output_height,
            0,
            0,
            page.width,
            page.height,
            page.bgra.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        rows != 0 && rows != GDI_ERROR && EndPage(device) > 0
    }
}
