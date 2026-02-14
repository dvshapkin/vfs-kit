//! This module provides a virtual filesystem (VFS) implementation that maps to a memory storage.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::anyhow;

use crate::core::{FsBackend, Result, utils};
use crate::{Entry, EntryType};

pub struct MapFS {
    root: PathBuf,                     // host-related absolute normalized path
    cwd: PathBuf,                      // inner absolute normalized path
    entries: BTreeMap<PathBuf, Entry>, // inner absolute normalized paths
}

impl MapFS {
    /// Creates new MapFS instance.
    /// By default, the root directory and current working directory are set to `/`.
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/"),
            cwd: PathBuf::from("/"),
            entries: BTreeMap::new(),
        }
    }

    /// Changes root path.
    /// * `path` must be an absolute
    /// If `path` isn't an absolute error returns.
    pub fn set_root<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(anyhow!("root path must be an absolute"));
        }
        self.root = path.to_path_buf();
        Ok(())
    }

    fn to_inner<P: AsRef<Path>>(&self, inner_path: P) -> PathBuf {
        utils::normalize(self.cwd.join(inner_path))
    }
}

impl FsBackend for MapFS {
    fn root(&self) -> &Path {
        self.root.as_path()
    }

    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    /// Returns the path on the host system that matches the specified internal path.
    /// * `inner_path` must exist in VFS
    fn to_host<P: AsRef<Path>>(&self, inner_path: P) -> Result<PathBuf> {
        let inner = self.to_inner(inner_path);
        Ok(self.root.join(inner.strip_prefix("/").unwrap()))
    }

    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let target = self.to_inner(path);
        if !self.exists(&target) {
            return Err(anyhow!("{} does not exist", target.display()));
        }
        self.cwd = target;
        Ok(())
    }

    fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        let inner = self.to_inner(path);
        self.entries.contains_key(&inner)
    }

    fn is_dir<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let path = path.as_ref();
        let inner = self.to_inner(path);
        if !self.exists(&inner) {
            return Err(anyhow!("{} does not exist", path.display()));
        }
        Ok(self.entries[&inner].is_dir())
    }

    fn is_file<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let path = path.as_ref();
        let inner = self.to_inner(path);
        if !self.exists(&inner) {
            return Err(anyhow!("{} does not exist", path.display()));
        }
        Ok(self.entries[&inner].is_file())
    }

    fn ls<P: AsRef<Path>>(&self, path: P) -> Result<impl Iterator<Item = &Path>> {
        let inner_path = self.to_inner(path);
        if !self.exists(&inner_path) {
            return Err(anyhow!("{} does not exist", inner_path.display()));
        }
        let component_count = inner_path.components().count() + 1;
        Ok(self
            .entries
            .iter()
            .map(|(pb, _)| pb.as_path())
            .filter(move |&path| {
                path.starts_with(&inner_path)
                    && path != inner_path
                    && path.components().count() == component_count
            }))
    }

    fn tree<P: AsRef<Path>>(&self, path: P) -> Result<impl Iterator<Item = &Path>> {
        let inner_path = self.to_inner(path);
        if !self.exists(&inner_path) {
            return Err(anyhow!("{} does not exist", inner_path.display()));
        }
        Ok(self
            .entries
            .iter()
            .map(|(pb, _)| pb.as_path())
            .filter(move |&path| path.starts_with(&inner_path) && path != inner_path))
    }

    fn mkdir<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if path.as_ref().as_os_str().is_empty() {
            return Err(anyhow!("invalid path: empty"));
        }

        let inner_path = self.to_inner(path);

        if self.exists(&inner_path) {
            return Err(anyhow!("path already exists: {}", inner_path.display()));
        }

        // Looking for the first existing parent
        let mut existed_parent = inner_path.clone();
        while let Some(parent) = existed_parent.parent() {
            let parent_buf = parent.to_path_buf();
            if self.exists(parent) {
                existed_parent = parent_buf;
                break;
            }
            existed_parent = parent_buf;
        }

        // Create from the closest existing parent to the target path
        let need_to_create: Vec<_> = inner_path
            .strip_prefix(&existed_parent)?
            .components()
            .collect();

        let mut built = PathBuf::from(&existed_parent);
        for component in need_to_create {
            built.push(component);
            if !self.exists(&built) {
                self.entries
                    .insert(built.clone(), Entry::new(&built, EntryType::Directory));
            }
        }

        Ok(())
    }

    fn mkfile<P: AsRef<Path>>(&mut self, file_path: P, content: Option<&[u8]>) -> Result<()> {
        let file_path = self.to_inner(file_path);
        if let Some(parent) = file_path.parent() {
            if !self.exists(parent) {
                self.mkdir(parent)?;
            }
        }
        // let host = self.to_host(&file_path)?;
        // let mut fd = std::fs::File::create(host)?;

        todo!("Where to store the file content?");

        self.entries
            .insert(file_path.clone(), Entry::new(&file_path, EntryType::File));
        if let Some(content) = content {
            //fd.write_all(content)?;
        }
        Ok(())
    }

    fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>> {
        todo!()
    }

    fn write<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()> {
        todo!()
    }

    fn append<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()> {
        todo!()
    }

    fn rm<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if path.as_ref().as_os_str().is_empty() {
            return Err(anyhow!("invalid path: empty"));
        }
        if utils::is_virtual_root(&path) {
            return Err(anyhow!("invalid path: the root cannot be removed"));
        }

        let inner_path = self.to_inner(path); // Convert to VFS-internal normalized path

        // Check if the path exists in the virtual filesystem
        if !self.exists(&inner_path) {
            return Err(anyhow!("{} does not exist", inner_path.display()));
        }

        // Update internal state: collect all entries that start with `inner_path`
        let removed: Vec<PathBuf> = self
            .entries
            .iter()
            .map(|(entry_path, _)| entry_path)
            .filter(|&p| p.starts_with(&inner_path)) // Match prefix (includes subpaths)
            .cloned()
            .collect();

        // Remove all matched entries from the set
        for p in &removed {
            self.entries.remove(p);
        }

        Ok(())
    }

    fn cleanup(&mut self) -> bool {
        // Collect all paths to delete (except the root "/")
        let mut sorted_paths_to_remove: BTreeSet<PathBuf> = BTreeSet::new();
        for (pb, _) in &self.entries {
            if pb != "/" {
                sorted_paths_to_remove.insert(pb.clone());
            }
        }

        for entry in sorted_paths_to_remove.iter().rev() {
            self.entries.remove(entry);
        }

        true
    }
}
