use std::path::{Component, Path, PathBuf};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DirEntryType {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirEntry {
    path: PathBuf,
    kind: DirEntryType,
    content: Option<Vec<u8>>,
}

impl DirEntry {
    pub fn new<P: AsRef<Path>>(path: P, kind: DirEntryType) -> DirEntry {
        DirEntry { path: path.as_ref().to_path_buf(), kind, content: None }
    }
    
    pub fn path(&self) -> &Path {
        &self.path
    }
    
    pub fn kind(&self) -> DirEntryType {
        self.kind
    }
    
    pub fn is_file(&self) -> bool {
        self.kind == DirEntryType::File
    }
    
    pub fn is_dir(&self) -> bool {
        self.kind == DirEntryType::Directory
    }
    
    pub fn is_root(&self) -> bool {
        let components: Vec<_> = self.path.components().collect();
        self.kind == DirEntryType::Directory
            && components.len() == 1
            && components[0] == Component::RootDir
    }
}