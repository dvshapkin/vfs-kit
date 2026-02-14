use std::path::{Component, Path, PathBuf};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum EntryType {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    entry_type: EntryType,
    content: Option<Vec<u8>>,
}

impl Entry {
    pub fn new<P: AsRef<Path>>(path: P, entry_type: EntryType) -> Entry {
        Entry {
            entry_type,
            content: None,
        }
    }

    pub fn entry_type(&self) -> EntryType {
        self.entry_type
    }

    pub fn is_file(&self) -> bool {
        self.entry_type == EntryType::File
    }

    pub fn is_dir(&self) -> bool {
        self.entry_type == EntryType::Directory
    }
}
