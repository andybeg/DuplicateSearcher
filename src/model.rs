use std::path::PathBuf;

use serde::Serialize;

use crate::formats::FormatKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileId {
    pub dev: u64,
    pub ino: u64,
}

/// Один обычный файл, увиденный при обходе.
#[derive(Clone, Debug)]
pub struct FileRec {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<i64>,
    pub ext: String,
    pub(crate) id: Option<FileId>,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<i64>,
}

/// Сводка по одному расширению внутри выбранной папки.
#[derive(Clone, Debug)]
pub struct FormatInfo {
    pub extension: String,
    pub kind: FormatKind,
    pub title: String,
    pub blurb: String,
    pub files: u64,
    pub bytes: u64,
    pub files_in_search: u64,
    pub bytes_in_search: u64,
    pub samples: Vec<Sample>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DupFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DupGroup {
    pub size: u64,
    pub copies: usize,
    pub reclaimable: u64,
    pub hash: String,
    pub files: Vec<DupFile>,
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

pub fn format_mtime(secs: Option<i64>) -> String {
    let Some(secs) = secs else {
        return "дата неизвестна".to_owned();
    };
    let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) else {
        return "дата неизвестна".to_owned();
    };
    dt.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}
