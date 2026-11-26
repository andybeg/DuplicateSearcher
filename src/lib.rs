#![forbid(unsafe_code)]

mod formats;
mod model;
mod preview;
mod scan;

pub use formats::{classify, extension_of, FormatKind};
pub use model::{
    format_bytes, format_count, format_mtime, DupFile, DupGroup, FileRec, FormatInfo, Sample,
};
pub use preview::{load_preview, Preview};
pub use scan::{
    inventory, search, summarize, Inventory, Progress, ScanErr, ScanOptions, SearchOptions,
    SearchOutcome,
};
