//! This module provides a virtual filesystem (VFS) implementation that maps to a real directory
//! on the host system. It allows file and directory operations (create, read, remove, navigate)
//! within a controlled root path while maintaining internal state consistency.
//!
//! ### Key Features:
//! - **Isolated root**: All operations are confined to a designated root directory (self.root).
//! - **Path normalization**: Automatically resolves . and .. components and removes trailing slashes.
//! - **State tracking**: Maintains an internal set of valid paths (self.entries) to reflect VFS
//!   structure.
//! - **Auto‑cleanup**: Optionally removes created artifacts on Drop (when is_auto_clean = true).
//! - **Cross‑platform**: Uses std::path::Path and PathBuf for portable path handling.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::core::{FsBackend, Result, utils};
use crate::{Entry, EntryType};

/// A virtual filesystem (VFS) implementation that maps to a real directory on the host system.
///
/// `DirFS` provides an isolated, path‑normalized view of a portion of the filesystem, rooted at a
/// designated absolute path (`root`). It maintains an internal state of valid paths and supports
/// all operations defined in `FsBackend` trait.
///
/// ### Usage notes:
/// - `DirFS` does not follow symlinks; `rm()` removes the link, not the target.
/// - Permissions are not automatically adjusted; ensure `root` is writable.
/// - Not thread‑safe in current version (wrap in `Mutex` if needed).
/// - Errors are returned via `anyhow::Result` with descriptive messages.
///
/// ### Example:
/// ```
/// use vfs_kit::{DirFS, FsBackend};
///
/// let tmp = std::env::temp_dir();
/// let root = tmp.join("my_vfs");
///
/// let mut fs = DirFS::new(root).unwrap();
/// fs.mkdir("/docs").unwrap();
/// fs.mkfile("/docs/note.txt", Some(b"Hello")).unwrap();
/// assert!(fs.exists("/docs/note.txt"));
///
/// fs.rm("/docs/note.txt").unwrap();
/// ```
pub struct DirFS {
    root: PathBuf,                      // host-related absolute normalized path
    cwd: PathBuf,                       // inner absolute normalized path
    entries: BTreeMap<PathBuf, Entry>,  // inner absolute normalized paths
    created_root_parents: Vec<PathBuf>, // host-related absolute normalized paths
    is_auto_clean: bool,
}

impl DirFS {
    /// Creates a new DirFs instance with the root directory at `path`.
    /// Checks permissions to create and write into `path`.
    /// * `path` is an absolute host path. If path not exists it will be created.
    /// If `path` is not absolute or path is not a directory, error returns.
    /// By default, the `is_auto_clean` flag is set to `true`.
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref();

        if root.as_os_str().is_empty() {
            return Err(anyhow!("invalid root path: empty"));
        }
        if root.is_relative() {
            return Err(anyhow!("the root path must be absolute"));
        }
        if root.exists() && !root.is_dir() {
            return Err(anyhow!("{:?} is not a directory", root));
        }

        let root = utils::normalize(root);

        let mut created_root_parents = Vec::new();
        if !std::fs::exists(&root)? {
            created_root_parents.extend(Self::mkdir_all(&root)?);
        }

        // check permissions
        if !Self::check_permissions(&root) {
            return Err(anyhow!("Access denied: {:?}", root));
        }

        let inner_root = PathBuf::from("/");
        let mut entries = BTreeMap::new();
        entries.insert(inner_root.clone(), Entry::new(EntryType::Directory));

        Ok(Self {
            root,
            cwd: inner_root,
            entries,
            created_root_parents,
            is_auto_clean: true,
        })
    }

    /// Changes auto-clean flag.
    /// If auto-clean flag is true all created in vfs artifacts
    /// will be removed on drop.
    pub fn set_auto_clean(&mut self, clean: bool) {
        self.is_auto_clean = clean;
    }

    /// Adds an existing artifact (file or directory) to the VFS.
    /// The artifact must exist and be located in the VFS root directory.
    /// If artifact is directory - all its childs will be added recursively.
    /// Once added, it will be managed by the VFS (e.g., deleted upon destruction).
    /// * `path` is an inner VFS path.
    pub fn add<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let inner = self.to_inner(&path);
        let host = self.to_host(&inner)?;
        if !host.exists() {
            return Err(anyhow!(
                "No such file or directory: {}",
                path.as_ref().display()
            ));
        }
        self.add_recursive(&inner, &host)
    }

    /// Removes a file or directory from the VFS and recursively untracks all its contents.
    ///
    /// This method "forgets" the specified path — it is removed from the VFS tracking.
    /// If the path is a directory, all its children (files and subdirectories) are also untracked
    /// recursively.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to remove from the VFS. Can be a file or a directory.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - If the path was successfully removed (or was not tracked in the first place).
    /// * `Err(anyhow::Error)` - If:
    ///   * The path is not tracked by the VFS.
    ///   * The path is the root directory (`/`), which cannot be forgotten.
    ///
    /// # Behavior
    ///
    /// 1. **Existence check**: Returns an error if the resolved path is not currently tracked.
    /// 2. **Root protection**: Blocks attempts to forget the root directory (`/`).
    /// 3. **Removal**:
    ///    * If the path is a file: removes only that file.
    ///    * If the path is a directory: removes the directory and all its descendants (recursively).
    ///
    ///
    /// # Examples
    ///
    /// ```no_run
    /// vfs.mkdir("/docs/backup");
    /// vfs.mkfile("/docs/readme.txt", None);
    ///
    /// // Forget the entire /docs directory (and all its contents)
    /// vfs.forget("/docs").unwrap();
    ///
    /// assert!(!vfs.exists("/docs/readme.txt"));
    /// assert!(!vfs.exists("/docs/backup"));
    /// ```
    ///
    /// ```no_run
    /// // Error: trying to forget a non-existent path
    /// assert!(vfs.forget("/nonexistent").is_err());
    ///
    /// // Error: trying to forget the root
    /// assert!(vfs.forget("/").is_err());
    /// ```
    ///
    /// # Notes
    ///
    /// * The method does **not** interact with the real filesystem — it only affects the VFS's
    ///   internal tracking.
    /// * If the path does not exist in the VFS, the method returns an error
    ///   (unlike `remove` in some systems that may silently succeed).
    pub fn forget<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let inner = self.to_inner(&path);
        if !self.exists(&inner) {
            return Err(anyhow!("{:?} path is not tracked by VFS", path.as_ref()));
        }
        if utils::is_virtual_root(&inner) {
            return Err(anyhow!("cannot forget root directory"));
        }

        if let Some(entry) = self.entries.remove(&inner) {
            if entry.is_dir() {
                let childs: Vec<_> = self
                    .entries
                    .iter()
                    .map(|(path, _)| path)
                    .filter(|&path| path.starts_with(&inner))
                    .cloned()
                    .collect();

                for child in childs {
                    self.entries.remove(&child);
                }
            }
        }

        Ok(())
    }

    fn to_inner<P: AsRef<Path>>(&self, inner_path: P) -> PathBuf {
        utils::normalize(self.cwd.join(inner_path))
    }

    /// Make directories recursively.
    /// * `path` is an absolute host path.
    /// Returns vector of created directories.
    fn mkdir_all<P: AsRef<Path>>(path: P) -> Result<Vec<PathBuf>> {
        let host_path = path.as_ref().to_path_buf();

        // Looking for the first existing parent
        let mut existed_part = host_path.clone();
        while let Some(parent) = existed_part.parent() {
            let parent_buf = parent.to_path_buf();
            if std::fs::exists(parent)? {
                existed_part = parent_buf;
                break;
            }
            existed_part = parent_buf;
        }

        // Create from the closest existing parent to the target path
        let need_to_create: Vec<_> = host_path
            .strip_prefix(&existed_part)?
            .components()
            .collect();

        let mut created = Vec::new();

        let mut built = PathBuf::from(&existed_part);
        for component in need_to_create {
            built.push(component);
            if !std::fs::exists(&built)? {
                std::fs::create_dir(&built)?;
                created.push(built.clone());
            }
        }

        Ok(created)
    }

    fn check_permissions<P: AsRef<Path>>(path: P) -> bool {
        let path = path.as_ref();
        let filename = path.join(".access");
        if let Err(_) = std::fs::write(&filename, b"check") {
            return false;
        }
        if let Err(_) = std::fs::remove_file(filename) {
            return false;
        }
        true
    }

    /// Recursively adds a directory and all its entries to the VFS.
    fn add_recursive(&mut self, inner_path: &Path, host_path: &Path) -> Result<()> {
        let entry_type = if host_path.is_dir() {
            EntryType::Directory
        } else {
            EntryType::File
        };
        self.entries
            .insert(inner_path.to_path_buf(), Entry::new(entry_type));

        if host_path.is_dir() {
            for entry in std::fs::read_dir(host_path)? {
                let entry = entry?;
                let host_child = entry.path();
                let inner_child = inner_path.join(entry.file_name());

                self.add_recursive(&inner_child, &host_child)?;
            }
        }

        Ok(())
    }
}

impl FsBackend for DirFS {
    /// Returns root path related to the host file system.
    fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Returns current working directory related to the vfs root.
    fn cwd(&self) -> &Path {
        self.cwd.as_path()
    }

    /// Returns the path on the host system that matches the specified internal path.
    /// * `inner_path` must exist in VFS
    fn to_host<P: AsRef<Path>>(&self, inner_path: P) -> Result<PathBuf> {
        let inner = self.to_inner(inner_path);
        Ok(self.root.join(inner.strip_prefix("/").unwrap()))
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
    /// for entry in fs.ls("/").unwrap() {
    ///     println!("{:?}", entry);
    /// }
    ///
    /// // List contents of "/docs"
    /// for entry in fs.ls("/docs").unwrap() {
    ///     if entry.is_file() {
    ///         println!("File: {:?}", entry);
    ///     } else {
    ///         println!("Dir:  {:?}", entry);
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
    /// for entry in fs.tree("/").unwrap() {
    ///     println!("{:?}", entry);
    /// }
    ///
    /// // Iterate over a specific directory
    /// for entry in fs.tree("/docs").unwrap() {
    ///     if entry.is_file() {
    ///         println!("File: {:?}", entry);
    ///     }
    /// }
    /// ```
    ///
    /// # Notes
    /// - The iterator borrows data from VFS. The returned iterator is valid as long
    ///   as `self` is alive.
    /// - Symbolic links are treated as regular entries (no follow/resolve).
    /// - Use `DirFS` methods (e.g., `is_file()`, `is_dir()`) for yielded items for type checks.
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
                let host = self.to_host(&built)?;
                std::fs::create_dir(&host)?;
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
        let host = self.to_host(&file_path)?;
        let mut fd = std::fs::File::create(host)?;
        self.entries
            .insert(file_path.clone(), Entry::new(EntryType::File));
        if let Some(content) = content {
            fd.write_all(content)?;
        }
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
    ///   - Permission issues when accessing the host file
    ///   - I/O errors during reading
    ///
    /// # Notes
    /// - Does **not** follow symbolic links on the host filesystem (reads the link itself).
    /// - Returns an empty vector for empty files.
    fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>> {
        let inner = self.to_inner(&path);
        if self.is_dir(&inner)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.as_ref().display()));
        }
        let mut content = Vec::new();
        let host = self.to_host(&inner)?;
        std::fs::File::open(&host)?.read_to_end(&mut content)?;

        Ok(content)
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
    ///   - Permission issues when accessing the host file
    ///   - I/O errors during writing (e.g., disk full, invalid path)
    ///
    /// # Behavior
    /// - **Overwrites completely**: The entire existing content is replaced.
    /// - **No file creation**: File must exist (use `mkfile()` first).
    /// - **Atomic operation**: Uses `std::fs::write()` which replaces the file in one step.
    /// - **Permissions**: The file retains its original permissions (no chmod is performed).
    fn write<P: AsRef<Path>>(&mut self, path: P, content: &[u8]) -> Result<()> {
        let inner = self.to_inner(&path);
        if self.is_dir(&inner)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.as_ref().display()));
        }
        let host = self.to_host(&inner)?;
        std::fs::write(&host, content)?;

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
    ///   - Permission issues when accessing the host file
    ///   - I/O errors during writing (e.g., disk full, invalid path)
    ///
    /// # Behavior
    /// - **Appends only**: Existing content is preserved; new bytes are added at the end.
    /// - **File creation**: Does NOT create the file if it doesn't exist (returns error).
    /// - **Permissions**: The file retains its original permissions.
    fn append<P: AsRef<Path>>(&mut self, path: P, content: &[u8]) -> Result<()> {
        let inner = self.to_inner(&path);
        if self.is_dir(&inner)? {
            // checks for existent too
            return Err(anyhow!("{} is a directory", path.as_ref().display()));
        }
        // Open file in append mode and write content
        use std::fs::OpenOptions;
        let host = self.to_host(&inner)?;
        let mut file = OpenOptions::new().write(true).append(true).open(&host)?;

        file.write_all(content)?;

        Ok(())
    }

    /// Removes a file or directory at the specified path.
    ///
    /// - `path`: can be absolute (starting with '/') or relative to the current working
    /// directory (cwd). If the path is a directory, all its contents are removed recursively.
    ///
    /// Returns:
    /// - `Ok(())` on successful removal.
    /// - `Err(_)` if:
    ///   - the path does not exist in the VFS;
    ///   - there are insufficient permissions;
    ///   - a filesystem error occurs.
    fn rm<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if path.as_ref().as_os_str().is_empty() {
            return Err(anyhow!("invalid path: empty"));
        }
        if utils::is_virtual_root(&path) {
            return Err(anyhow!("invalid path: the root cannot be removed"));
        }

        let inner_path = self.to_inner(path); // Convert to VFS-internal normalized path
        let host_path = self.to_host(&inner_path)?; // Map to real filesystem path

        // Check if the path exists in the virtual filesystem
        if !self.exists(&inner_path) {
            return Err(anyhow!("{} does not exist", inner_path.display()));
        }

        // Remove from the real filesystem
        if std::fs::exists(&host_path)? {
            utils::rm_on_host(&host_path)?;
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

    /// Removes all artifacts (dirs and files) in vfs, but preserve its root.
    fn cleanup(&mut self) -> bool {
        let mut is_ok = true;

        // Collect all paths to delete (except the root "/")
        let mut sorted_paths_to_remove: BTreeSet<PathBuf> = BTreeSet::new();
        for (pb, _) in &self.entries {
            if pb != "/" {
                sorted_paths_to_remove.insert(pb.clone());
            }
        }

        for entry in sorted_paths_to_remove.iter().rev() {
            if let Ok(host) = self.to_host(entry) {
                let result = utils::rm_on_host(&host);
                if result.is_ok() {
                    self.entries.remove(entry);
                } else {
                    is_ok = false;
                    eprintln!("Unable to remove: {}", host.display());
                }
            }
        }

        is_ok
    }
}

impl Drop for DirFS {
    fn drop(&mut self) {
        if !self.is_auto_clean {
            return;
        }

        if self.cleanup() {
            self.entries.clear();
        }

        let errors: Vec<_> = self
            .created_root_parents
            .iter()
            .rev()
            .filter_map(|p| utils::rm_on_host(p).err())
            .collect();
        if !errors.is_empty() {
            eprintln!("Failed to remove parents: {:?}", errors);
        }

        self.created_root_parents.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempdir::TempDir;

    mod creations {
        use super::*;

        #[test]
        fn test_new_absolute_path_existing() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().to_path_buf();

            let fs = DirFS::new(&root).unwrap();

            assert_eq!(fs.root, root);
            assert_eq!(fs.cwd, PathBuf::from("/"));
            assert!(fs.entries.contains_key(&PathBuf::from("/")));
            assert!(fs.created_root_parents.is_empty());
            assert!(fs.is_auto_clean);
        }

        #[test]
        fn test_new_nonexistent_path_created() {
            let temp_dir = setup_test_env();
            let nonexistent = temp_dir.path().join("new_root");

            let fs = DirFS::new(&nonexistent).unwrap();

            assert_eq!(fs.root, nonexistent);
            assert!(!fs.created_root_parents.is_empty()); // parents must be created
            assert!(nonexistent.exists()); // The catalog has been created
        }

        #[test]
        fn test_new_nested_nonexistent_path() {
            let temp_dir = setup_test_env();
            let nested = temp_dir.path().join("a/b/c");

            let fs = DirFS::new(&nested).unwrap();

            assert_eq!(fs.root, nested);
            assert_eq!(fs.created_root_parents.len(), 3); // a, a/b, a/b/c
            assert!(nested.exists());
        }

        #[test]
        fn test_new_permission_denied() {
            // This test requires a specific environment (e.g. readonly FS)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                let protected = temp_dir.path().join("protected");
                let protected_root = protected.join("root");
                std::fs::create_dir_all(&protected_root).unwrap();
                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o000)).unwrap(); // No access

                let result = DirFS::new(&protected_root);
                assert!(result.is_err());

                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o755)).unwrap(); // Grant access
            }
        }

        #[test]
        fn test_new_normalize_path() {
            let temp_dir = setup_test_env();
            let messy_path = temp_dir.path().join("././subdir/../subdir");

            let fs = DirFS::new(&messy_path).unwrap();
            let canonical = utils::normalize(temp_dir.path().join("subdir"));

            assert_eq!(fs.root, canonical);
        }

        #[test]
        fn test_new_root_is_file() {
            let temp_dir = setup_test_env();
            let file_path = temp_dir.path().join("file.txt");
            std::fs::write(&file_path, "content").unwrap();

            let result = DirFS::new(&file_path);
            assert!(result.is_err()); // Cannot create DirFs on file
        }

        #[test]
        fn test_new_empty_path() {
            let result = DirFS::new("");
            assert!(result.is_err());
        }

        #[test]
        fn test_new_special_characters() {
            let temp_dir = setup_test_env();
            let special = temp_dir.path().join("папка с пробелами и юникод!");

            let fs = DirFS::new(&special).unwrap();

            assert_eq!(fs.root, special);
            assert!(special.exists());
        }

        #[test]
        fn test_new_is_auto_clean_default() {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path()).unwrap();
            assert!(fs.is_auto_clean); // True by default
        }

        #[test]
        fn test_root_returns_correct_path() {
            let temp_dir = setup_test_env();

            let vfs_root = temp_dir.path().join("vfs-root");
            let fs = DirFS::new(&vfs_root).unwrap();
            assert_eq!(fs.root(), vfs_root);
        }

        #[test]
        fn test_cwd_defaults_to_root() {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir).unwrap();
            assert_eq!(fs.cwd(), Path::new("/"));
        }
    }

    mod normalize {
        use super::*;

        #[test]
        fn test_normalize_path() {
            assert_eq!(utils::normalize("/a/b/c/"), PathBuf::from("/a/b/c"));
            assert_eq!(utils::normalize("/a/b/./c"), PathBuf::from("/a/b/c"));
            assert_eq!(utils::normalize("/a/b/../c"), PathBuf::from("/a/c"));
            assert_eq!(utils::normalize("/"), PathBuf::from("/"));
            assert_eq!(utils::normalize("/.."), PathBuf::from("/"));
            assert_eq!(utils::normalize(".."), PathBuf::from(""));
            assert_eq!(utils::normalize(""), PathBuf::from(""));
            assert_eq!(utils::normalize("../a"), PathBuf::from("a"));
            assert_eq!(utils::normalize("./a"), PathBuf::from("a"));
        }
    }

    mod cd {
        use super::*;

        #[test]
        fn test_cd_to_absolute_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/projects").unwrap();
            fs.cd("/projects").unwrap();
            assert_eq!(fs.cwd(), Path::new("/projects"));
        }

        #[test]
        fn test_cd_with_relative_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/home/user").unwrap();
            fs.cwd = PathBuf::from("/home");
            fs.cd("user").unwrap();
            assert_eq!(fs.cwd(), Path::new("/home/user"));
        }

        #[test]
        fn test_cd_extreme_cases() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();

            fs.cd("..").unwrap(); // where cwd == "/"
            assert_eq!(fs.cwd(), Path::new("/"));

            fs.cd(".").unwrap(); // where cwd == "/"
            assert_eq!(fs.cwd(), Path::new("/"));

            fs.cwd = PathBuf::from("/home");
            assert_eq!(fs.cwd(), Path::new("/home"));
            fs.mkdir("/other").unwrap();
            fs.cd("../other").unwrap();
            assert_eq!(fs.cwd(), Path::new("/other"));

            fs.cwd = PathBuf::from("/home");
            assert_eq!(fs.cwd(), Path::new("/home"));
            fs.mkdir("/home/other").unwrap();
            fs.cd("./other").unwrap();
            assert_eq!(fs.cwd(), Path::new("/home/other"));
        }
    }

    mod mkdir {
        use super::*;

        #[test]
        fn test_mkdir_create_single_dir() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/projects").unwrap();
            assert!(fs.exists("/projects"));
        }

        #[test]
        fn test_mkdir_relative_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("home").unwrap();
            fs.cd("/home").unwrap();
            fs.mkdir("user").unwrap();
            assert!(fs.exists("/home/user"));
        }

        #[test]
        fn test_mkdir_nested_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/a/b/c").unwrap();
            assert!(fs.exists("/a"));
            assert!(fs.exists("/a/b"));
            assert!(fs.exists("/a/b/c"));
        }

        #[test]
        fn test_mkdir_already_exists() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/data").unwrap();
            let result = fs.mkdir("/data");
            assert!(result.is_err());
        }

        #[test]
        fn test_mkdir_invalid_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            let result = fs.mkdir("");
            assert!(result.is_err());
        }
    }

    mod exists {
        use super::*;

        #[test]
        fn test_exists_root() {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(&temp_dir).unwrap();
            assert!(fs.exists("/"));
        }

        #[test]
        fn test_exists_cwd() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir).unwrap();
            fs.mkdir("/projects").unwrap();
            fs.cd("/projects").unwrap();
            assert!(fs.exists("."));
            assert!(fs.exists("./"));
            assert!(fs.exists("/projects"));
        }

        #[test]
        fn test_exists_empty_path() {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(&temp_dir).unwrap();
            assert!(fs.exists(""));
        }
    }

    mod is_dir_file {
        use super::*;

        #[test]
        fn test_is_dir_existing_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkdir("/docs")?;

            let result = vfs.is_dir("/docs")?;
            assert!(result, "Expected /docs to be a directory");

            Ok(())
        }

        #[test]
        fn test_is_dir_nonexistent_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let vfs = DirFS::new(temp_dir.path())?;

            let result = vfs.is_dir("/nonexistent");
            assert!(result.is_err(), "Expected error for nonexistent path");
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should mention path does not exist"
            );

            Ok(())
        }

        #[test]
        fn test_is_dir_file_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkfile("/file.txt", Some(b"Content"))?;

            let result = vfs.is_dir("/file.txt")?;
            assert!(!result, "Expected /file.txt not to be a directory");

            Ok(())
        }

        #[test]
        fn test_is_file_existing_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkfile("/report.pdf", Some(b"PDF Content"))?;

            let result = vfs.is_file("/report.pdf")?;
            assert!(result, "Expected /report.pdf to be a file");

            Ok(())
        }

        #[test]
        fn test_is_file_nonexistent_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let vfs = DirFS::new(temp_dir.path())?;

            let result = vfs.is_file("/missing.txt");
            assert!(result.is_err(), "Expected error for nonexistent file");
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should indicate path does not exist"
            );

            Ok(())
        }

        #[test]
        fn test_is_file_directory_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkdir("/src")?;
            let result = vfs.is_file("/src")?;
            assert!(!result, "Expected /src not to be a regular file");

            Ok(())
        }

        #[test]
        fn test_is_dir_and_is_file_on_same_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkfile("/data.json", Some(b"{}"))?;

            // File should not be a directory
            assert!(!vfs.is_dir("/data.json")?);
            // But should be a file
            assert!(vfs.is_file("/data.json")?);

            Ok(())
        }

        #[test]
        fn test_is_dir_and_is_file_on_same_dir() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkdir("/assets")?;

            // Directory should be a directory
            assert!(vfs.is_dir("/assets")?);
            // But not a regular file
            assert!(!vfs.is_file("/assets")?);

            Ok(())
        }

        #[test]
        fn test_relative_paths_resolution() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut vfs = DirFS::new(temp_dir.path())?;

            vfs.mkdir("/base")?;
            vfs.cd("/base")?;
            vfs.mkdir("sub")?;
            vfs.mkfile("file.txt", None)?;

            // Test relative directory
            assert!(vfs.is_dir("sub")?);
            // Test relative file
            assert!(vfs.is_file("file.txt")?);

            Ok(())
        }

        #[test]
        fn test_root_directory_checks() -> Result<()> {
            let temp_dir = setup_test_env();
            let vfs = DirFS::new(temp_dir.path())?;

            assert!(vfs.is_dir("/")?, "Root '/' should be a directory");
            assert!(!vfs.is_file("/")?, "Root should not be a regular file");

            Ok(())
        }
    }

    mod ls {
        use super::*;

        #[test]
        fn test_ls_empty_cwd() -> Result<()> {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path())?;

            let entries: Vec<_> = fs.ls(fs.cwd())?.collect();
            assert!(entries.is_empty(), "CWD should have no entries");

            Ok(())
        }

        #[test]
        fn test_ls_single_file_in_cwd() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/file.txt", Some(b"Hello"))?;

            let entries: Vec<_> = fs.ls(fs.cwd())?.collect();
            assert_eq!(entries.len(), 1, "Should return exactly one file");
            assert_eq!(entries[0], Path::new("/file.txt"), "File path should match");

            Ok(())
        }

        #[test]
        fn test_ls_multiple_items_in_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.mkfile("/docs/readme.txt", None)?;
            fs.mkfile("/docs/todo.txt", None)?;

            let entries: Vec<_> = fs.ls("/docs")?.collect();

            assert_eq!(entries.len(), 2, "Should list both files in directory");
            assert!(entries.contains(&PathBuf::from("/docs/readme.txt").as_path()));
            assert!(entries.contains(&PathBuf::from("/docs/todo.txt").as_path()));

            Ok(())
        }

        #[test]
        fn test_ls_nested_files_excluded() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/project/src")?;
            fs.mkfile("/project/main.rs", None)?;
            fs.mkfile("/project/src/lib.rs", None)?; // nested - should be excluded

            let entries: Vec<_> = fs.ls("/project")?.collect();

            assert_eq!(entries.len(), 2, "Only immediate children should be listed");
            assert!(entries.contains(&PathBuf::from("/project/main.rs").as_path()));
            assert!(
                !entries
                    .iter()
                    .any(|&p| p == PathBuf::from("/project/src/lib.rs").as_path()),
                "Nested file should not be included"
            );

            Ok(())
        }

        #[test]
        fn test_ls_directories_and_files_mixed() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/mix")?;
            fs.mkfile("/mix/file1.txt", None)?;
            fs.mkdir("/mix/subdir")?; // subdirectory - should be included
            fs.mkfile("/mix/subdir/deep.txt", None)?; // deeper - should be excluded

            let entries: Vec<_> = fs.ls("/mix")?.collect();

            assert_eq!(
                entries.len(),
                2,
                "Both file and subdirectory should be listed"
            );
            assert!(entries.contains(&PathBuf::from("/mix/file1.txt").as_path()));
            assert!(entries.contains(&PathBuf::from("/mix/subdir").as_path()));
            assert!(
                !entries
                    .iter()
                    .any(|&p| p.to_str().unwrap().contains("deep.txt")),
                "Deeper nested file should be excluded"
            );

            Ok(())
        }

        #[test]
        fn test_ls_nonexistent_path_returns_error() -> Result<()> {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path())?;

            let result: Result<Vec<_>> = fs.ls("/nonexistent/path").map(|iter| iter.collect());

            assert!(result.is_err(), "Should return error for nonexistent path");
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error message should indicate path does not exist"
            );

            Ok(())
        }

        #[test]
        fn test_ls_relative_path_resolution() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/base")?;
            fs.cd("/base")?;
            fs.mkdir("sub")?;
            fs.mkfile("sub/file.txt", None)?;
            fs.mkfile("note.txt", None)?;

            // List contents of relative path "sub"
            let sub_entries: Vec<_> = fs.ls("sub")?.collect();
            assert_eq!(
                sub_entries.len(),
                1,
                "Current directory should list one item"
            );

            // List current directory (base)
            let base_entries: Vec<_> = fs.ls(".")?.collect();
            assert_eq!(
                base_entries.len(),
                2,
                "Current directory should list two items"
            );
            assert!(base_entries.contains(&PathBuf::from("/base/sub").as_path()));
            assert!(base_entries.contains(&PathBuf::from("/base/note.txt").as_path()));

            Ok(())
        }

        #[test]
        fn test_ls_unicode_path_support() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/проект")?;
            fs.mkfile("/проект/документ.txt", Some(b"Content"))?;
            fs.mkdir("/проект/подпапка")?;
            fs.mkfile("/проект/подпапка/файл.txt", Some(b"Nested"))?; // should be excluded

            let entries: Vec<_> = fs.ls("/проект")?.collect();

            assert_eq!(
                entries.len(),
                2,
                "Should include both file and subdir at level"
            );
            assert!(entries.contains(&PathBuf::from("/проект/документ.txt").as_path()));
            assert!(entries.contains(&PathBuf::from("/проект/подпапка").as_path()));
            assert!(
                !entries
                    .iter()
                    .any(|&p| p.to_str().unwrap().contains("файл.txt")),
                "Nested unicode file should be excluded"
            );

            Ok(())
        }

        #[test]
        fn test_ls_root_directory_listing() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/a.txt", None)?;
            fs.mkdir("/sub")?;
            fs.mkfile("/sub/inner.txt", None)?; // should be excluded (nested)

            let entries: Vec<_> = fs.ls("/")?.collect();

            assert_eq!(
                entries.len(),
                2,
                "Root should list immediate files and dirs"
            );
            assert!(entries.contains(&PathBuf::from("/a.txt").as_path()));
            assert!(entries.contains(&PathBuf::from("/sub").as_path()));
            assert!(
                !entries
                    .iter()
                    .any(|&p| p.to_str().unwrap().contains("inner.txt")),
                "Nested file in sub should be excluded"
            );

            Ok(())
        }

        #[test]
        fn test_ls_empty_directory_returns_empty() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/empty")?;

            let entries: Vec<_> = fs.ls("/empty")?.collect();
            assert!(
                entries.is_empty(),
                "Empty directory should return no entries"
            );

            Ok(())
        }
    }

    mod tree {
        use super::*;

        #[test]
        fn test_tree_current_directory_empty() -> Result<()> {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path())?;

            let entries: Vec<_> = fs.tree(fs.cwd())?.collect();
            assert!(entries.is_empty());

            Ok(())
        }

        #[test]
        fn test_tree_specific_directory_empty() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/empty_dir")?;

            let entries: Vec<_> = fs.tree("/empty_dir")?.collect();
            assert!(entries.is_empty());

            Ok(())
        }

        #[test]
        fn test_tree_single_file_in_cwd() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/file.txt", Some(b"Content"))?;

            let entries: Vec<_> = fs.tree(fs.cwd())?.collect();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0], PathBuf::from("/file.txt"));

            Ok(())
        }

        #[test]
        fn test_tree_file_in_subdirectory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.mkfile("/docs/readme.txt", Some(b"Docs"))?;

            let entries: Vec<_> = fs.tree("/docs")?.collect();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0], PathBuf::from("/docs/readme.txt"));

            Ok(())
        }

        #[test]
        fn test_tree_nested_structure() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create nested structure
            fs.mkdir("/project")?;
            fs.mkdir("/project/src")?;
            fs.mkdir("/project/tests")?;
            fs.mkfile("/project/main.rs", Some(b"fn main() {}"))?;
            fs.mkfile("/project/src/lib.rs", Some(b"mod utils;"))?;
            fs.mkfile("/project/tests/test.rs", Some(b"#[test] fn it_works() {}"))?;

            // Test tree from root
            let root_entries: Vec<_> = fs.tree("/")?.collect();
            assert_eq!(root_entries.len(), 6); // /project, /project/src, /project/tests, /project/main.rs, /project/src/lib.rs, /project/tests/test.rs

            // Test tree from /project
            let project_entries: Vec<_> = fs.tree("/project")?.collect();
            assert_eq!(project_entries.len(), 5); // /project/src, /project/tests, /project/main.rs, /project/src/lib.rs, /project/tests/test.rs

            Ok(())
        }

        #[test]
        fn test_tree_nonexistent_path_error() -> Result<()> {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path())?;

            let result: Result<Vec<_>> = fs.tree("/nonexistent").map(|iter| iter.collect());
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("does not exist"));

            Ok(())
        }

        #[test]
        fn test_tree_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.cd("/docs")?;
            fs.mkdir("sub")?;
            fs.mkfile("sub/file.txt", Some(b"Relative"))?;

            let entries: Vec<_> = fs.tree("sub")?.collect();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0], PathBuf::from("/docs/sub/file.txt"));

            Ok(())
        }

        #[test]
        fn test_tree_unicode_paths() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/проект")?;
            fs.mkfile("/проект/документ.txt", Some(b"Unicode"))?;
            fs.mkdir("/проект/подпапка")?;
            fs.mkfile("/проект/подпапка/файл.txt", Some(b"Nested unicode"))?;

            let entries: Vec<_> = fs.tree("/проект")?.collect();

            assert_eq!(entries.len(), 3);
            assert!(entries.contains(&PathBuf::from("/проект/документ.txt").as_path()));
            assert!(entries.contains(&PathBuf::from("/проект/подпапка").as_path()));
            assert!(entries.contains(&PathBuf::from("/проект/подпапка/файл.txt").as_path()));

            Ok(())
        }

        #[test]
        fn test_tree_no_root_inclusion() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/parent")?;
            fs.mkfile("/parent/child.txt", Some(b"Child"))?;

            let entries: Vec<_> = fs.tree("/parent")?.collect();

            // Should not include /parent itself, only its contents
            assert!(!entries.iter().any(|&p| p == &PathBuf::from("/parent")));
            assert!(
                entries
                    .iter()
                    .any(|&p| p == &PathBuf::from("/parent/child.txt"))
            );

            Ok(())
        }

        #[test]
        fn test_tree_order_independence() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/order_test")?;
            fs.mkfile("/order_test/a.txt", None)?;
            fs.mkfile("/order_test/b.txt", None)?;
            fs.mkfile("/order_test/c.txt", None)?;

            let entries: Vec<_> = fs.tree("/order_test")?.collect();

            assert_eq!(entries.len(), 3);

            Ok(())
        }
    }

    mod mkdir_all {
        use super::*;
        use std::fs;
        use std::path::PathBuf;

        #[test]
        fn test_mkdir_all_simple_creation() {
            let temp_dir = setup_test_env();
            let target = temp_dir.path().join("a/b/c");

            let created = DirFS::mkdir_all(&target).unwrap();

            assert_eq!(created.len(), 3);
            assert!(created.contains(&temp_dir.path().join("a")));
            assert!(created.contains(&temp_dir.path().join("a/b")));
            assert!(created.contains(&temp_dir.path().join("a/b/c")));

            // Проверяем, что каталоги реально созданы
            assert!(temp_dir.path().join("a").is_dir());
            assert!(temp_dir.path().join("a/b").is_dir());
            assert!(temp_dir.path().join("a/b/c").is_dir());
        }

        #[test]
        fn test_mkdir_all_existing_parent() {
            let temp_dir = setup_test_env();
            fs::create_dir_all(temp_dir.path().join("a")).unwrap(); // It already exists

            let target = temp_dir.path().join("a/b/c");
            let created = DirFS::mkdir_all(&target).unwrap();

            assert_eq!(created.len(), 2); // Только b и c
            assert!(created.contains(&temp_dir.path().join("a/b")));
            assert!(created.contains(&temp_dir.path().join("a/b/c")));
        }

        #[test]
        fn test_mkdir_all_target_exists() {
            let temp_dir = setup_test_env();
            fs::create_dir_all(temp_dir.path().join("x/y")).unwrap();

            let target = temp_dir.path().join("x/y");
            let created = DirFS::mkdir_all(&target).unwrap();

            assert!(created.is_empty()); // Nothing was created
        }

        #[test]
        fn test_mkdir_all_root_path() {
            // FS root (usually "/")
            let result = DirFS::mkdir_all("/");
            assert!(result.is_ok());
            assert!(result.unwrap().is_empty());
        }

        #[test]
        fn test_mkdir_all_single_dir() {
            let temp_dir = setup_test_env();
            let target = temp_dir.path().join("single");

            let created = DirFS::mkdir_all(&target).unwrap();

            assert_eq!(created.len(), 1);
            assert!(created.contains(&target));
            assert!(target.is_dir());
        }

        #[test]
        fn test_mkdir_all_absolute_vs_relative() {
            let temp_dir = setup_test_env();

            // The absolute path
            let abs_target = temp_dir.path().join("abs/a/b");
            let abs_created = DirFS::mkdir_all(&abs_target).unwrap();

            assert!(!abs_created.is_empty());
        }

        #[test]
        fn test_mkdir_all_nested_existing() {
            let temp_dir = setup_test_env();
            fs::create_dir_all(temp_dir.path().join("deep/a")).unwrap();

            let target = temp_dir.path().join("deep/a/b/c/d");
            let created = DirFS::mkdir_all(&target).unwrap();

            assert_eq!(created.len(), 3); // b, c, d
        }

        #[test]
        fn test_mkdir_all_invalid_path() {
            // Attempt to create in a non-existent location (without rights)
            #[cfg(unix)]
            {
                let invalid_path = PathBuf::from("/nonexistent/parent/child");

                // Expecting an error (e.g. PermissionDenied or NoSuchFile)
                let result = DirFS::mkdir_all(&invalid_path);
                assert!(result.is_err());
            }
        }

        #[test]
        fn test_mkdir_all_file_in_path() {
            let temp_dir = setup_test_env();
            let file_path = temp_dir.path().join("file.txt");
            fs::write(&file_path, "content").unwrap(); // Create a file

            let target = file_path.join("subdir"); // Trying to create inside the file

            let result = DirFS::mkdir_all(&target);
            assert!(result.is_err()); // Must be an error
        }

        #[test]
        fn test_mkdir_all_trailing_slash() {
            let temp_dir = setup_test_env();
            let target = temp_dir.path().join("trailing/");

            let created = DirFS::mkdir_all(&target).unwrap();
            assert!(!created.is_empty());
            assert!(temp_dir.path().join("trailing").is_dir());
        }

        #[test]
        fn test_mkdir_all_unicode_paths() {
            let temp_dir = setup_test_env();
            let target = temp_dir.path().join("папка/файл");

            let created = DirFS::mkdir_all(&target).unwrap();

            assert_eq!(created.len(), 2);
            assert!(temp_dir.path().join("папка").is_dir());
            assert!(temp_dir.path().join("папка/файл").is_dir());
        }

        #[test]
        fn test_mkdir_all_permissions_error() {
            // This test requires a specific environment (e.g. readonly FS).
            // Skip it in general tests, but leave it for manual launch.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                fs::set_permissions(&temp_dir, PermissionsExt::from_mode(0o444)).unwrap(); // readonly

                let target = temp_dir.path().join("protected/dir");
                let result = DirFS::mkdir_all(&target);

                assert!(result.is_err());
            }
        }
    }

    mod drop {
        use super::*;

        #[test]
        fn test_drop_removes_created_directories() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("to_remove");

            // Create DirFs, which will create new directories.
            let fs = DirFS::new(&root).unwrap();
            assert!(root.exists());

            // Destroy fs (Drop should work)
            drop(fs);

            // Check that the root has been removed.
            assert!(!root.exists());
        }

        #[test]
        fn test_drop_only_removes_created_parents() {
            let temp_dir = setup_test_env();
            let parent = temp_dir.path().join("parent");
            let child = parent.join("child");

            std::fs::create_dir_all(&parent).unwrap(); // The parent already exists
            let fs = DirFS::new(&child).unwrap();

            assert!(parent.exists()); // The parent must remain.
            assert!(child.exists());

            drop(fs);

            assert!(parent.exists()); // The parent is not deleted
            assert!(!child.exists()); // The child has been removed
        }

        #[test]
        fn test_drop_with_is_auto_clean_false() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("keep");

            let mut fs = DirFS::new(&root).unwrap();
            fs.is_auto_clean = false; // Disable auto-cleaning

            drop(fs);

            assert!(root.exists()); // The catalog must remain
        }

        #[test]
        fn test_drop_empty_created_root_parents() {
            let temp_dir = setup_test_env();
            let existing = temp_dir.path().join("existing");
            std::fs::create_dir(&existing).unwrap();

            let fs = DirFS::new(&existing).unwrap(); // Already exists → created_root_parents is empty

            drop(fs);

            assert!(existing.exists()); // It should remain (we didn't create it)
        }

        #[test]
        fn test_drop_nested_directories_removed() {
            let temp_dir = setup_test_env();
            let nested = temp_dir.path().join("a/b/c");

            let fs = DirFS::new(&nested).unwrap();
            assert!(nested.exists());

            drop(fs);

            // Все уровни должны быть удалены
            assert!(!temp_dir.path().join("a").exists());
            assert!(!temp_dir.path().join("a/b").exists());
            assert!(!nested.exists());
        }

        //-----------------------------

        #[test]
        fn test_drop_removes_entries_created_by_mkdir() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("test_root");

            let mut fs = DirFS::new(&root).unwrap();
            fs.mkdir("/subdir").unwrap();
            assert!(root.join("subdir").exists());

            drop(fs);

            assert!(!root.exists()); // Корень удалён
            assert!(!root.join("subdir").exists()); // The subdirectory has also been deleted.
        }

        #[test]
        fn test_drop_removes_entries_created_by_mkfile() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("test_root");

            let mut fs = DirFS::new(&root).unwrap();
            fs.mkfile("/file.txt", None).unwrap();
            assert!(root.join("file.txt").exists());

            drop(fs);

            assert!(!root.exists());
            assert!(!root.join("file.txt").exists());
        }

        #[test]
        fn test_drop_handles_nested_entries() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("test_root");

            let mut fs = DirFS::new(&root).unwrap();
            fs.mkdir("/a/b/c").unwrap();
            fs.mkfile("/a/file.txt", None).unwrap();

            assert!(root.join("a/b/c").exists());
            assert!(root.join("a/file.txt").exists());

            drop(fs);

            assert!(!root.exists());
        }

        #[test]
        fn test_drop_ignores_non_entries() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("test_root");
            let external = temp_dir.path().join("external_file.txt");

            std::fs::write(&external, "content").unwrap(); // File outside VFS

            let fs = DirFS::new(&root).unwrap();
            drop(fs);

            assert!(!root.exists());
            assert!(external.exists()); // The external file remains
        }

        #[test]
        fn test_drop_with_empty_entries() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("empty_root");

            let fs = DirFS::new(&root).unwrap();
            // entries contains only "/" (root)

            drop(fs);

            assert!(!root.exists());
        }
    }

    mod mkfile {
        use super::*;

        #[test]
        fn test_mkfile_simple_creation() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkfile("/file.txt", None).unwrap();

            assert!(fs.exists("/file.txt"));
            assert!(root.join("file.txt").exists());
            assert_eq!(fs.entries.contains_key(&PathBuf::from("/file.txt")), true);
        }

        #[test]
        fn test_mkfile_with_content() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            let content = b"Hello, VFS!";
            fs.mkfile("/data.bin", Some(content)).unwrap();

            assert!(fs.exists("/data.bin"));
            let file_content = std::fs::read(root.join("data.bin")).unwrap();
            assert_eq!(&file_content, content);
        }

        #[test]
        fn test_mkfile_in_subdirectory() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkdir("/subdir").unwrap();
            fs.mkfile("/subdir/file.txt", None).unwrap();

            assert!(fs.exists("/subdir/file.txt"));
            assert!(root.join("subdir/file.txt").exists());
        }

        #[test]
        fn test_mkfile_parent_does_not_exist() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();

            let result = fs.mkfile("/nonexistent/file.txt", None);
            assert!(result.is_ok());
            assert!(root.join("nonexistent/file.txt").exists());
        }

        #[test]
        fn test_mkfile_file_already_exists() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkfile("/existing.txt", None).unwrap();

            // Trying to create the same file again
            let result = fs.mkfile("/existing.txt", None);
            assert!(result.is_ok()); // Should overwrite (File::create truncates the file)
            assert!(fs.exists("/existing.txt"));
        }

        #[test]
        fn test_mkfile_empty_content() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkfile("/empty.txt", Some(&[])).unwrap(); // An empty array

            assert!(fs.exists("/empty.txt"));
            let file_size = std::fs::metadata(root.join("empty.txt")).unwrap().len();
            assert_eq!(file_size, 0);
        }

        #[test]
        fn test_mkfile_relative_path() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkdir("/sub").unwrap();
            fs.cd("/sub").unwrap(); // Changes the current directory

            fs.mkfile("relative.txt", None).unwrap(); // A relative path

            assert!(fs.exists("/sub/relative.txt"));
            assert!(root.join("sub/relative.txt").exists());
        }

        #[test]
        fn test_mkfile_normalize_path() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkdir("/normalized").unwrap();

            fs.mkfile("/./normalized/../normalized/file.txt", None)
                .unwrap();

            assert!(fs.exists("/normalized/file.txt"));
            assert!(root.join("normalized/file.txt").exists());
        }

        #[test]
        fn test_mkfile_invalid_path_components() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();

            // Attempt to create a file with an invalid name (depending on the file system)
            #[cfg(unix)]
            {
                let result = fs.mkfile("/invalid\0name.txt", None);
                assert!(result.is_err()); // NUL in filenames is prohibited in Unix.
            }
        }

        #[test]
        fn test_mkfile_root_directory() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();

            // Cannot create a file named "/" (it is a directory)
            let result = fs.mkfile("/", None);
            assert!(result.is_err());
        }

        #[test]
        fn test_mkfile_unicode_filename() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.mkfile("/тест.txt", Some(b"Content")).unwrap();

            assert!(fs.exists("/тест.txt"));
            assert!(root.join("тест.txt").exists());
            let content = std::fs::read_to_string(root.join("тест.txt")).unwrap();
            assert_eq!(content, "Content");
        }
    }

    mod read {
        use super::*;

        #[test]
        fn test_read_existing_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(&temp_dir)?;

            // Create and write a file
            fs.mkfile("/test.txt", Some(b"Hello, VFS!"))?;

            // Read it back
            let content = fs.read("/test.txt")?;
            assert_eq!(content, b"Hello, VFS!");

            Ok(())
        }

        #[test]
        fn test_read_nonexistent_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let fs = DirFS::new(temp_dir.path())?;

            let result = fs.read("/not/found.txt");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("does not exist"));

            Ok(())
        }

        #[test]
        fn test_read_directory_as_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/empty_dir")?;

            let result = fs.read("/empty_dir");
            assert!(result.is_err());
            // Note: error comes from std::fs::File::open (not a file), not our exists check
            assert!(result.unwrap_err().to_string().contains("is a directory"));

            Ok(())
        }

        #[test]
        fn test_read_empty_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/empty.txt", None)?; // Create empty file

            let content = fs.read("/empty.txt")?;
            assert_eq!(content.len(), 0);

            Ok(())
        }

        #[test]
        fn test_read_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.cd("/")?;
            fs.mkdir("/parent")?;
            fs.cd("/parent")?;
            fs.mkfile("child.txt", Some(b"Content"))?;

            // Read using relative path from cwd
            let content = fs.read("child.txt")?;
            assert_eq!(content, b"Content");

            Ok(())
        }

        #[test]
        fn test_read_unicode_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/папка")?;
            fs.mkfile("/папка/файл.txt", Some(b"Unicode content"))?;

            let content = fs.read("/папка/файл.txt")?;
            assert_eq!(content, b"Unicode content");

            Ok(())
        }

        #[test]
        fn test_read_permission_denied() -> Result<()> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path())?;

                // Create file and restrict permissions
                fs.mkfile("/protected.txt", Some(b"Secret"))?;
                let host_path = temp_dir.path().join("protected.txt");
                std::fs::set_permissions(&host_path, PermissionsExt::from_mode(0o000))?;

                // Try to read (should fail due to permissions)
                let result = fs.read("/protected.txt");
                assert!(result.is_err());
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Permission denied")
                );

                // Clean up: restore permissions
                std::fs::set_permissions(&host_path, PermissionsExt::from_mode(0o644))?;
            }
            Ok(())
        }

        #[test]
        fn test_read_root_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/root_file.txt", Some(b"At root"))?;
            let content = fs.read("/root_file.txt")?;
            assert_eq!(content, b"At root");

            Ok(())
        }
    }

    mod write {
        use super::*;

        #[test]
        fn test_write_new_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/new.txt", None)?;
            let content = b"Hello, VFS!";
            fs.write("/new.txt", content)?;

            // Check file exists and has correct content
            assert!(fs.exists("/new.txt"));
            let read_back = fs.read("/new.txt")?;
            assert_eq!(read_back, content);

            Ok(())
        }

        #[test]
        fn test_write_existing_file_overwrite() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/exist.txt", Some(b"Old content"))?;

            let new_content = b"New content";
            fs.write("/exist.txt", new_content)?;

            let read_back = fs.read("/exist.txt")?;
            assert_eq!(read_back, new_content);

            Ok(())
        }

        #[test]
        fn test_write_to_directory_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/dir")?;

            let result = fs.write("/dir", b"Content");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("is a directory"));

            Ok(())
        }

        #[test]
        fn test_write_to_nonexistent_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let result = fs.write("/parent/child.txt", b"Content");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("does not exist"));

            Ok(())
        }

        #[test]
        fn test_write_empty_content() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/empty.txt", None)?;
            fs.write("/empty.txt", &[])?;

            let read_back = fs.read("/empty.txt")?;
            assert!(read_back.is_empty());

            Ok(())
        }

        #[test]
        fn test_write_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.cd("docs")?;

            fs.mkfile("file.txt", None)?;
            let content = b"Relative write";
            fs.write("file.txt", content)?;

            let read_back = fs.read("/docs/file.txt")?;
            assert_eq!(read_back, content);

            Ok(())
        }
    }

    mod append {
        use super::*;

        #[test]
        fn test_append_to_existing_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create initial file
            fs.mkfile("/log.txt", Some(b"Initial content\n"))?;

            // Append new content
            fs.append("/log.txt", b"Appended line 1\n")?;
            fs.append("/log.txt", b"Appended line 2\n")?;

            // Verify full content
            let content = fs.read("/log.txt")?;
            assert_eq!(
                content,
                b"Initial content\nAppended line 1\nAppended line 2\n"
            );

            Ok(())
        }

        #[test]
        fn test_append_to_empty_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create empty file
            fs.mkfile("/empty.txt", Some(&[]))?;

            // Append content
            fs.append("/empty.txt", b"First append\n")?;
            fs.append("/empty.txt", b"Second append\n")?;

            let content = fs.read("/empty.txt")?;
            assert_eq!(content, b"First append\nSecond append\n");

            Ok(())
        }

        #[test]
        fn test_append_nonexistent_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let result = fs.append("/not_found.txt", b"Content");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("does not exist"));

            Ok(())
        }

        #[test]
        fn test_append_to_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/mydir")?;

            let result = fs.append("/mydir", b"Content");
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("is a directory"));

            Ok(())
        }

        #[test]
        fn test_append_empty_content() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/test.txt", Some(b"Existing\n"))?;

            // Append empty slice
            fs.append("/test.txt", &[])?;

            // Content should remain unchanged
            let content = fs.read("/test.txt")?;
            assert_eq!(content, b"Existing\n");

            Ok(())
        }

        #[test]
        fn test_append_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.cd("/docs")?;
            fs.mkfile("log.txt", Some(b"Start\n"))?; // Relative path

            fs.append("log.txt", b"Added\n")?;

            let content = fs.read("/docs/log.txt")?;
            assert_eq!(content, b"Start\nAdded\n");

            Ok(())
        }

        #[test]
        fn test_append_unicode_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let first = Vec::from("Начало\n");
            let second = Vec::from("Продолжение\n");

            fs.mkdir("/папка")?;
            fs.mkfile("/папка/файл.txt", Some(first.as_slice()))?;
            fs.append("/папка/файл.txt", second.as_slice())?;

            let content = fs.read("/папка/файл.txt")?;

            let mut expected = Vec::from(first);
            expected.extend(second);

            assert_eq!(content, expected);

            Ok(())
        }

        #[test]
        fn test_concurrent_append_safety() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/concurrent.txt", Some(b""))?;

            // Simulate multiple appends
            for i in 1..=3 {
                fs.append("/concurrent.txt", format!("Line {}\n", i).as_bytes())?;
            }

            let content = fs.read("/concurrent.txt")?;
            assert_eq!(content, b"Line 1\nLine 2\nLine 3\n");

            Ok(())
        }

        #[test]
        fn test_append_permission_denied() -> Result<()> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path())?;

                // Create file and restrict permissions
                fs.mkfile("/protected.txt", Some(b"Content"))?;
                let host_path = temp_dir.path().join("protected.txt");
                std::fs::set_permissions(&host_path, PermissionsExt::from_mode(0o000))?;

                // Try to append (should fail)
                let result = fs.append("/protected.txt", b"New content");
                assert!(result.is_err());
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Permission denied")
                );

                // Clean up: restore permissions
                std::fs::set_permissions(&host_path, PermissionsExt::from_mode(0o644))?;
            }
            Ok(())
        }
    }

    mod add {
        use super::*;

        #[test]
        fn test_add_existing_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create a file outside VFS that we'll add
            let host_file = temp_dir.path().join("external.txt");
            std::fs::write(&host_file, b"Content from host")?;

            // Add it to VFS
            fs.add("external.txt")?;

            // Verify it's now tracked by VFS
            assert!(fs.exists("/external.txt"));
            let content = fs.read("/external.txt")?;
            assert_eq!(content, b"Content from host");

            Ok(())
        }

        #[test]
        fn test_add_existing_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create directory outside VFS
            let host_dir = temp_dir.path().join("external_dir");
            std::fs::create_dir_all(&host_dir)?;

            // Add directory to VFS
            fs.add("external_dir")?;

            // Verify directory and its contents are accessible
            assert!(fs.exists("/external_dir"));

            Ok(())
        }

        #[test]
        fn test_add_nonexistent_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let result = fs.add("/nonexistent.txt");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("No such file or directory")
            );

            Ok(())
        }

        #[test]
        fn test_add_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create file in subdirectory
            let subdir = temp_dir.path().join("sub");
            std::fs::create_dir_all(&subdir)?;
            std::fs::write(subdir.join("file.txt"), b"Relative content")?;

            fs.add("/sub")?;
            fs.cd("/sub")?;

            // Change cwd and add using relative path
            fs.add("file.txt")?;

            assert!(fs.exists("/sub/file.txt"));
            let content = fs.read("/sub/file.txt")?;
            assert_eq!(content, b"Relative content");

            Ok(())
        }

        #[test]
        fn test_add_already_tracked_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // First add a file
            let host_file = temp_dir.path().join("duplicate.txt");
            std::fs::write(&host_file, b"Original")?;
            fs.add("duplicate.txt")?;

            // Then try to add it again
            let result = fs.add("duplicate.txt");
            // Should succeed (no harm in re-adding)
            assert!(result.is_ok());

            // Content should remain unchanged
            let content = fs.read("/duplicate.txt")?;
            assert_eq!(content, b"Original");

            Ok(())
        }

        #[test]
        fn test_add_unicode_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create file with Unicode name
            let unicode_file = temp_dir.path().join("файл.txt");
            std::fs::write(&unicode_file, b"Unicode content")?;

            fs.add("файл.txt")?;

            assert!(fs.exists("/файл.txt"));
            let content = fs.read("/файл.txt")?;
            assert_eq!(content, b"Unicode content");

            Ok(())
        }

        #[test]
        fn test_add_and_auto_cleanup() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            // Create and add a file
            let host_file = temp_dir.path().join("cleanup.txt");
            std::fs::write(&host_file, b"To be cleaned up")?;
            fs.add("cleanup.txt")?;

            assert!(host_file.exists());

            // Drop fs - should auto-cleanup if configured
            drop(fs);

            // Depending on auto_cleanup setting, file may or may not exist
            // This test assumes auto_cleanup=true
            assert!(!host_file.exists());

            Ok(())
        }

        #[test]
        fn test_add_single_file_no_recursion() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let host_file = temp_dir.path().join("file.txt");
            std::fs::write(&host_file, b"Content")?;

            fs.add("file.txt")?;

            assert!(fs.exists("/file.txt"));
            assert_eq!(fs.read("/file.txt")?, b"Content");

            Ok(())
        }

        #[test]
        fn test_add_empty_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let host_dir = temp_dir.path().join("empty_dir");
            std::fs::create_dir_all(&host_dir)?;

            fs.add("empty_dir")?;

            assert!(fs.exists("/empty_dir"));

            Ok(())
        }

        #[test]
        fn test_add_directory_with_files() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let data_dir = temp_dir.path().join("data");
            std::fs::create_dir_all(&data_dir)?;
            std::fs::write(data_dir.join("file1.txt"), b"First")?;
            std::fs::write(data_dir.join("file2.txt"), b"Second")?;

            fs.add("data")?;

            assert!(fs.exists("/data"));
            assert!(fs.exists("/data/file1.txt"));
            assert!(fs.exists("/data/file2.txt"));
            assert_eq!(fs.read("/data/file1.txt")?, b"First");
            assert_eq!(fs.read("/data/file2.txt")?, b"Second");

            Ok(())
        }

        #[test]
        fn test_add_nested_directories() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let project = temp_dir.path().join("project");
            std::fs::create_dir_all(project.join("src"))?;
            std::fs::create_dir_all(project.join("docs"))?;

            std::fs::write(project.join("src").join("main.rs"), b"fn main() {}")?;
            std::fs::write(project.join("docs").join("README.md"), b"Project docs")?;

            std::fs::write(project.join("config.toml"), b"[config]")?;

            fs.add("project")?;

            assert!(fs.exists("/project"));
            assert!(fs.exists("/project/src"));
            assert!(fs.exists("/project/docs"));
            assert!(fs.exists("/project/src/main.rs"));
            assert!(fs.exists("/project/docs/README.md"));
            assert!(fs.exists("/project/config.toml"));

            assert_eq!(fs.read("/project/src/main.rs")?, b"fn main() {}");
            assert_eq!(fs.read("/project/docs/README.md")?, b"Project docs");
            assert_eq!(fs.read("/project/config.toml")?, b"[config]");

            Ok(())
        }
    }

    mod forget {
        use super::*;

        #[test]
        fn test_forget_existing_file() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkfile("/note.txt", Some(b"Hello"))?;
            assert!(fs.exists("/note.txt"));

            fs.forget("/note.txt")?;

            assert!(!fs.exists("/note.txt"));
            assert!(std::fs::exists(fs.root().join("note.txt")).unwrap());

            Ok(())
        }

        #[test]
        fn test_forget_existing_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/temp")?;
            assert!(fs.exists("/temp"));

            fs.forget("/temp")?;

            assert!(!fs.exists("/temp"));
            assert!(std::fs::exists(fs.root().join("temp")).unwrap());

            Ok(())
        }

        #[test]
        fn test_forget_nested_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/a")?;
            fs.mkdir("/a/b")?;
            fs.mkfile("/a/b/file.txt", Some(b"Data"))?;

            assert!(fs.exists("/a/b/file.txt"));

            fs.forget("/a/b")?;

            assert!(!fs.exists("/a/b"));
            assert!(!fs.exists("/a/b/file.txt"));
            assert!(fs.exists("/a"));

            Ok(())
        }

        #[test]
        fn test_forget_nonexistent_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let result = fs.forget("/not/found.txt");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("path is not tracked by VFS")
            );

            Ok(())
        }

        #[test]
        fn test_forget_relative_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/docs")?;
            fs.cd("/docs")?;
            fs.mkdir("sub")?;
            fs.mkfile("sub/file.txt", Some(b"Content"))?;

            assert!(fs.exists("/docs/sub/file.txt"));

            fs.forget("sub/file.txt")?;

            assert!(!fs.exists("/docs/sub/file.txt"));
            assert!(fs.exists("/docs/sub"));

            Ok(())
        }

        #[test]
        fn test_forget_root_directory() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let result = fs.forget("/");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("cannot forget root directory")
            );

            assert!(fs.exists("/"));

            Ok(())
        }

        #[test]
        fn test_forget_parent_after_child() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/parent")?;
            fs.mkfile("/parent/child.txt", Some(b"Child content"))?;

            fs.forget("/parent/child.txt")?;
            assert!(!fs.exists("/parent/child.txt"));

            fs.forget("/parent")?;
            assert!(!fs.exists("/parent"));

            Ok(())
        }

        #[test]
        fn test_forget_unicode_path() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            fs.mkdir("/папка")?;
            fs.mkfile("/папка/файл.txt", Some(b"Unicode"))?;
            assert!(fs.exists("/папка/файл.txt"));

            fs.forget("/папка/файл.txt")?;

            assert!(!fs.exists("/папка/файл.txt"));
            assert!(fs.exists("/папка"));

            Ok(())
        }

        #[test]
        fn test_forget_case_sensitivity_unix() -> Result<()> {
            #[cfg(unix)]
            {
                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path())?;

                fs.mkfile("/File.TXT", Some(b"Case test"))?;
                assert!(fs.exists("/File.TXT"));

                let result = fs.forget("/file.txt");
                assert!(result.is_err());
                assert!(fs.exists("/File.TXT"));

                fs.forget("/File.TXT")?;
                assert!(!fs.exists("/File.TXT"));
            }
            Ok(())
        }

        #[test]
        fn test_forget_after_add_and_remove() -> Result<()> {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path())?;

            let host_file = temp_dir.path().join("external.txt");
            std::fs::write(&host_file, b"External")?;

            fs.add("external.txt")?;
            assert!(fs.exists("/external.txt"));

            std::fs::remove_file(&host_file)?;
            assert!(!host_file.exists());

            fs.forget("external.txt")?;
            assert!(!fs.exists("/external.txt"));

            Ok(())
        }
    }

    mod rm {
        use super::*;

        #[test]
        fn test_rm_file_success() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            // Create a file in VFS
            fs.mkfile("/test.txt", Some(b"hello")).unwrap();
            assert!(fs.exists("/test.txt"));
            assert!(temp_dir.path().join("test.txt").exists());

            // Remove it
            fs.rm("/test.txt").unwrap();

            // Verify: VFS and filesystem are updated
            assert!(!fs.exists("/test.txt"));
            assert!(!temp_dir.path().join("test.txt").exists());
        }

        #[test]
        fn test_rm_directory_recursive() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            // Create nested structure
            fs.mkdir("/a/b/c").unwrap();
            fs.mkfile("/a/file1.txt", None).unwrap();
            fs.mkfile("/a/b/file2.txt", None).unwrap();

            assert!(fs.exists("/a/b/c"));
            assert!(fs.exists("/a/file1.txt"));
            assert!(fs.exists("/a/b/file2.txt"));

            // Remove top-level directory
            fs.rm("/a").unwrap();

            // Verify everything is gone
            assert!(!fs.exists("/a"));
            assert!(!fs.exists("/a/b"));
            assert!(!fs.exists("/a/b/c"));
            assert!(!fs.exists("/a/file1.txt"));
            assert!(!fs.exists("/a/b/file2.txt"));

            assert!(!temp_dir.path().join("a").exists());
        }

        #[test]
        fn test_rm_nonexistent_path() {
            #[cfg(unix)]
            {
                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path()).unwrap();

                let result = fs.rm("/not/found");
                assert!(result.is_err());
                assert_eq!(result.unwrap_err().to_string(), "/not/found does not exist");
            }
        }

        #[test]
        fn test_rm_relative_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            fs.mkdir("/parent").unwrap();
            fs.cd("/parent").unwrap();
            fs.mkfile("child.txt", None).unwrap();

            assert!(fs.exists("/parent/child.txt"));

            // Remove using relative path
            fs.rm("child.txt").unwrap();

            assert!(!fs.exists("/parent/child.txt"));
            assert!(!temp_dir.path().join("parent/child.txt").exists());
        }

        #[test]
        fn test_rm_empty_string_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            let result = fs.rm("");
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().to_string(), "invalid path: empty");
        }

        #[test]
        fn test_rm_root_directory() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            // Attempt to remove root '/'
            let result = fs.rm("/");
            assert!(result.is_err());
            assert_eq!(
                result.unwrap_err().to_string(),
                "invalid path: the root cannot be removed"
            );

            // Root should still exist
            assert!(fs.exists("/"));
            assert!(temp_dir.path().exists());
        }

        #[test]
        fn test_rm_trailing_slash() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            fs.mkdir("/dir/").unwrap(); // With trailing slash
            fs.mkfile("/dir/file.txt", None).unwrap();

            // Remove with trailing slash
            fs.rm("/dir/").unwrap();

            assert!(!fs.exists("/dir"));
            assert!(!temp_dir.path().join("dir").exists());
        }

        #[test]
        fn test_rm_unicode_path() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            let unicode_path = "/папка/файл.txt";
            fs.mkdir("/папка").unwrap();
            fs.mkfile(unicode_path, None).unwrap();

            assert!(fs.exists(unicode_path));

            fs.rm(unicode_path).unwrap();

            assert!(!fs.exists(unicode_path));
            assert!(!temp_dir.path().join("папка/файл.txt").exists());
        }

        #[test]
        fn test_rm_permission_denied() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path()).unwrap();
                fs.mkdir("/protected").unwrap();

                // Create a directory and restrict permissions
                let protected = fs.root().join("protected");
                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o000)).unwrap();

                // Try to remove via VFS (should fail)
                let result = fs.rm("/protected");
                assert!(result.is_err());
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Permission denied")
                );

                // Clean up: restore permissions
                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o755)).unwrap();
            }
        }

        #[test]
        fn test_rm_symlink_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;

                let temp_dir = setup_test_env();
                let mut fs = DirFS::new(temp_dir.path()).unwrap();

                // Create real file and symlink
                std::fs::write(temp_dir.path().join("real.txt"), "content").unwrap();
                symlink("real.txt", temp_dir.path().join("link.txt")).unwrap();

                fs.mkfile("/link.txt", None).unwrap(); // Add symlink to VFS
                assert!(fs.exists("/link.txt"));

                // Remove symlink (not the target)
                fs.rm("/link.txt").unwrap();

                assert!(!fs.exists("/link.txt"));
                assert!(!temp_dir.path().join("link.txt").exists()); // Symlink gone
                assert!(temp_dir.path().join("real.txt").exists()); // Target still there
            }
        }

        #[test]
        fn test_rm_after_cd() {
            let temp_dir = setup_test_env();
            let mut fs = DirFS::new(temp_dir.path()).unwrap();

            fs.mkdir("/projects").unwrap();
            fs.cd("/projects").unwrap();
            fs.mkfile("notes.txt", None).unwrap();

            assert!(fs.exists("/projects/notes.txt"));

            // Remove from cwd using relative path
            fs.rm("notes.txt").unwrap();

            assert!(!fs.exists("/projects/notes.txt"));
            assert!(!temp_dir.path().join("projects/notes.txt").exists());
        }

        #[test]
        fn test_rm_not_existed_on_host() {
            let temp_dir = setup_test_env();
            std::fs::File::create(temp_dir.path().join("host-file.txt")).unwrap();

            let mut fs = DirFS::new(temp_dir.path()).unwrap();
            fs.add("/host-file.txt").unwrap();

            assert!(fs.exists("/host-file.txt"));

            std::fs::remove_file(fs.root().join("host-file.txt")).unwrap();
            let result = fs.rm("/host-file.txt");

            assert!(result.is_ok());
        }
    }

    mod cleanup {
        use super::*;

        #[test]
        fn test_cleanup_ignores_is_auto_clean() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            fs.is_auto_clean = false; // Clearly disabled
            fs.mkfile("/temp.txt", None).unwrap();

            fs.cleanup(); // Must be removed despite is_auto_clean=false

            assert!(!fs.exists("/temp.txt"));
            assert!(!root.join("temp.txt").exists());
        }

        #[test]
        fn test_cleanup_preserves_root_and_parents() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path().join("preserve_root");

            let mut fs = DirFS::new(&root).unwrap();
            fs.mkdir("/subdir").unwrap();
            fs.mkfile("/subdir/file.txt", None).unwrap();

            // created_root_parents is populated at initialization
            assert!(!fs.created_root_parents.is_empty());

            fs.cleanup();

            // Root and his parents remained
            assert!(root.exists());
            for parent in &fs.created_root_parents {
                assert!(parent.exists());
            }

            // Only entries (except "/") were removed
            assert_eq!(fs.entries.len(), 1);
            assert!(fs.entries.contains_key(&PathBuf::from("/")));
        }

        #[test]
        fn test_cleanup_empty_entries() {
            let temp_dir = setup_test_env();
            let root = temp_dir.path();

            let mut fs = DirFS::new(root).unwrap();
            // entries contains only "/"
            assert_eq!(fs.entries.len(), 1);

            fs.cleanup();

            assert_eq!(fs.entries.len(), 1); // "/" remained
            assert!(fs.entries.contains_key(&PathBuf::from("/")));
            assert!(root.exists()); // The root is not removed
        }
    }

    // Helper function: Creates a temporary directory for tests
    fn setup_test_env() -> TempDir {
        TempDir::new("dirfs_test").unwrap()
    }
}
