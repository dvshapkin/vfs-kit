//! This module provides a virtual filesystem (VFS) implementation that maps to a memory storage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::core::{FsBackend, Result, utils};
use crate::{Entry, EntryType};

/// A virtual file system (VFS) implementation that stores file and directory entries in memory
/// using a hierarchical map structure.
///
/// `MapFS` provides a POSIX‑like file system interface where all data is kept in‑process,
/// allowing operations such as file creation, directory traversal, path resolution, and metadata
/// inspection without touching the host filesystem.
///
/// ### Internal state
///
/// * `root` — An absolute, normalized path associated with the host that serves as the physical
/// anchor of the virtual file system (VFS). It has no effect on VFS operation under typical usage
/// scenarios. This path determines how virtual paths are mapped to host paths
/// (e.g., for synchronization or persistent storage layers).
///   - Must be absolute and normalized (no `..`, no redundant separators).
///   - Example: `/tmp/my_vfs_root` on Unix, `C:\\vfs\\root` on Windows.
///
/// * `cwd` — Current Working Directory, expressed as an **inner absolute normalized path**
///   within the virtual file system.
///   - Determines how relative paths (e.g., `docs/file.txt`) are resolved.
///   - Always starts with `/` (or `\` on Windows) and is normalized.
///   - Default value: `/` (the virtual root).
///   - Changed via methods like `cd()`.
///
/// * `entries` — The core storage map that holds all virtual file and directory entries.
///   - Key: `PathBuf` representing **inner absolute normalized paths** (always start with `/`).
///   - Value: `Entry` struct containing type, metadata, and (for files) content.
///   - Uses `BTreeMap` for:
///     - Ordered traversal (natural path hierarchy).
///     - Efficient prefix‑based queries (e.g., `ls`, `forget`).
///     - Deterministic iteration.
///
/// ### Invariants
///
/// 1. **Root existence**: The path `/` is always present in `entries` and has type `Directory`.
/// 2. **Path normalization**: All keys in `entries`, as well as `cwd` and `root`, are normalized
///    (no `..`, no `//`, trailing `/` removed except for root).
/// 3. **Parent consistency**: For any entry at `/a/b/c`, there must exist an entry `/a/b` of type
///    `Directory` (except for the root `/`).
/// 4. **Uniqueness**: No duplicate paths; each path maps to exactly one `Entry`.
///
/// ### Lifecycle
///
/// - On creation: `root` and `cwd` is set to `/`; `entries` contains only the root directory.
///   If you want, you may set `root` to a user‑supplied host path;
/// - As files/directories are added via methods (e.g., `mkfile()`, `mkdir()`, `add()`), they are
///   inserted into `entries` with inner absolute paths.
/// - Path resolution (e.g., in `is_file()`, `ls()`) combines `cwd` with input paths to produce
///   inner absolute paths before querying `entries`.
///
/// ### Thread Safety
///
/// This struct is **not thread‑safe by default**. If concurrent access is required, wrap it in
/// a synchronization primitive (e.g., `Arc<Mutex<MapFS>>` or `RwLock<MapFS>`) at the application level.
///
/// ### Example
///
/// ```no_run
/// let fs = MapFS::new();
///
/// fs.mkdir("/docs").unwrap();
/// fs.mkfile("/docs/note.txt", Some(b"Hello")).unwrap();
///
/// assert!(fs.exists("/docs/note.txt"));
///
/// fs.rm("/docs/note.txt").unwrap();
/// ```
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
    /// Returns root path.
    fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Returns current working directory related to the vfs root.
    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    /// Returns a hypothetical "host-path" joining `root` and `inner_path`.
    /// * `inner_path` must exist in VFS
    fn to_host<P: AsRef<Path>>(&self, inner_path: P) -> Result<PathBuf> {
        let inner = self.to_inner(inner_path);
        Ok(self.root.join(inner.strip_prefix("/")?))
    }

    /// Changes the current working directory.
    /// * `path` can be in relative or absolute form, but in both cases it must exist in VFS.
    /// An error is returned if the specified `path` does not exist.
    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let target = self.to_inner(path);
        if !self.exists(&target) {
            return Err(anyhow!("{} does not exist", target.display()));
        }
        self.cwd = target;
        Ok(())
    }

    /// Checks if a `path` exists in the VFS.
    /// The `path` can be in relative or absolute form.
    fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        let inner = self.to_inner(path);
        self.entries.contains_key(&inner)
    }

    /// Checks if `path` is a directory.
    fn is_dir<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let path = path.as_ref();
        let inner = self.to_inner(path);
        if !self.exists(&inner) {
            return Err(anyhow!("{} does not exist", path.display()));
        }
        Ok(self.entries[&inner].is_dir())
    }

    /// Checks if `path` is a regular file.
    fn is_file<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let path = path.as_ref();
        let inner = self.to_inner(path);
        if !self.exists(&inner) {
            return Err(anyhow!("{} does not exist", path.display()));
        }
        Ok(self.entries[&inner].is_file())
    }

    /// Returns an iterator over directory entries at a specific depth (shallow listing).
    ///
    /// This method lists only the **immediate children** of the given directory,
    /// i.e., entries that are exactly one level below the specified path.
    /// It does *not* recurse into subdirectories (see `tree()` if you need recurse).
    ///
    /// # Arguments
    /// * `path` - path to the directory to list (must exist in VFS).
    ///
    /// # Returns
    /// * `Ok(impl Iterator<Item = &Path>)` - Iterator over entries of immediate children
    ///   (relative to VFS root). The yielded paths are *inside* the target directory
    ///   but do not include deeper nesting.
    /// * `Err(anyhow::Error)` - If the specified path does not exist in VFS.
    ///
    /// # Example:
    ///```no_run
    /// fs.mkdir("/docs/subdir");
    /// fs.mkfile("/docs/document.txt", None);
    ///
    /// // List root contents
    /// for path in fs.ls("/").unwrap() {
    ///     println!("{:?}", path);
    /// }
    ///
    /// // List contents of "/docs"
    /// for path in fs.ls("/docs").unwrap() {
    ///     if path.is_file() {
    ///         println!("File: {:?}", path);
    ///     } else {
    ///         println!("Dir:  {:?}", path);
    ///     }
    /// }
    /// ```
    ///
    /// # Notes
    /// - **No recursion:** Unlike `tree()`, this method does *not* traverse subdirectories.
    /// - **Path ownership:** The returned iterator borrows from the VFS's internal state.
    ///   It is valid as long as `self` lives.
    /// - **Excludes root:** The input directory itself is not included in the output.
    /// - **Error handling:** If `path` does not exist, an error is returned before iteration.
    /// - **Performance:** The filtering is done in‑memory; no additional filesystem I/O occurs
    ///   during iteration.
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

    /// Returns a recursive iterator over the directory tree starting from a given path.
    ///
    /// The iterator yields all entries (files and directories) that are *inside* the specified
    /// directory (i.e., the starting directory itself is **not** included).
    ///
    /// # Arguments
    /// * `path` - path to the directory to traverse (must exist in VFS).
    ///
    /// # Returns
    /// * `Ok(impl Iterator<Item = &Path>)` - Iterator over all entries *within* the tree
    ///   (relative to VFS root), excluding the root of the traversal.
    /// * `Err(anyhow::Error)` - If:
    ///   - The specified path does not exist in VFS.
    ///   - The path is not a directory (implicitly checked via `exists` and tree structure).
    ///
    /// # Behavior
    /// - **Recursive traversal**: Includes all nested files and directories.
    /// - **Excludes root**: The starting directory path is not yielded (only its contents).
    /// - **Path normalization**: Input path is normalized.
    /// - **VFS-only**: Only returns paths tracked in VFS.
    /// - **Performance:** The filtering is done in‑memory; no additional filesystem I/O occurs
    ///   during iteration.
    ///
    /// # Example:
    /// ```no_run
    /// fs.mkdir("/docs/subdir");
    /// fs.mkfile("/docs/document.txt", None);
    ///
    /// // Iterate over current working directory
    /// for path in fs.tree("/").unwrap() {
    ///     println!("{:?}", path);
    /// }
    ///
    /// // Iterate over a specific directory
    /// for path in fs.tree("/docs").unwrap() {
    ///     if path.is_file() {
    ///         println!("File: {:?}", path);
    ///     }
    /// }
    /// ```
    ///
    /// # Notes
    /// - The iterator borrows data from VFS. The returned iterator is valid as long
    ///   as `self` is alive.
    /// - Symbolic links are treated as regular entries (no follow/resolve).
    /// - Use `MapFS` methods (e.g., `is_file()`, `is_dir()`) for yielded items for type checks.
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

    /// Creates directory and all it parents (if needed).
    /// * `path` - inner vfs path.
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
                    .insert(built.clone(), Entry::new(EntryType::Directory));
            }
        }

        Ok(())
    }

    /// Creates new file in VFS.
    /// * `file_path` must be inner VFS path. It must contain the name of the file,
    /// optionally preceded by parent directory.
    /// If the parent directory does not exist, it will be created.
    fn mkfile<P: AsRef<Path>>(&mut self, file_path: P, content: Option<&[u8]>) -> Result<()> {
        let file_path = self.to_inner(file_path);
        if let Some(parent) = file_path.parent() {
            if !self.exists(parent) {
                self.mkdir(parent)?;
            }
        }

        let mut entry = Entry::new(EntryType::File);
        if let Some(content) = content {
            entry.set_content(content);
        }
        self.entries.insert(file_path.clone(), entry);

        Ok(())
    }

    /// Reads the entire contents of a file into a byte vector.
    /// * `path` is the inner VFS path.
    ///
    /// # Returns
    /// * `Ok(Vec<u8>)` - File content as a byte vector if successful.
    /// * `Err(anyhow::Error)` - If any of the following occurs:
    ///   - File does not exist in VFS (`file does not exist: ...`)
    ///   - Path points to a directory (`... is a directory`)
    ///
    /// # Notes
    /// - Does **not** follow symbolic links on the host filesystem (reads the link itself).
    /// - Returns an empty vector for empty files.
    fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>> {
        let path = path.as_ref();
        if self.is_dir(path)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.display()));
        }
        Ok(self.entries[path].content().cloned().unwrap_or(Vec::new()))
    }

    /// Writes bytes to an existing file, replacing its entire contents.
    /// * `path` - Path to the file.
    /// * `content` - Byte slice (`&[u8]`) to write to the file.
    ///
    /// # Returns
    /// * `Ok(())` - If the write operation succeeded.
    /// * `Err(anyhow::Error)` - If any of the following occurs:
    ///   - File does not exist in VFS (`file does not exist: ...`)
    ///   - Path points to a directory (`... is a directory`)
    ///
    /// # Behavior
    /// - **Overwrites completely**: The entire existing content is replaced.
    /// - **No file creation**: File must exist (use `mkfile()` first).
    fn write<P: AsRef<Path>>(&mut self, path: P, content: &[u8]) -> Result<()> {
        let path = path.as_ref();
        if self.is_dir(path)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.display()));
        }
        self.entries.get_mut(path).unwrap().set_content(content); // safe unwrap()
        Ok(())
    }

    /// Appends bytes to the end of an existing file, preserving its old contents.
    ///
    /// # Arguments
    /// * `path` - Path to the existing file.
    /// * `content` - Byte slice (`&[u8]`) to append to the file.
    ///
    /// # Returns
    /// * `Ok(())` - If the append operation succeeded.
    /// * `Err(anyhow::Error)` - If any of the following occurs:
    ///   - File does not exist in VFS (`file does not exist: ...`)
    ///   - Path points to a directory (`... is a directory`)
    ///
    /// # Behavior
    /// - **Appends only**: Existing content is preserved; new bytes are added at the end.
    /// - **File creation**: Does NOT create the file if it doesn't exist (returns error).
    fn append<P: AsRef<Path>>(&mut self, path: P, content: &[u8]) -> Result<()> {
        let path = path.as_ref();
        if self.is_dir(path)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.display()));
        }
        self.entries.get_mut(path).unwrap().append_content(content); // safe unwrap()
        Ok(())
    }

    /// Removes a file or directory at the specified path.
    ///
    /// - `path`: can be absolute (starting with '/') or relative to the current working
    /// directory (cwd). If the path is a directory, all its contents are removed recursively.
    ///
    /// Returns:
    /// - `Ok(())` on successful removal.
    /// - `Err(_)` if the path does not exist in the VFS;
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
            .map(|(pb, _)| pb)
            .filter(|&pb| pb.starts_with(&inner_path)) // Match prefix (includes subpaths)
            .cloned()
            .collect();

        // Remove all matched entries from the set
        for p in &removed {
            self.entries.remove(p);
        }

        Ok(())
    }

    /// Removes all artifacts (dirs and files) in vfs, but preserve its root.
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


#[cfg(test)]
mod tests {
    use super::*;

    mod creations {
        use super::*;

        #[test]
        fn test_new_map_fs() {
            let mut fs = MapFS::new();
            assert_eq!(fs.root(), "/");
            assert_eq!(fs.cwd(), "/");

            fs.set_root("/new/root").unwrap();
            assert_eq!(fs.root(), "/new/root");

            let host_path = fs.to_host("/inner/path").unwrap();
            assert_eq!(host_path.as_path(), "/new/root/inner/path");

            let result = fs.set_root("new/relative/root");
            assert!(result.is_err());
        }
    }
}
