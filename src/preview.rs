use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    ptr::null,
};

use windows::{
    Win32::{
        Graphics::Imaging::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, CoCreateInstance,
            StructuredStorage::{PROPVARIANT, PropVariantClear, PropVariantToUInt32},
        },
    },
    core::{GUID, Interface, Result, w},
};

use crate::shell::Apartment;

pub const TEXT_LIMIT: usize = 128 * 1024;
const IMAGE_LIMIT: usize = 24 * 1024 * 1024;
const SOURCE_PIXELS: u64 = 16_000_000;
const IMAGE_EDGE: u32 = 1600;

pub enum Preview {
    Text { content: String, excerpt: bool },
    Image(Image),
    Details(String),
}

pub struct Image {
    pub width: u32,
    pub height: u32,
    pub original: (u32, u32),
    pub pixels: Vec<u8>,
}

pub fn load(path: &Path, current: impl Fn() -> bool) -> Option<Preview> {
    if !current() {
        return None;
    }
    let result = read_preview(path, &current);
    if !current() {
        return None;
    }
    Some(
        result
            .unwrap_or_else(|error| Preview::Details(format!("Could not read preview · {error}"))),
    )
}

pub fn load_archive(archive: &Path, member: &str, current: impl Fn() -> bool) -> Option<Preview> {
    if !current() {
        return None;
    }
    let result = read_archive_preview(archive, member, &current);
    if !current() {
        return None;
    }
    Some(
        result
            .unwrap_or_else(|error| Preview::Details(format!("Could not read preview · {error}"))),
    )
}

fn image_codec(bytes: &[u8]) -> Option<&'static GUID> {
    match bytes {
        [0xff, 0xd8, 0xff, ..] => Some(&CLSID_WICJpegDecoder),
        bytes if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some(&CLSID_WICPngDecoder),
        bytes if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => {
            Some(&CLSID_WICGifDecoder)
        }
        _ => None,
    }
}

fn read_archive_preview(
    archive: &Path,
    member: &str,
    current: &impl Fn() -> bool,
) -> io::Result<Preview> {
    let (mut bytes, size) = crate::archive::read_member(archive, member, TEXT_LIMIT + 1, current)?;
    if let Some(codec) = image_codec(&bytes) {
        if size > IMAGE_LIMIT as u64 {
            return Ok(Preview::Details(
                "Image exceeds the 24 MiB preview limit.".into(),
            ));
        }
        if (bytes.len() as u64) < size {
            let (full, _) = crate::archive::read_member(archive, member, IMAGE_LIMIT + 1, current)?;
            bytes = full;
        }
        if bytes.len() > IMAGE_LIMIT {
            return Ok(Preview::Details(
                "Image exceeds the 24 MiB preview limit.".into(),
            ));
        }
        if !current() {
            return Ok(Preview::Details(String::new()));
        }
        return Ok(match image(&bytes, codec) {
            Ok(Some(image)) => Preview::Image(image),
            Ok(None) => Preview::Details("Image dimensions exceed the preview limit.".into()),
            Err(error) => Preview::Details(format!("Could not decode image · {}", error.message())),
        });
    }
    let excerpt = bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);
    Ok(match text(&bytes, excerpt) {
        Some(content) => Preview::Text { content, excerpt },
        None => Preview::Details(
            "Extract this ZIP member with F5 to view it in its associated app.".into(),
        ),
    })
}

fn read_preview(path: &Path, current: &impl Fn() -> bool) -> io::Result<Preview> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    read_to(&mut file, &mut bytes, TEXT_LIMIT + 1, current)?;
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        && (bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06"))
    {
        drop(file);
        return match crate::archive::summarize(path, current) {
            Ok(content) => Ok(Preview::Text {
                content,
                excerpt: false,
            }),
            Err(error) => Ok(Preview::Details(format!(
                "Could not read ZIP directory · {error}"
            ))),
        };
    }
    if let Some(codec) = image_codec(&bytes) {
        if file.metadata()?.len() > IMAGE_LIMIT as u64 {
            return Ok(Preview::Details(
                "Image exceeds the 24 MiB preview limit.".into(),
            ));
        }
        read_to(&mut file, &mut bytes, IMAGE_LIMIT + 1, current)?;
        if bytes.len() > IMAGE_LIMIT {
            return Ok(Preview::Details(
                "Image exceeds the 24 MiB preview limit.".into(),
            ));
        }
        if !current() {
            return Ok(Preview::Details(String::new()));
        }
        return Ok(match image(&bytes, codec) {
            Ok(Some(image)) => Preview::Image(image),
            Ok(None) => Preview::Details("Image dimensions exceed the preview limit.".into()),
            Err(error) => Preview::Details(format!("Could not decode image · {}", error.message())),
        });
    }
    let excerpt = bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);
    Ok(match text(&bytes, excerpt) {
        Some(content) => Preview::Text { content, excerpt },
        None => Preview::Details("Use Open to view this file in its associated app.".into()),
    })
}

fn read_to(
    file: &mut File,
    bytes: &mut Vec<u8>,
    limit: usize,
    current: &impl Fn() -> bool,
) -> io::Result<()> {
    let mut buffer = [0; 32 * 1024];
    while bytes.len() < limit && current() {
        let len = (limit - bytes.len()).min(buffer.len());
        match file.read(&mut buffer[..len]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn text(bytes: &[u8], excerpt: bool) -> Option<String> {
    let decoded = if let [0xff, 0xfe, rest @ ..] | [0xfe, 0xff, rest @ ..] = bytes {
        if !excerpt && !rest.len().is_multiple_of(2) {
            return None;
        }
        let little = bytes[0] == 0xff;
        let mut units = rest
            .chunks_exact(2)
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        if excerpt
            && units
                .last()
                .is_some_and(|last| (0xd800..=0xdbff).contains(last))
        {
            units.pop();
        }
        String::from_utf16(&units).ok()?
    } else {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(value) => value.to_owned(),
            Err(error) if excerpt && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .ok()?
                    .to_owned()
            }
            Err(_) => return None,
        }
    };
    let mut content = String::with_capacity(decoded.len());
    let mut previous = '\0';
    for character in decoded.chars() {
        match character {
            '\r' => content.push_str("\r\n"),
            '\n' if previous == '\r' => {}
            '\n' => content.push_str("\r\n"),
            '\t' => content.push(character),
            character if !character.is_control() => content.push(character),
            _ => return None,
        }
        previous = character;
    }
    Some(content)
}

pub fn fit(width: u32, height: u32, available: (u32, u32)) -> (u32, u32) {
    let width_limit = u64::from(width.min(available.0));
    let height_limit = u64::from(height.min(available.1));
    let (width, height) = (u64::from(width), u64::from(height));
    if width_limit * height <= height_limit * width {
        (
            width_limit as u32,
            (width_limit * height / width).max(1) as u32,
        )
    } else {
        (
            (height_limit * width / height).max(1) as u32,
            height_limit as u32,
        )
    }
}

fn image(bytes: &[u8], codec: &GUID) -> Result<Option<Image>> {
    let _apartment = Apartment::enter()?;
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        // The stream and decoder are dropped before the borrowed byte buffer.
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder: IWICBitmapDecoder = CoCreateInstance(codec, None, CLSCTX_INPROC_SERVER)?;
        decoder.Initialize(&stream, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let mut original = (0, 0);
        frame.GetSize(&mut original.0, &mut original.1)?;
        if original.0 == 0 || original.1 == 0 {
            return Ok(None);
        }
        let dimensions = fit(original.0, original.1, (IMAGE_EDGE, IMAGE_EDGE));
        let source: IWICBitmapSource = frame.cast()?;
        let source = if original != dimensions
            && let Ok(scaled) = scaled_decode(&factory, &frame, dimensions, original)
        {
            scaled
        } else if u64::from(original.0) * u64::from(original.1) <= SOURCE_PIXELS {
            source
        } else {
            return Ok(None);
        };
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(
            &source,
            dimensions.0,
            dimensions.1,
            WICBitmapInterpolationModeFant,
        )?;
        let orientation = if *codec == CLSID_WICJpegDecoder {
            jpeg_orientation(&frame).unwrap_or(1)
        } else {
            1
        };
        let transform = match orientation {
            2 => WICBitmapTransformFlipHorizontal,
            3 => WICBitmapTransformRotate180,
            4 => WICBitmapTransformFlipVertical,
            5 => WICBitmapTransformOptions(
                WICBitmapTransformRotate90.0 | WICBitmapTransformFlipVertical.0,
            ),
            6 => WICBitmapTransformRotate90,
            7 => WICBitmapTransformOptions(
                WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
            ),
            8 => WICBitmapTransformRotate270,
            _ => WICBitmapTransformRotate0,
        };
        let source: IWICBitmapSource = if transform != WICBitmapTransformRotate0 {
            let rotate = factory.CreateBitmapFlipRotator()?;
            rotate.Initialize(&scaler, transform)?;
            rotate.cast()?
        } else {
            scaler.cast()?
        };
        let mut dimensions = (0, 0);
        source.GetSize(&mut dimensions.0, &mut dimensions.1)?;
        if (5..=8).contains(&orientation) {
            original = (original.1, original.0);
        }
        let convert = factory.CreateFormatConverter()?;
        convert.Initialize(
            &source,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let stride = dimensions.0 * 4;
        let mut pixels = vec![0; (stride * dimensions.1) as usize];
        convert.CopyPixels(null(), stride, &mut pixels)?;
        for (row, line) in pixels.chunks_exact_mut(stride as usize).enumerate() {
            for (column, pixel) in line.chunks_exact_mut(4).enumerate() {
                let alpha = u32::from(pixel[3]);
                if alpha < 255 {
                    let shade = if (row / 12 + column / 12).is_multiple_of(2) {
                        218
                    } else {
                        176
                    };
                    for value in &mut pixel[..3] {
                        *value = ((u32::from(*value) * alpha + shade * (255 - alpha)) / 255) as u8;
                    }
                }
            }
        }
        Ok(Some(Image {
            width: dimensions.0,
            height: dimensions.1,
            original,
            pixels,
        }))
    }
}

fn jpeg_orientation(frame: &IWICBitmapFrameDecode) -> Result<u32> {
    unsafe {
        let reader = frame.GetMetadataQueryReader()?;
        let mut value = PROPVARIANT::default();
        let result = reader
            .GetMetadataByName(w!("/app1/ifd/{ushort=274}"), &mut value)
            .and_then(|()| PropVariantToUInt32(&value));
        let _ = PropVariantClear(&mut value);
        result
    }
}

fn scaled_decode(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    (mut width, mut height): (u32, u32),
    original: (u32, u32),
) -> Result<IWICBitmapSource> {
    unsafe {
        let transform: IWICBitmapSourceTransform = frame.cast()?;
        transform.GetClosestSize(&mut width, &mut height)?;
        if width == 0
            || height == 0
            || (width, height) == original
            || u64::from(width) * u64::from(height) > 6_000_000
        {
            return Err(windows::Win32::Foundation::E_FAIL.into());
        }
        let mut format = GUID_WICPixelFormat24bppBGR;
        transform.GetClosestPixelFormat(&mut format)?;
        let info: IWICPixelFormatInfo = factory.CreateComponentInfo(&format)?.cast()?;
        let bits = info.GetBitsPerPixel()?;
        let stride = u64::from(width) * u64::from(bits);
        let stride = stride.div_ceil(8);
        let size = stride * u64::from(height);
        if size == 0 || size > u64::from(IMAGE_LIMIT as u32) {
            return Err(windows::Win32::Foundation::E_FAIL.into());
        }
        let bitmap = factory.CreateBitmap(width, height, &format, WICBitmapCacheOnLoad)?;
        let lock = bitmap.Lock(null(), WICBitmapLockWrite.0 as u32)?;
        let stride = lock.GetStride()?;
        let mut length = 0;
        let mut data = std::ptr::null_mut();
        lock.GetDataPointer(&mut length, &mut data)?;
        if data.is_null() || length == 0 || length as usize > IMAGE_LIMIT {
            return Err(windows::Win32::Foundation::E_FAIL.into());
        }
        // The write lock keeps this buffer valid and exclusive until it is dropped.
        let pixels = std::slice::from_raw_parts_mut(data, length as usize);
        transform.CopyPixels(
            null(),
            width,
            height,
            &format,
            WICBitmapTransformRotate0,
            stride,
            pixels,
        )?;
        drop(lock);
        bitmap.cast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_preserves_unicode_at_the_excerpt_boundary() {
        assert_eq!(
            text(b"good\r\nline\nlast\r", false).as_deref(),
            Some("good\r\nline\r\nlast\r\n")
        );
        assert_eq!(text(b"ok\xf0\x9f", true).as_deref(), Some("ok"));
        assert_eq!(text(b"\xff\xfeX\x00\x3d\xd8", true).as_deref(), Some("X"));
        assert!(text(b"abc\0binary", false).is_none());
        assert!(text(b"ok\xf0\x9f", false).is_none());
    }

    #[test]
    fn preview_fit_keeps_aspect_and_avoids_upscaling() {
        assert_eq!(fit(8000, 6000, (1600, 1600)), (1600, 1200));
        assert_eq!(fit(100, 500, (200, 200)), (40, 200));
        assert_eq!(fit(100, 80, (200, 200)), (100, 80));
    }
}
