use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::formats::{classify, extension_of};
use crate::model::{DupFile, DupGroup, FileId, FileRec, FormatInfo, Sample};

const SAMPLE: u64 = 64 * 1024;
const READ_CHUNK: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub skip_junk: bool,
    pub follow_symlinks: bool,
    pub one_file_system: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            skip_junk: true,
            follow_symlinks: false,
            one_file_system: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub min_size: u64,
    pub extensions: HashSet<String>,
    pub full_hash_below: u64,
    /// Одинаковые байты файла. Включено по умолчанию.
    pub by_content: bool,
    /// Одинаковый размер. При включённом содержимом размер и так совпадает.
    pub by_size: bool,
    /// Одинаковое имя файла, без учёта папки.
    pub by_name: bool,
    /// Одинаковая дата изменения, с точностью до секунды.
    pub by_mtime: bool,
    /// Пустые файлы не считать дублями.
    pub skip_empty: bool,
    /// Несколько имён одного файла не считать лишними копиями.
    pub collapse_hardlinks: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            min_size: 1,
            extensions: HashSet::new(),
            full_hash_below: 1024 * 1024,
            by_content: true,
            by_size: true,
            by_name: false,
            by_mtime: false,
            skip_empty: true,
            collapse_hardlinks: true,
        }
    }
}

impl SearchOptions {
    pub fn has_positive_rule(&self) -> bool {
        self.by_content || self.by_size || self.by_name || self.by_mtime
    }
}

pub struct Progress {
    pub files_seen: AtomicU64,
    pub bytes_seen: AtomicU64,
    pub hashed_files: AtomicU64,
    pub hash_total: AtomicU64,
    pub bytes_read: AtomicU64,
    pub cancel: AtomicBool,
    errors: Mutex<ErrorLog>,
}

struct ErrorLog {
    count: u64,
    samples: Vec<String>,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            files_seen: AtomicU64::new(0),
            bytes_seen: AtomicU64::new(0),
            hashed_files: AtomicU64::new(0),
            hash_total: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            errors: Mutex::new(ErrorLog {
                count: 0,
                samples: Vec::new(),
            }),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn error_count(&self) -> u64 {
        self.errors.lock().map(|log| log.count).unwrap_or(0)
    }

    pub fn error_samples(&self) -> Vec<String> {
        self.errors
            .lock()
            .map(|log| log.samples.clone())
            .unwrap_or_default()
    }

    fn note_error(&self, message: impl Into<String>) {
        let Ok(mut log) = self.errors.lock() else {
            return;
        };
        log.count += 1;
        if log.samples.len() < 30 {
            log.samples.push(message.into());
        }
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            files_seen: AtomicU64::new(0),
            bytes_seen: AtomicU64::new(0),
            hashed_files: AtomicU64::new(0),
            hash_total: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            errors: Mutex::new(ErrorLog {
                count: 0,
                samples: Vec::new(),
            }),
        }
    }
}

#[derive(Debug)]
pub enum ScanErr {
    Cancelled,
    NotDirectory(PathBuf),
    Io(io::Error),
    Pool(String),
}

impl std::fmt::Display for ScanErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "Остановлено"),
            Self::NotDirectory(path) => write!(f, "Это не папка: {}", path.display()),
            Self::Io(err) => write!(f, "{err}"),
            Self::Pool(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ScanErr {}

pub struct Inventory {
    pub root: PathBuf,
    pub files: Arc<Vec<FileRec>>,
    pub formats: Vec<FormatInfo>,
    pub elapsed: Duration,
    pub symlinks_skipped: u64,
    pub junk_skipped: u64,
    pub empty_files: u64,
    pub unreadable: u64,
    pub error_samples: Vec<String>,
}

pub struct SearchOutcome {
    pub groups: Vec<DupGroup>,
    pub files_considered: u64,
    pub bytes_considered: u64,
    pub hardlinks_collapsed: u64,
    pub unreadable: u64,
    pub error_samples: Vec<String>,
    pub elapsed: Duration,
}

pub fn inventory(
    root: &Path,
    opts: &ScanOptions,
    progress: &Progress,
) -> Result<Inventory, ScanErr> {
    let started = Instant::now();
    let meta = fs::metadata(root).map_err(ScanErr::Io)?;
    if !meta.is_dir() {
        return Err(ScanErr::NotDirectory(root.to_path_buf()));
    }
    let root_dev = device_id(&meta);
    let mut junk_dirs = 0u64;
    let mut other_disks = 0u64;
    let mut seen_dirs = HashSet::new();

    let walker = WalkDir::new(root)
        .follow_links(opts.follow_symlinks)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if !entry.file_type().is_dir() {
                return true;
            }
            if opts.skip_junk && is_junk_dir(entry.file_name()) {
                junk_dirs += 1;
                return false;
            }
            if let Ok(meta) = entry.metadata() {
                if opts.one_file_system && device_id(&meta) != root_dev {
                    other_disks += 1;
                    return false;
                }
                if opts.follow_symlinks {
                    if let Some(id) = file_id(&meta) {
                        if !seen_dirs.insert(id) {
                            return false;
                        }
                    }
                }
            }
            true
        });

    let mut files = Vec::new();
    let mut symlinks_skipped = 0u64;
    let mut junk_files = 0u64;
    let mut empty_files = 0u64;

    for entry in walker {
        if progress.is_cancelled() {
            return Err(ScanErr::Cancelled);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                progress.note_error(err.to_string());
                continue;
            }
        };
        let file_type = entry.file_type();
        if file_type.is_symlink() && !opts.follow_symlinks {
            symlinks_skipped += 1;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if opts.skip_junk && is_junk_file(entry.file_name()) {
            junk_files += 1;
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                progress.note_error(format!("{}: {err}", entry.path().display()));
                continue;
            }
        };
        let size = meta.len();
        progress.files_seen.fetch_add(1, Ordering::Relaxed);
        progress.bytes_seen.fetch_add(size, Ordering::Relaxed);
        if size == 0 {
            empty_files += 1;
        }
        let modified = meta.modified().ok().and_then(unix_time);
        files.push(FileRec {
            ext: extension_of(entry.path()),
            path: entry.path().to_path_buf(),
            size,
            modified,
            id: file_id(&meta),
        });
    }

    let files = Arc::new(files);
    let formats = summarize(&files, 1);
    Ok(Inventory {
        root: root.to_path_buf(),
        files,
        formats,
        elapsed: started.elapsed(),
        symlinks_skipped,
        junk_skipped: junk_dirs + junk_files + other_disks,
        empty_files,
        unreadable: progress.error_count(),
        error_samples: progress.error_samples(),
    })
}

pub fn summarize(files: &[FileRec], min_size: u64) -> Vec<FormatInfo> {
    struct Acc {
        files: u64,
        bytes: u64,
        in_search_files: u64,
        in_search_bytes: u64,
        samples: Vec<Sample>,
        fallback: Vec<Sample>,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for file in files {
        let acc = map.entry(file.ext.clone()).or_insert_with(|| Acc {
            files: 0,
            bytes: 0,
            in_search_files: 0,
            in_search_bytes: 0,
            samples: Vec::new(),
            fallback: Vec::new(),
        });
        acc.files += 1;
        acc.bytes += file.size;
        let sample = Sample {
            path: file.path.clone(),
            size: file.size,
            modified: file.modified,
        };
        if file.size >= min_size {
            acc.in_search_files += 1;
            acc.in_search_bytes += file.size;
            if acc.samples.len() < 4 {
                acc.samples.push(sample);
            }
        } else if acc.fallback.len() < 4 {
            acc.fallback.push(sample);
        }
    }
    let mut formats: Vec<FormatInfo> = map
        .into_iter()
        .map(|(extension, mut acc)| {
            if acc.samples.is_empty() {
                acc.samples = acc.fallback;
            }
            let (kind, title, blurb) = classify(&extension);
            FormatInfo {
                extension,
                kind,
                title,
                blurb,
                files: acc.files,
                bytes: acc.bytes,
                files_in_search: acc.in_search_files,
                bytes_in_search: acc.in_search_bytes,
                samples: acc.samples,
            }
        })
        .collect();
    formats.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.title.cmp(&b.title)));
    formats
}

pub fn search(
    files: &[FileRec],
    opts: &SearchOptions,
    progress: &Progress,
) -> Result<SearchOutcome, ScanErr> {
    let started = Instant::now();
    let mut chosen = Vec::new();
    for (idx, file) in files.iter().enumerate() {
        if included_in_search(file, opts) {
            chosen.push(idx);
        }
    }

    let mut seen_links = HashSet::new();
    let mut hardlinks_collapsed = 0u64;
    let mut bytes_considered = 0u64;
    let mut unique = Vec::with_capacity(chosen.len());
    for idx in chosen {
        if opts.collapse_hardlinks {
            if let Some(id) = files[idx].id {
                if !seen_links.insert(id) {
                    hardlinks_collapsed += 1;
                    continue;
                }
            }
        }
        bytes_considered += files[idx].size;
        unique.push(idx);
    }
    let files_considered = unique.len() as u64;

    if !opts.has_positive_rule() {
        return Ok(SearchOutcome {
            groups: Vec::new(),
            files_considered,
            bytes_considered,
            hardlinks_collapsed,
            unreadable: progress.error_count(),
            error_samples: progress.error_samples(),
            elapsed: started.elapsed(),
        });
    }

    if !opts.by_content {
        let mut groups = loose_groups(files, &unique, opts);
        sort_groups(&mut groups);
        return Ok(SearchOutcome {
            groups,
            files_considered,
            bytes_considered,
            hardlinks_collapsed,
            unreadable: progress.error_count(),
            error_samples: progress.error_samples(),
            elapsed: started.elapsed(),
        });
    }

    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for idx in unique {
        by_size.entry(files[idx].size).or_default().push(idx);
    }
    let candidates: Vec<usize> = by_size
        .into_values()
        .filter(|group| group.len() > 1)
        .flatten()
        .collect();
    progress
        .hash_total
        .store(candidates.len() as u64, Ordering::Relaxed);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count())
        .build()
        .map_err(|err| ScanErr::Pool(err.to_string()))?;

    let fingerprinted = pool.install(|| {
        candidates
            .par_iter()
            .copied()
            .map(|idx| fingerprint_indexed(files, idx, opts.full_hash_below, progress))
            .collect::<Vec<_>>()
    });
    if progress.is_cancelled() {
        return Err(ScanErr::Cancelled);
    }

    let mut full_groups: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    let mut partial_groups: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    for item in fingerprinted.into_iter().flatten() {
        let size = files[item.idx].size;
        match item.kind {
            HashKind::Full => full_groups
                .entry((size, item.hash))
                .or_default()
                .push(item.idx),
            HashKind::Partial => partial_groups
                .entry((size, item.hash))
                .or_default()
                .push(item.idx),
        }
    }

    let need_full: Vec<usize> = partial_groups
        .into_values()
        .filter(|group| group.len() > 1)
        .flatten()
        .collect();
    progress
        .hash_total
        .fetch_add(need_full.len() as u64, Ordering::Relaxed);
    let confirmed = pool.install(|| {
        need_full
            .par_iter()
            .copied()
            .map(|idx| full_hash_indexed(files, idx, progress))
            .collect::<Vec<_>>()
    });
    if progress.is_cancelled() {
        return Err(ScanErr::Cancelled);
    }
    for item in confirmed.into_iter().flatten() {
        let size = files[item.idx].size;
        full_groups
            .entry((size, item.hash))
            .or_default()
            .push(item.idx);
    }

    let mut groups = Vec::new();
    for ((_size, hash), idxs) in full_groups {
        groups.extend(materialize(files, idxs, Some(hash), opts));
    }
    sort_groups(&mut groups);

    Ok(SearchOutcome {
        groups,
        files_considered,
        bytes_considered,
        hardlinks_collapsed,
        unreadable: progress.error_count(),
        error_samples: progress.error_samples(),
        elapsed: started.elapsed(),
    })
}

fn included_in_search(file: &FileRec, opts: &SearchOptions) -> bool {
    if !opts.extensions.contains(&file.ext) {
        return false;
    }
    if file.size == 0 {
        return !opts.skip_empty;
    }
    file.size >= opts.min_size
}

#[derive(Hash, PartialEq, Eq)]
struct NameDateKey {
    name: Option<String>,
    modified: Option<Option<i64>>,
}

#[derive(Hash, PartialEq, Eq)]
struct LooseKey {
    size: Option<u64>,
    name: Option<String>,
    modified: Option<Option<i64>>,
}

fn loose_groups(files: &[FileRec], indexes: &[usize], opts: &SearchOptions) -> Vec<DupGroup> {
    let mut buckets: HashMap<LooseKey, Vec<usize>> = HashMap::new();
    for &idx in indexes {
        let key = LooseKey {
            size: opts.by_size.then_some(files[idx].size),
            name: opts.by_name.then(|| file_label(&files[idx].path)),
            modified: opts.by_mtime.then_some(files[idx].modified),
        };
        buckets.entry(key).or_default().push(idx);
    }
    buckets
        .into_values()
        .flat_map(|idxs| materialize(files, idxs, None, opts))
        .collect()
}

fn materialize(
    files: &[FileRec],
    indexes: Vec<usize>,
    hash: Option<[u8; 32]>,
    opts: &SearchOptions,
) -> Vec<DupGroup> {
    split_name_and_date(files, indexes, opts)
        .into_iter()
        .filter(|idxs| idxs.len() > 1)
        .map(|idxs| {
            let mut copies: Vec<DupFile> = idxs
                .into_iter()
                .map(|idx| DupFile {
                    path: files[idx].path.clone(),
                    size: files[idx].size,
                    modified: files[idx].modified,
                })
                .collect();
            copies.sort_by(|a, b| {
                a.modified
                    .cmp(&b.modified)
                    .then_with(|| a.path.cmp(&b.path))
            });
            let size = copies[0].size;
            let copies_len = copies.len();
            DupGroup {
                size,
                copies: copies_len,
                reclaimable: reclaimable_bytes(&copies),
                hash: hash.map(|bytes| to_hex(&bytes)).unwrap_or_default(),
                files: copies,
            }
        })
        .collect()
}

fn split_name_and_date(
    files: &[FileRec],
    indexes: Vec<usize>,
    opts: &SearchOptions,
) -> Vec<Vec<usize>> {
    if !opts.by_name && !opts.by_mtime {
        return vec![indexes];
    }
    let mut buckets: HashMap<NameDateKey, Vec<usize>> = HashMap::new();
    for idx in indexes {
        let key = NameDateKey {
            name: opts.by_name.then(|| file_label(&files[idx].path)),
            modified: opts.by_mtime.then_some(files[idx].modified),
        };
        buckets.entry(key).or_default().push(idx);
    }
    buckets.into_values().collect()
}

fn reclaimable_bytes(files: &[DupFile]) -> u64 {
    let total: u64 = files.iter().map(|file| file.size).sum();
    let largest = files.iter().map(|file| file.size).max().unwrap_or(0);
    total.saturating_sub(largest)
}

fn sort_groups(groups: &mut [DupGroup]) {
    groups.sort_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then_with(|| a.files[0].path.cmp(&b.files[0].path))
    });
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

struct Fingerprint {
    idx: usize,
    kind: HashKind,
    hash: [u8; 32],
}

#[derive(Clone, Copy)]
enum HashKind {
    Full,
    Partial,
}

fn fingerprint_indexed(
    files: &[FileRec],
    idx: usize,
    full_below: u64,
    progress: &Progress,
) -> Option<Fingerprint> {
    if progress.is_cancelled() {
        return None;
    }
    let file = &files[idx];
    match fingerprint(&file.path, file.size, full_below, progress) {
        Ok((kind, hash)) => {
            progress.hashed_files.fetch_add(1, Ordering::Relaxed);
            Some(Fingerprint { idx, kind, hash })
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => None,
        Err(err) => {
            progress.note_error(format!("{}: {err}", file.path.display()));
            progress.hashed_files.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

fn full_hash_indexed(files: &[FileRec], idx: usize, progress: &Progress) -> Option<Fingerprint> {
    if progress.is_cancelled() {
        return None;
    }
    let file = &files[idx];
    match hash_file(&file.path, Some(file.size), progress) {
        Ok(hash) => {
            progress.hashed_files.fetch_add(1, Ordering::Relaxed);
            Some(Fingerprint {
                idx,
                kind: HashKind::Full,
                hash,
            })
        }
        Err(err) if err.kind() == io::ErrorKind::Interrupted => None,
        Err(err) => {
            progress.note_error(format!("{}: {err}", file.path.display()));
            progress.hashed_files.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

fn fingerprint(
    path: &Path,
    size: u64,
    full_below: u64,
    progress: &Progress,
) -> io::Result<(HashKind, [u8; 32])> {
    if size <= full_below {
        return hash_file(path, Some(size), progress).map(|hash| (HashKind::Full, hash));
    }
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"partial-v1");
    hasher.update(&size.to_le_bytes());
    hash_window(&mut file, &mut hasher, 0, SAMPLE, progress)?;
    if size > SAMPLE.saturating_mul(3) {
        hash_window(&mut file, &mut hasher, size / 2, SAMPLE, progress)?;
    }
    hash_window(
        &mut file,
        &mut hasher,
        size.saturating_sub(SAMPLE),
        SAMPLE,
        progress,
    )?;
    Ok((HashKind::Partial, *hasher.finalize().as_bytes()))
}

fn hash_file(path: &Path, expected: Option<u64>, progress: &Progress) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut read_total = 0u64;
    loop {
        if progress.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "остановлено"));
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        read_total += n as u64;
        progress.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
    }
    if let Some(expected) = expected {
        if read_total != expected {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "размер файла изменился во время чтения",
            ));
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn hash_window(
    file: &mut File,
    hasher: &mut blake3::Hasher,
    start: u64,
    len: u64,
    progress: &Progress,
) -> io::Result<()> {
    if progress.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "остановлено"));
    }
    file.seek(SeekFrom::Start(start))?;
    let mut remaining = len;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
        progress.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

fn to_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
}

fn unix_time(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(err) => i64::try_from(err.duration().as_secs())
            .ok()
            .map(|secs| -secs),
    }
}

fn device_id(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.dev()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        u64::from(meta.volume_serial_number().unwrap_or(0))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        0
    }
}

fn file_id(meta: &fs::Metadata) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() <= 1 {
            return None;
        }
        let ino = meta.ino();
        if ino == 0 {
            return None;
        }
        Some(FileId {
            dev: meta.dev(),
            ino,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.number_of_links()? <= 1 {
            return None;
        }
        let ino = meta.file_index()?;
        if ino == 0 {
            return None;
        }
        Some(FileId {
            dev: u64::from(meta.volume_serial_number()?),
            ino,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        None
    }
}

fn is_junk_dir(name: &std::ffi::OsStr) -> bool {
    let lower = name.to_string_lossy().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "$recycle.bin"
            | "system volume information"
            | ".spotlight-v100"
            | ".trashes"
            | ".fseventsd"
            | ".temporaryitems"
            | ".documentrevisions-v100"
            | ".trash"
            | ".git"
            | ".svn"
            | ".hg"
            | ".thumbnails"
    )
}

fn is_junk_file(name: &std::ffi::OsStr) -> bool {
    let value = name.to_string_lossy();
    if value.starts_with("._") {
        return true;
    }
    matches!(
        value.to_ascii_lowercase().as_str(),
        ".ds_store" | "thumbs.db" | "desktop.ini" | "ehthumbs.db"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_all(min_size: u64) -> SearchOptions {
        SearchOptions {
            min_size,
            extensions: [".txt".into(), ".bin".into(), ".dat".into()]
                .into_iter()
                .collect(),
            full_hash_below: 1024,
            ..SearchOptions::default()
        }
    }

    fn run_search(root: &Path, options: SearchOptions) -> SearchOutcome {
        let progress = Progress::new();
        let inventory = inventory(
            root,
            &ScanOptions {
                skip_junk: true,
                ..ScanOptions::default()
            },
            &progress,
        )
        .unwrap();
        search(&inventory.files, &options, &Progress::new()).unwrap()
    }

    #[test]
    fn finds_exact_copies_and_ignores_same_size() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"same-text").unwrap();
        fs::write(dir.path().join("b.txt"), b"same-text").unwrap();
        fs::write(dir.path().join("c.txt"), b"other!!!!").unwrap();
        let outcome = run_search(dir.path(), opts_all(1));
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].copies, 2);
        assert_eq!(outcome.groups[0].reclaimable, 9);
    }

    #[test]
    fn skips_empty_junk_and_git() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"visible").unwrap();
        fs::write(dir.path().join("empty.txt"), b"").unwrap();
        fs::write(dir.path().join(".DS_Store"), b"visible").unwrap();
        let git = dir.path().join(".git");
        fs::create_dir(&git).unwrap();
        fs::write(git.join("keep.txt"), b"visible").unwrap();
        let progress = Progress::new();
        let inventory = inventory(dir.path(), &ScanOptions::default(), &progress).unwrap();
        assert_eq!(inventory.empty_files, 1);
        assert_eq!(
            inventory.files.iter().filter(|file| file.size > 0).count(),
            1
        );
        let outcome = search(&inventory.files, &opts_all(1), &Progress::new()).unwrap();
        assert!(outcome.groups.is_empty());
    }

    #[test]
    fn full_hash_separates_files_that_differ_outside_the_sample() {
        let dir = tempfile::tempdir().unwrap();
        let mark_at = (SAMPLE + 100) as usize;
        let size = (SAMPLE * 4) as usize;
        for (name, mark) in [("a.bin", 9u8), ("b.bin", 9u8), ("c.bin", 3u8)] {
            let mut data = vec![1u8; size];
            data[mark_at] = mark;
            fs::write(dir.path().join(name), data).unwrap();
        }
        let outcome = run_search(dir.path(), opts_all(1));
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].copies, 2);
        let names: Vec<_> = outcome.groups[0]
            .files
            .iter()
            .map(|file| {
                file.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(names.contains(&"a.bin".to_owned()));
        assert!(names.contains(&"b.bin".to_owned()));
        assert!(!names.contains(&"c.bin".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlink_or_count_hardlink_as_extra_copy() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("a.txt"), b"copy").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        fs::write(dir.path().join("b.txt"), b"copy").unwrap();
        fs::write(dir.path().join("hard.txt"), b"copy").unwrap();
        fs::hard_link(
            dir.path().join("hard.txt"),
            dir.path().join("hard-link.txt"),
        )
        .unwrap();

        let progress = Progress::new();
        let inventory = inventory(dir.path(), &ScanOptions::default(), &progress).unwrap();
        assert!(inventory.symlinks_skipped >= 1);
        let outcome = search(&inventory.files, &opts_all(1), &Progress::new()).unwrap();
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].copies, 3);
        assert_eq!(outcome.hardlinks_collapsed, 1);
        assert_eq!(outcome.groups[0].reclaimable, 8);

        let mut count_links = opts_all(1);
        count_links.collapse_hardlinks = false;
        let outcome = search(&inventory.files, &count_links, &Progress::new()).unwrap();
        assert_eq!(outcome.hardlinks_collapsed, 0);
        assert_eq!(outcome.groups[0].copies, 4);
    }

    #[test]
    fn optional_name_and_empty_rules() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"same-text").unwrap();
        fs::write(dir.path().join("b.txt"), b"same-text").unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("a.txt"), b"other-txt").unwrap();
        fs::write(dir.path().join("empty-a.txt"), b"").unwrap();
        fs::write(dir.path().join("empty-b.txt"), b"").unwrap();

        let mut same_name = opts_all(1);
        same_name.by_name = true;
        let outcome = run_search(dir.path(), same_name);
        assert!(outcome.groups.is_empty());

        let mut name_only = opts_all(1);
        name_only.by_content = false;
        name_only.by_size = false;
        name_only.by_name = true;
        let outcome = run_search(dir.path(), name_only);
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].copies, 2);
        assert!(outcome.groups[0].hash.is_empty());

        let mut with_empty = opts_all(1);
        with_empty.skip_empty = false;
        let outcome = run_search(dir.path(), with_empty);
        assert!(outcome
            .groups
            .iter()
            .any(|group| group.size == 0 && group.copies == 2));
        assert!(outcome.groups.iter().any(|group| group.size == 9));
    }
}
