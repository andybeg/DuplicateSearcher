use std::ffi::OsStr;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKind {
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Text,
    Other,
}

pub fn extension_of(path: &Path) -> String {
    match path.extension().and_then(OsStr::to_str) {
        Some(ext) if !ext.is_empty() => format!(".{}", ext.to_lowercase()),
        _ => "(без расширения)".to_owned(),
    }
}

pub fn classify(ext: &str) -> (FormatKind, String, String) {
    if ext.chars().any(|c| c.is_ascii_uppercase()) {
        return classify(&ext.to_ascii_lowercase());
    }
    let (kind, title, blurb) = match ext {
        ".jpg" | ".jpeg" | ".jpe" | ".jfif" => (FormatKind::Image, "JPEG", "Фотография JPEG"),
        ".png" => (FormatKind::Image, "PNG", "Изображение PNG"),
        ".gif" => (FormatKind::Image, "GIF", "Изображение или анимация GIF"),
        ".webp" => (FormatKind::Image, "WebP", "Изображение WebP"),
        ".bmp" => (FormatKind::Image, "BMP", "Изображение BMP"),
        ".tif" | ".tiff" => (FormatKind::Image, "TIFF", "Изображение TIFF"),
        ".ico" => (FormatKind::Image, "ICO", "Значок"),
        ".heic" | ".heif" => (FormatKind::Image, "HEIC", "Фотография HEIC"),
        ".avif" => (FormatKind::Image, "AVIF", "Изображение AVIF"),
        ".raw" | ".cr2" | ".nef" | ".arw" | ".dng" => {
            (FormatKind::Image, "RAW", "Сырое фото с камеры")
        }
        ".mp4" | ".m4v" => (FormatKind::Video, "MP4", "Видео MP4"),
        ".mov" => (FormatKind::Video, "MOV", "Видео QuickTime"),
        ".mkv" => (FormatKind::Video, "MKV", "Видео Matroska"),
        ".avi" => (FormatKind::Video, "AVI", "Видео AVI"),
        ".webm" => (FormatKind::Video, "WebM", "Видео WebM"),
        ".wmv" => (FormatKind::Video, "WMV", "Видео Windows Media"),
        ".mpg" | ".mpeg" => (FormatKind::Video, "MPEG", "Видео MPEG"),
        ".flv" => (FormatKind::Video, "FLV", "Видео Flash"),
        ".3gp" => (FormatKind::Video, "3GP", "Видео 3GP"),
        ".mp3" => (FormatKind::Audio, "MP3", "Аудио MP3"),
        ".wav" => (FormatKind::Audio, "WAV", "Аудио WAV"),
        ".flac" => (FormatKind::Audio, "FLAC", "Аудио FLAC"),
        ".aac" => (FormatKind::Audio, "AAC", "Аудио AAC"),
        ".m4a" => (FormatKind::Audio, "M4A", "Аудио MPEG-4"),
        ".ogg" | ".oga" => (FormatKind::Audio, "Ogg", "Аудио Ogg"),
        ".opus" => (FormatKind::Audio, "Opus", "Аудио Opus"),
        ".wma" => (FormatKind::Audio, "WMA", "Аудио Windows Media"),
        ".aiff" | ".aif" => (FormatKind::Audio, "AIFF", "Аудио AIFF"),
        ".pdf" => (FormatKind::Document, "PDF", "Документ PDF"),
        ".doc" | ".docx" => (FormatKind::Document, "Word", "Документ Word"),
        ".xls" | ".xlsx" => (FormatKind::Document, "Excel", "Таблица Excel"),
        ".ppt" | ".pptx" => (FormatKind::Document, "PowerPoint", "Презентация"),
        ".odt" | ".ods" | ".odp" => (
            FormatKind::Document,
            "OpenDocument",
            "Документ OpenDocument",
        ),
        ".rtf" => (FormatKind::Document, "RTF", "Текст RTF"),
        ".pages" => (FormatKind::Document, "Pages", "Документ Pages"),
        ".numbers" => (FormatKind::Document, "Numbers", "Таблица Numbers"),
        ".key" => (FormatKind::Document, "Keynote", "Презентация Keynote"),
        ".zip" => (FormatKind::Archive, "ZIP", "Архив ZIP"),
        ".7z" => (FormatKind::Archive, "7-Zip", "Архив 7-Zip"),
        ".rar" => (FormatKind::Archive, "RAR", "Архив RAR"),
        ".tar" => (FormatKind::Archive, "tar", "Архив tar"),
        ".gz" | ".tgz" => (FormatKind::Archive, "gzip", "Сжатый файл gzip"),
        ".bz2" => (FormatKind::Archive, "bzip2", "Сжатый файл bzip2"),
        ".xz" => (FormatKind::Archive, "xz", "Сжатый файл xz"),
        ".iso" => (FormatKind::Archive, "ISO", "Образ диска ISO"),
        ".dmg" => (FormatKind::Archive, "DMG", "Образ диска macOS"),
        ".txt" | ".text" | ".log" => (FormatKind::Text, "Текст", "Текстовый файл"),
        ".md" | ".markdown" => (FormatKind::Text, "Markdown", "Текст Markdown"),
        ".json" => (FormatKind::Text, "JSON", "Данные JSON"),
        ".xml" => (FormatKind::Text, "XML", "Разметка XML"),
        ".html" | ".htm" => (FormatKind::Text, "HTML", "Страница HTML"),
        ".css" => (FormatKind::Text, "CSS", "Таблица стилей"),
        ".js" | ".mjs" | ".cjs" => (FormatKind::Text, "JavaScript", "Исходник JavaScript"),
        ".ts" | ".tsx" | ".jsx" => (FormatKind::Text, "TypeScript", "Исходник TypeScript"),
        ".rs" => (FormatKind::Text, "Rust", "Исходник Rust"),
        ".py" => (FormatKind::Text, "Python", "Исходник Python"),
        ".go" => (FormatKind::Text, "Go", "Исходник Go"),
        ".java" => (FormatKind::Text, "Java", "Исходник Java"),
        ".c" | ".h" => (FormatKind::Text, "C", "Исходник C"),
        ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" => (FormatKind::Text, "C++", "Исходник C++"),
        ".cs" => (FormatKind::Text, "C#", "Исходник C#"),
        ".php" => (FormatKind::Text, "PHP", "Исходник PHP"),
        ".rb" => (FormatKind::Text, "Ruby", "Исходник Ruby"),
        ".sh" | ".bash" | ".zsh" => (FormatKind::Text, "Скрипт", "Скрипт оболочки"),
        ".sql" => (FormatKind::Text, "SQL", "Запрос SQL"),
        ".yaml" | ".yml" => (FormatKind::Text, "YAML", "Данные YAML"),
        ".toml" => (FormatKind::Text, "TOML", "Настройки TOML"),
        ".ini" | ".cfg" | ".conf" => (FormatKind::Text, "Настройки", "Файл настроек"),
        ".csv" | ".tsv" => (FormatKind::Text, "Таблица", "Таблица в тексте"),
        ".svg" => (FormatKind::Text, "SVG", "Векторный рисунок в тексте"),
        "(без расширения)" => {
            (FormatKind::Other, "Без расширения", "Файл без расширения")
        }
        _ => {
            let name = ext.trim_start_matches('.').to_uppercase();
            let title: String = if name.is_empty() {
                "Файл".to_owned()
            } else {
                name
            };
            return (
                FormatKind::Other,
                title,
                "Отдельного предпросмотра для этого формата нет".to_owned(),
            );
        }
    };
    (kind, title.to_owned(), blurb.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_extensions() {
        assert_eq!(classify(".JPEG").0, FormatKind::Image);
        assert_eq!(classify(".jpg").0, FormatKind::Image);
        assert_eq!(classify(".mp4").1, "MP4");
        assert_eq!(classify(".rs").0, FormatKind::Text);
        assert_eq!(extension_of(Path::new("Photo.JPG")), ".jpg");
        assert_eq!(extension_of(Path::new("README")), "(без расширения)");
    }
}
