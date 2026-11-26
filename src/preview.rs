use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::formats::FormatKind;

#[derive(Clone, Debug)]
pub enum Preview {
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Text(String),
    Unavailable(String),
}

pub fn load_preview(path: &Path, kind: FormatKind, max_px: u32) -> Preview {
    match kind {
        FormatKind::Image => match load_image(path, max_px.max(1)) {
            Ok((width, height, rgba)) => Preview::Image {
                width,
                height,
                rgba,
            },
            Err(err) => Preview::Unavailable(err),
        },
        FormatKind::Text => match load_text(path) {
            Ok(text) => Preview::Text(text),
            Err(err) => Preview::Unavailable(err),
        },
        _ => Preview::Unavailable(
            "Для этого формата показаны имя, размер и дата. Картинку и текст программа читает у фото и текстовых файлов.".to_owned(),
        ),
    }
}

fn load_image(path: &Path, max_px: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height) = image_size(path)?;
    if width == 0 || height == 0 {
        return Err("пустое изображение".to_owned());
    }
    if u64::from(width) * u64::from(height) > 40_000_000 {
        return Err("изображение слишком большое для предпросмотра".to_owned());
    }
    let reader = image::ImageReader::open(path)
        .map_err(|err| err.to_string())?
        .with_guessed_format()
        .map_err(|err| err.to_string())?;
    let decoded = reader.decode().map_err(|err| err.to_string())?;
    let scaled = if width > max_px || height > max_px {
        decoded.thumbnail(max_px, max_px)
    } else {
        decoded
    };
    let rgba = scaled.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

fn image_size(path: &Path) -> Result<(u32, u32), String> {
    let reader = image::ImageReader::open(path)
        .map_err(|err| err.to_string())?
        .with_guessed_format()
        .map_err(|err| err.to_string())?;
    reader.into_dimensions().map_err(|err| err.to_string())
}

fn load_text(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut buf = vec![0u8; 8192];
    let n = file.read(&mut buf).map_err(|err| err.to_string())?;
    buf.truncate(n);
    if buf.contains(&0) {
        return Err("внутри файла есть двоичные данные".to_owned());
    }
    let text = std::str::from_utf8(&buf).map_err(|_| "файл не в кодировке UTF-8".to_owned())?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok("Файл пустой.".to_owned());
    }
    let mut short: String = trimmed.chars().take(2500).collect();
    if trimmed.chars().count() > 2500 {
        short.push_str("\n…");
    }
    Ok(short)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn previews_png_and_utf8_text() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("dot.png");
        let image: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(8, 6, Rgb([9, 8, 7]));
        image.save(&image_path).unwrap();
        match load_preview(&image_path, FormatKind::Image, 32) {
            Preview::Image {
                width,
                height,
                rgba,
            } => {
                assert_eq!((width, height), (8, 6));
                assert_eq!(rgba.len(), 8 * 6 * 4);
            }
            other => panic!("ожидалась картинка, получили {other:?}"),
        }

        let text_path = dir.path().join("note.txt");
        std::fs::write(&text_path, "привет, дубль").unwrap();
        match load_preview(&text_path, FormatKind::Text, 32) {
            Preview::Text(text) => assert!(text.contains("привет")),
            other => panic!("ожидался текст, получили {other:?}"),
        }
    }

    #[test]
    fn rejects_binary_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [b'a', 0, b'b']).unwrap();
        match load_preview(&path, FormatKind::Text, 32) {
            Preview::Unavailable(_) => {}
            other => panic!("ожидался отказ, получили {other:?}"),
        }
    }
}
