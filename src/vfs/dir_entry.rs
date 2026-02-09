use std::path::PathBuf;

pub enum DirEntryType {
    File,
    Directory,
}

pub struct DirEntry {
    path: PathBuf,
    kind: DirEntryType,
}