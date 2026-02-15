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
        let inner_root = PathBuf::from("/");
        let mut entries = BTreeMap::new();
        entries.insert(inner_root.clone(), Entry::new(EntryType::Directory));

        Self {
            root: PathBuf::from("/"),
            cwd: PathBuf::from("/"),
            entries,
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
        if !self.is_dir(&target)? {
            return Err(anyhow!("{} not a directory", target.display()));
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
        let is_file = self.is_file(&inner_path)?;
        let component_count = if is_file {
            inner_path.components().count()
        } else {
            inner_path.components().count() + 1
        };
        Ok(self
            .entries
            .iter()
            .map(|(pb, _)| pb.as_path())
            .filter(move |&path| {
                path.starts_with(&inner_path)
                    && (path != inner_path || is_file)
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
        let is_file = self.is_file(&inner_path)?;
        Ok(self
            .entries
            .iter()
            .map(|(pb, _)| pb.as_path())
            .filter(move |&path| path.starts_with(&inner_path) && (path != inner_path || is_file)))
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
        if self.exists(&file_path) {
            return Err(anyhow!("{} already exist", file_path.display()));
        }
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

    mod cd {
        use super::*;

        /// Helper function to set up a test VFS with a predefined structure
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new(); // Assume MapFS has a new() constructor

            // Create a sample directory structure
            vfs.mkdir("/home").unwrap();
            vfs.mkdir("/home/user").unwrap();
            vfs.mkdir("/etc").unwrap();
            vfs.mkfile("/home/user/config.txt", Some(b"Config content"))
                .unwrap();

            vfs
        }

        #[test]
        fn test_cd_absolute_path_success() -> Result<()> {
            let mut vfs = setup_test_vfs();

            assert_eq!(vfs.cwd, Path::new("/")); // Initial CWD is root

            vfs.cd("/home/user")?;

            assert_eq!(vfs.cwd, Path::new("/home/user"));
            Ok(())
        }

        #[test]
        fn test_cd_relative_path_success() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.cd("/home")?; // Change to /home first
            assert_eq!(vfs.cwd, Path::new("/home"));

            vfs.cd("user")?; // Relative path from current CWD

            assert_eq!(vfs.cwd, Path::new("/home/user"));
            Ok(())
        }

        #[test]
        fn test_cd_root_directory() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.cd("/")?;

            assert_eq!(vfs.cwd, Path::new("/"));
            Ok(())
        }

        #[test]
        fn test_cd_nonexistent_path_error() -> Result<()> {
            let mut vfs = setup_test_vfs();

            let result = vfs.cd("/nonexistent/path");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error message should indicate path does not exist"
            );

            // CWD should remain unchanged
            assert_eq!(vfs.cwd, Path::new("/"));
            Ok(())
        }

        #[test]
        fn test_cd_file_path_error() -> Result<()> {
            let mut vfs = setup_test_vfs();

            let result = vfs.cd("/home/user/config.txt");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("not a directory"),
                "Even though the file exists, cd() should fail because it's not a directory"
            );

            // CWD should remain unchanged
            assert_eq!(vfs.cwd, Path::new("/"));
            Ok(())
        }

        #[test]
        fn test_cd_same_directory() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.cd("/home")?;
            assert_eq!(vfs.cwd, Path::new("/home"));

            vfs.cd("/home")?; // CD to same directory

            assert_eq!(vfs.cwd, Path::new("/home")); // Should remain unchanged
            Ok(())
        }

        #[test]
        fn test_cd_deep_nested_path() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.cd("/home/user")?;

            assert_eq!(vfs.cwd, Path::new("/home/user"));
            Ok(())
        }

        #[test]
        fn test_cd_sequential_changes() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.cd("/etc")?;
            assert_eq!(vfs.cwd, Path::new("/etc"));

            vfs.cd("/home")?;
            assert_eq!(vfs.cwd, Path::new("/home"));

            vfs.cd("/")?;
            assert_eq!(vfs.cwd, Path::new("/"));

            Ok(())
        }

        #[test]
        fn test_cd_with_trailing_slash() -> Result<()> {
            let mut vfs = setup_test_vfs();

            // Test that trailing slash is handled correctly
            vfs.cd("/home/")?;
            assert_eq!(vfs.cwd, Path::new("/home"));

            vfs.cd("/home/user//")?;
            assert_eq!(vfs.cwd, Path::new("/home/user"));
            Ok(())
        }
    }

    mod exists {
        use super::*;

        /// Helper to create a pre‑populated MapFS instance for testing
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new();

            // Create a sample hierarchy
            vfs.mkdir("/etc").unwrap();
            vfs.mkdir("/home").unwrap();
            vfs.mkdir("/home/user").unwrap();
            vfs.mkfile("/home/user/file.txt", Some(b"Hello")).unwrap();
            vfs.mkfile("/readme.md", Some(b"Project docs")).unwrap();

            vfs
        }

        #[test]
        fn test_exists_absolute_path_file() {
            let vfs = setup_test_vfs();
            assert!(vfs.exists("/home/user/file.txt"));
        }

        #[test]
        fn test_exists_absolute_path_directory() {
            let vfs = setup_test_vfs();
            assert!(vfs.exists("/home/user"));
        }

        #[test]
        fn test_exists_root_directory() {
            let vfs = setup_test_vfs();
            assert!(vfs.exists("/"));
        }

        #[test]
        fn test_exists_relative_path_from_root() {
            let vfs = setup_test_vfs();
            // Current CWD is "/" by default
            assert!(vfs.exists("home/user/file.txt"));
        }

        #[test]
        fn test_exists_relative_path_nested() {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap(); // Change CWD
            assert!(vfs.exists("file.txt")); // Relative to current CWD
        }

        #[test]
        fn test_exists_nonexistent_file() {
            let vfs = setup_test_vfs();
            assert!(!vfs.exists("/home/user/nonexistent.txt"));
        }

        #[test]
        fn test_exists_nonexistent_directory() {
            let vfs = setup_test_vfs();
            assert!(!vfs.exists("/tmp"));
        }

        #[test]
        fn test_exists_partial_path() {
            let vfs = setup_test_vfs();
            // "/home/us" is not a complete path in our hierarchy
            assert!(!vfs.exists("/home/us"));
        }

        #[test]
        fn test_exists_with_trailing_slash() {
            let vfs = setup_test_vfs();
            assert!(vfs.exists("/home/")); // Should normalize to /home
            assert!(vfs.exists("/home/user/")); // Should normalize to /home/user
            assert!(vfs.exists("/readme.md/")); // File with trailing slash
        }

        #[test]
        fn test_exists_case_sensitivity() {
            #[cfg(unix)]
            {
                let mut vfs = setup_test_vfs();
                // Add a mixed-case path
                vfs.mkdir("/Home/User").unwrap();

                assert!(vfs.exists("/Home/User"));
                assert!(!vfs.exists("/home/User")); // Different case
            }
        }

        #[test]
        fn test_exists_empty_string() {
            let vfs = setup_test_vfs();
            // Empty string should resolve to CWD (which is "/")
            assert!(vfs.exists(""));
        }

        #[test]
        fn test_exists_dot_path() {
            let vfs = setup_test_vfs();
            assert!(vfs.exists(".")); // Current directory
            assert!(vfs.exists("./home")); // Relative with dot
        }

        #[test]
        fn test_exists_double_dot_path() {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();
            assert!(vfs.exists("..")); // Parent directory
            assert!(vfs.exists("../user")); // Sibling
            assert!(vfs.exists("../../etc")); // Up two levels
        }
    }

    mod is_dir_file {
        use super::*;

        /// Helper to create a pre‑populated MapFS instance for testing
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new();

            // Create a sample hierarchy
            vfs.mkdir("/etc").unwrap();
            vfs.mkdir("/home").unwrap();
            vfs.mkdir("/home/user").unwrap();
            vfs.mkfile("/home/user/file.txt", Some(b"Hello")).unwrap();
            vfs.mkfile("/readme.md", Some(b"Project docs")).unwrap();
            vfs.mkfile("/empty.bin", None).unwrap(); // Empty file

            vfs
        }

        #[test]
        fn test_is_dir_existing_directory_absolute() -> Result<()> {
            let vfs = setup_test_vfs();
            assert!(vfs.is_dir("/home")?);
            assert!(vfs.is_dir("/home/user")?);
            assert!(vfs.is_dir("/")?); // Root
            Ok(())
        }

        #[test]
        fn test_is_dir_existing_directory_relative() -> Result<()> {
            let vfs = setup_test_vfs();
            // From root
            assert!(vfs.is_dir("home")?);
            // After changing CWD
            let mut vfs2 = setup_test_vfs();
            vfs2.cd("/home").unwrap();
            assert!(vfs2.is_dir("user")?);
            Ok(())
        }

        #[test]
        fn test_is_dir_file_path() -> Result<()> {
            let vfs = setup_test_vfs();
            assert!(!vfs.is_dir("/home/user/file.txt")?);
            assert!(!vfs.is_dir("/readme.md")?);
            Ok(())
        }

        #[test]
        fn test_is_dir_nonexistent_path() {
            let vfs = setup_test_vfs();
            let result = vfs.is_dir("/nonexistent");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should mention path does not exist"
            );
        }

        #[test]
        fn test_is_file_existing_file_absolute() -> Result<()> {
            let vfs = setup_test_vfs();
            assert!(vfs.is_file("/home/user/file.txt")?);
            assert!(vfs.is_file("/readme.md")?);
            assert!(vfs.is_file("/empty.bin")?); // Empty file is still a file
            Ok(())
        }

        #[test]
        fn test_is_file_existing_file_relative() -> Result<()> {
            let vfs = setup_test_vfs();
            // From root
            assert!(vfs.is_file("readme.md")?);
            // After changing CWD
            let mut vfs2 = setup_test_vfs();
            vfs2.cd("/home/user").unwrap();
            assert!(vfs2.is_file("file.txt")?);
            Ok(())
        }

        #[test]
        fn test_is_file_directory_path() -> Result<()> {
            let vfs = setup_test_vfs();
            assert!(!vfs.is_file("/home")?);
            assert!(!vfs.is_file("/home/user")?);
            assert!(!vfs.is_file("/")?); // Root is a directory
            Ok(())
        }

        #[test]
        fn test_is_file_nonexistent_path() {
            let vfs = setup_test_vfs();
            let result = vfs.is_file("/nonexistent.txt");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should mention path does not exist"
            );
        }

        #[test]
        fn test_is_dir_and_is_file_on_same_file() -> Result<()> {
            let vfs = setup_test_vfs();
            let file_path = "/home/user/file.txt";

            assert!(!vfs.is_dir(file_path)?); // Not a directory
            assert!(vfs.is_file(file_path)?); // Is a file

            Ok(())
        }

        #[test]
        fn test_is_dir_and_is_file_on_same_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let dir_path = "/home/user";

            assert!(vfs.is_dir(dir_path)?); // Is a directory
            assert!(!vfs.is_file(dir_path)?); // Not a file

            Ok(())
        }

        #[test]
        fn test_is_dir_with_trailing_slash() -> Result<()> {
            let vfs = setup_test_vfs();
            assert!(vfs.is_dir("/home/")?); // Trailing slash
            assert!(vfs.is_dir("/home/user/")?);
            Ok(())
        }

        #[test]
        fn test_is_file_with_trailing_slash() -> Result<()> {
            let vfs = setup_test_vfs();
            // Even with trailing slash, it should still be recognized as a file
            assert!(vfs.is_file("/readme.md/")?);
            assert!(vfs.is_file("/home/user/file.txt/")?);
            Ok(())
        }

        #[test]
        fn test_is_dir_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home").unwrap();

            assert!(vfs.is_dir(".")?); // Current directory
            assert!(vfs.is_dir("./user")?); // Subdirectory
            Ok(())
        }

        #[test]
        fn test_is_file_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();

            assert!(vfs.is_file("./file.txt")?);
            Ok(())
        }

        #[test]
        fn test_is_dir_double_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();

            assert!(vfs.is_dir("..")?); // Parent (/home)

            let result = vfs.is_dir("../etc");
            assert!(result.is_err()); // Sibling directory (not existed)
            // Note: ../etc doesn't exist in our setup, so this would fail
            // But .. itself should pass
            Ok(())
        }
    }

    mod ls {
        use super::*;

        /// Helper to create a pre‑populated MapFS instance for testing
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new();

            // Create a sample hierarchy
            vfs.mkdir("/etc").unwrap();
            vfs.mkdir("/home").unwrap();
            vfs.mkdir("/home/user").unwrap();
            vfs.mkdir("/home/guest").unwrap();
            vfs.mkfile("/home/user/file1.txt", Some(b"Content 1"))
                .unwrap();
            vfs.mkfile("/home/user/file2.txt", Some(b"Content 2"))
                .unwrap();
            vfs.mkfile("/home/guest/note.txt", Some(b"Note")).unwrap();
            vfs.mkfile("/readme.md", Some(b"Docs")).unwrap();

            vfs
        }

        #[test]
        fn test_ls_root_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.ls("/")?.collect();

            assert_eq!(entries.len(), 3);
            assert!(entries.contains(&Path::new("/etc")));
            assert!(entries.contains(&Path::new("/home")));
            assert!(entries.contains(&Path::new("/readme.md")));

            Ok(())
        }

        #[test]
        fn test_ls_home_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.ls("/home")?.collect();

            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/guest")));

            Ok(())
        }

        #[test]
        fn test_ls_user_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.ls("/home/user")?.collect();

            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/file2.txt")));

            Ok(())
        }

        #[test]
        fn test_ls_nonexistent_path() {
            let vfs = setup_test_vfs();
            let result: Result<Vec<_>> = vfs.ls("/nonexistent").map(|iter| iter.collect());
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should mention path does not exist"
            );
        }

        #[test]
        fn test_ls_file_path() {
            let vfs = setup_test_vfs();
            let result: Result<Vec<_>> = vfs.ls("/home/user/file1.txt").map(|iter| iter.collect());
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), vec!["/home/user/file1.txt"]);
        }

        #[test]
        fn test_ls_empty_directory() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.mkdir("/empty_dir").unwrap(); // Create empty dir

            let entries: Vec<_> = vfs.ls("/empty_dir")?.collect();
            assert_eq!(entries.len(), 0); // Should return empty iterator

            Ok(())
        }

        #[test]
        fn test_ls_relative_path_from_root() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.ls("home")?.collect(); // Relative path

            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/guest")));

            Ok(())
        }

        #[test]
        fn test_ls_relative_path_nested() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home").unwrap();

            let entries: Vec<_> = vfs.ls("user")?.collect();

            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/file2.txt")));

            Ok(())
        }

        #[test]
        fn test_ls_with_trailing_slash() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries1: Vec<_> = vfs.ls("/home/")?.collect(); // With slash
            let entries2: Vec<_> = vfs.ls("/home")?.collect(); // Without slash

            assert_eq!(entries1, entries2); // Results should be identical
            Ok(())
        }

        #[test]
        fn test_ls_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();

            let entries: Vec<_> = vfs.ls(".")?.collect();
            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/file2.txt")));

            Ok(())
        }

        #[test]
        fn test_ls_double_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();

            let entries: Vec<_> = vfs.ls("..")?.collect(); // Parent directory
            assert_eq!(entries.len(), 2);
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/guest")));

            Ok(())
        }

        #[test]
        fn test_ls_iterator_lazy_evaluation() -> Result<()> {
            let vfs = setup_test_vfs();
            let mut iter = vfs.ls("/home/user")?;

            // Test that iterator doesn't panic on immediate creation
            assert!(iter.next().is_some());

            // Consume all items
            let count = iter.count();
            assert_eq!(count + 1, 2); // +1 because we already took one with next()

            Ok(())
        }
    }

    mod tree {
        use super::*;

        /// Helper to create a pre‑populated MapFS instance for testing
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new();

            // Create a nested hierarchy
            vfs.mkdir("/etc").unwrap();
            vfs.mkdir("/home").unwrap();
            vfs.mkdir("/home/user").unwrap();
            vfs.mkdir("/home/user/projects").unwrap();
            vfs.mkdir("/home/guest").unwrap();
            vfs.mkfile("/home/user/file1.txt", Some(b"Content 1"))
                .unwrap();
            vfs.mkfile("/home/user/projects/proj1.rs", Some(b"Code 1"))
                .unwrap();
            vfs.mkfile("/home/user/projects/proj2.rs", Some(b"Code 2"))
                .unwrap();
            vfs.mkfile("/home/guest/note.txt", Some(b"Note")).unwrap();
            vfs.mkfile("/readme.md", Some(b"Docs")).unwrap();

            vfs
        }

        #[test]
        fn test_tree_root() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.tree("/")?.collect();

            assert_eq!(entries.len(), 10);
            assert!(entries.contains(&Path::new("/etc")));
            assert!(entries.contains(&Path::new("/home")));
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));
            assert!(entries.contains(&Path::new("/home/guest")));
            assert!(entries.contains(&Path::new("/home/guest/note.txt")));

            Ok(())
        }

        #[test]
        fn test_tree_home_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.tree("/home")?.collect();

            assert_eq!(entries.len(), 7);
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));
            assert!(entries.contains(&Path::new("/home/guest")));
            assert!(entries.contains(&Path::new("/home/guest/note.txt")));

            Ok(())
        }

        #[test]
        fn test_tree_user_directory() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.tree("/home/user")?.collect();

            assert_eq!(entries.len(), 4);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));

            Ok(())
        }

        #[test]
        fn test_tree_nonexistent_path() {
            let vfs = setup_test_vfs();
            let result: Result<Vec<_>> = vfs.tree("/nonexistent").map(|iter| iter.collect());
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("does not exist"),
                "Error should mention path does not exist"
            );
        }

        #[test]
        fn test_tree_file_path() {
            let vfs = setup_test_vfs();
            let result: Result<Vec<_>> =
                vfs.tree("/home/user/file1.txt").map(|iter| iter.collect());
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), vec!["/home/user/file1.txt"]);
        }

        #[test]
        fn test_tree_empty_directory() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.mkdir("/empty_dir").unwrap();

            let entries: Vec<_> = vfs.tree("/empty_dir")?.collect();
            assert_eq!(entries.len(), 0); // Empty directory → empty iterator

            Ok(())
        }

        #[test]
        fn test_tree_relative_path_from_root() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries: Vec<_> = vfs.tree("home")?.collect(); // Relative path

            assert_eq!(entries.len(), 7);
            assert!(entries.contains(&Path::new("/home/user")));
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));
            assert!(entries.contains(&Path::new("/home/guest")));
            assert!(entries.contains(&Path::new("/home/guest/note.txt")));

            Ok(())
        }

        #[test]
        fn test_tree_relative_path_nested() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home").unwrap();

            let entries: Vec<_> = vfs.tree("user")?.collect();

            assert_eq!(entries.len(), 4);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));

            Ok(())
        }

        #[test]
        fn test_tree_with_trailing_slash() -> Result<()> {
            let vfs = setup_test_vfs();
            let entries1: Vec<_> = vfs.tree("/home/")?.collect(); // With slash
            let entries2: Vec<_> = vfs.tree("/home")?.collect(); // Without slash

            assert_eq!(entries1, entries2); // Results should be identical
            Ok(())
        }

        #[test]
        fn test_tree_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user").unwrap();

            let entries: Vec<_> = vfs.tree(".")?.collect();
            assert_eq!(entries.len(), 4);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));

            Ok(())
        }

        #[test]
        fn test_tree_double_dot_path() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.cd("/home/user/projects").unwrap();

            let entries: Vec<_> = vfs.tree("..")?.collect(); // Parent directory
            assert_eq!(entries.len(), 4);
            assert!(entries.contains(&Path::new("/home/user/file1.txt")));
            assert!(entries.contains(&Path::new("/home/user/projects")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj1.rs")));
            assert!(entries.contains(&Path::new("/home/user/projects/proj2.rs")));

            Ok(())
        }

        #[test]
        fn test_tree_single_entry() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.mkdir("/single").unwrap();

            let entries: Vec<_> = vfs.tree("/single")?.collect();
            assert_eq!(entries.len(), 0); // No children → empty

            Ok(())
        }

        #[test]
        fn test_tree_iterator_lazy_evaluation() -> Result<()> {
            let vfs = setup_test_vfs();
            let mut iter = vfs.tree("/home/user")?;

            // Test that iterator doesn't panic on immediate creation
            assert!(iter.next().is_some());

            // Consume remaining items
            let count = iter.count();
            assert_eq!(count + 1, 4); // +1 because we already took one with next()

            Ok(())
        }

        #[test]
        fn test_tree_case_sensitivity() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.mkdir("/CASE_TEST").unwrap();
            vfs.mkfile("/CASE_TEST/file.txt", Some(b"Data")).unwrap();

            let entries: Vec<_> = vfs.tree("/CASE_TEST")?.collect();

            assert_eq!(entries.len(), 1);
            assert!(entries.contains(&Path::new("/CASE_TEST/file.txt")));

            Ok(())
        }
    }

    mod mkdir_mkfile {
        use super::*;

        /// Helper to create a fresh MapFS instance
        fn setup_vfs() -> MapFS {
            MapFS::new()
        }

        #[test]
        fn test_mkdir_simple_directory() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkdir("/test")?;

            assert!(vfs.exists("/test"));
            assert!(vfs.is_dir("/test")?);

            Ok(())
        }

        #[test]
        fn test_mkdir_nested_directories() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkdir("/a/b/c/d")?;

            assert!(vfs.exists("/a"));
            assert!(vfs.exists("/a/b"));
            assert!(vfs.exists("/a/b/c"));
            assert!(vfs.exists("/a/b/c/d"));

            Ok(())
        }

        #[test]
        fn test_mkdir_existing_path() {
            let mut vfs = setup_vfs();
            vfs.mkdir("/existing").unwrap();

            let result = vfs.mkdir("/existing");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("path already exists"),
                "Should error when path exists"
            );
        }

        #[test]
        fn test_mkdir_empty_path() {
            let mut vfs = setup_vfs();
            let result = vfs.mkdir("");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("invalid path: empty"),
                "Empty path should be rejected"
            );
        }

        #[test]
        fn test_mkdir_root_path() {
            let mut vfs = setup_vfs();
            let result = vfs.mkdir("/");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("path already exists"),
                "Root always exists, should error"
            );
        }

        #[test]
        fn test_mkdir_with_trailing_slash() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkdir("/test/")?; // Trailing slash

            assert!(vfs.exists("/test"));
            assert!(vfs.is_dir("/test")?);

            Ok(())
        }

        #[test]
        fn test_mkfile_simple_file() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkfile("/file.txt", Some(b"Hello World"))?;

            assert!(vfs.exists("/file.txt"));
            assert!(vfs.is_file("/file.txt")?);
            assert_eq!(vfs.read("/file.txt")?, b"Hello World");

            Ok(())
        }

        #[test]
        fn test_mkfile_in_nested_directory() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkfile("/a/b/c/file.txt", Some(b"Content"))?;

            // All parent directories should be created
            assert!(vfs.exists("/a"));
            assert!(vfs.exists("/a/b"));
            assert!(vfs.exists("/a/b/c"));
            assert!(vfs.exists("/a/b/c/file.txt"));

            assert_eq!(vfs.read("/a/b/c/file.txt")?, b"Content");

            Ok(())
        }

        #[test]
        fn test_mkfile_empty_content() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkfile("/empty.txt", None)?; // No content

            assert!(vfs.exists("/empty.txt"));
            assert!(vfs.is_file("/empty.txt")?);
            assert_eq!(vfs.read("/empty.txt")?, &[]);

            Ok(())
        }

        #[test]
        fn test_mkfile_existing_file() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkfile("/test.txt", Some(b"Original"))?;

            // Try to create same file again
            let result = vfs.mkfile("/test.txt", Some(b"New"));

            assert!(result.is_err());
            assert_eq!(vfs.read("/test.txt")?, b"Original");

            Ok(())
        }

        #[test]
        fn test_mkfile_to_existing_directory() {
            let mut vfs = setup_vfs();
            vfs.mkdir("/dir").unwrap();

            let result = vfs.mkfile("/dir", Some(b"Content"));
            assert!(result.is_err());
            // Depending on design, this might be allowed or not
            // Current implementation tries to create file at existing dir path
            // Consider whether this should be an error
        }

        #[test]
        fn test_mkfile_with_trailing_slash() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkfile("/file.txt/", Some(b"With slash"))?;

            assert!(vfs.exists("/file.txt")); // Should normalize
            assert_eq!(vfs.read("/file.txt")?, b"With slash");

            Ok(())
        }

        #[test]
        fn test_mkfile_relative_path() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkdir("/home")?;
            vfs.cd("/home")?; // Assume /home exists

            vfs.mkfile("file.txt", Some(b"Relative"))?;

            assert!(vfs.exists("/home/file.txt"));
            assert_eq!(vfs.read("/home/file.txt")?, b"Relative");

            Ok(())
        }

        #[test]
        fn test_mkdir_and_mkfile_combination() -> Result<()> {
            let mut vfs = setup_vfs();

            vfs.mkdir("/projects")?;
            vfs.mkfile("/projects/main.rs", Some(b"fn main() {}"))?;
            vfs.mkdir("/projects/tests")?;
            vfs.mkfile("/projects/tests/test1.rs", Some(b"#[test]"))?;

            assert!(vfs.exists("/projects"));
            assert!(vfs.exists("/projects/main.rs"));
            assert!(vfs.exists("/projects/tests"));
            assert!(vfs.exists("/projects/tests/test1.rs"));

            Ok(())
        }

        #[test]
        fn test_mkdir_case_sensitivity() -> Result<()> {
            let mut vfs = setup_vfs();
            vfs.mkdir("/CaseDir")?;

            assert!(vfs.exists("/CaseDir"));
            assert!(!vfs.exists("/casedir")); // Case-sensitive

            Ok(())
        }
    }

    mod read_write_append {
        use super::*;

        /// Helper to create a pre‑populated MapFS instance for testing
        fn setup_test_vfs() -> MapFS {
            let mut vfs = MapFS::new();

            // Create sample files and directories
            vfs.mkdir("/etc").unwrap();
            vfs.mkfile("/readme.md", Some(b"Project docs")).unwrap();
            vfs.mkfile("/data.bin", Some(b"\x00\x01\x02")).unwrap();
            vfs.mkfile("/empty.txt", None).unwrap(); // Empty file
            vfs.mkfile("/home/user/file.txt", Some(b"Hello World"))
                .unwrap();

            vfs
        }

        #[test]
        fn test_read_existing_file() -> Result<()> {
            let vfs = setup_test_vfs();
            let content = vfs.read("/readme.md")?;
            assert_eq!(content, b"Project docs");
            Ok(())
        }

        #[test]
        fn test_read_binary_file() -> Result<()> {
            let vfs = setup_test_vfs();
            let content = vfs.read("/data.bin")?;
            assert_eq!(content, vec![0x00, 0x01, 0x02]);
            Ok(())
        }

        #[test]
        fn test_read_empty_file() -> Result<()> {
            let vfs = setup_test_vfs();
            let content = vfs.read("/empty.txt")?;
            assert!(content.is_empty());
            Ok(())
        }

        #[test]
        fn test_read_nonexistent_file() {
            let vfs = setup_test_vfs();
            let result = vfs.read("/nonexistent.txt");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("does not exist"),
                "Error should mention file does not exist"
            );
        }

        #[test]
        fn test_read_directory_as_file() {
            let vfs = setup_test_vfs();
            let result = vfs.read("/etc");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("is a directory"),
                "Reading directory as file should error"
            );
        }

        #[test]
        fn test_write_existing_file() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.write("/readme.md", b"Updated content")?;

            let content = vfs.read("/readme.md")?;
            assert_eq!(content, b"Updated content");
            Ok(())
        }

        #[test]
        fn test_write_binary_content() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.write("/data.bin", &[0xFF, 0xFE, 0xFD])?;

            let content = vfs.read("/data.bin")?;
            assert_eq!(content, vec![0xFF, 0xFE, 0xFD]);
            Ok(())
        }

        #[test]
        fn test_write_empty_content() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.write("/empty.txt", &[])?;

            let content = vfs.read("/empty.txt")?;
            assert!(content.is_empty());
            Ok(())
        }

        #[test]
        fn test_write_nonexistent_file() {
            let mut vfs = setup_test_vfs();
            let result = vfs.write("/newfile.txt", b"Content");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("does not exist"),
                "Writing to nonexistent file should fail"
            );
        }

        #[test]
        fn test_write_directory_as_file() {
            let mut vfs = setup_test_vfs();
            let result = vfs.write("/etc", b"Content");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("is a directory"),
                "Writing to directory should error"
            );
        }

        #[test]
        fn test_append_to_file() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.append("/readme.md", b" - appended")?;

            let content = vfs.read("/readme.md")?;
            assert_eq!(content, b"Project docs - appended");
            Ok(())
        }

        #[test]
        fn test_append_binary_data() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.append("/data.bin", &[0xAA, 0xBB])?;

            let content = vfs.read("/data.bin")?;
            assert_eq!(content, vec![0x00, 0x01, 0x02, 0xAA, 0xBB]);
            Ok(())
        }

        #[test]
        fn test_append_empty_slice() -> Result<()> {
            let mut vfs = setup_test_vfs();
            vfs.append("/empty.txt", &[])?; // Append nothing

            let content = vfs.read("/empty.txt")?;
            assert!(content.is_empty()); // Still empty
            Ok(())
        }

        #[test]
        fn test_append_nonexistent_file() {
            let mut vfs = setup_test_vfs();
            let result = vfs.append("/newfile.txt", b"More content");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("does not exist"),
                "Appending to nonexistent file should fail"
            );
        }

        #[test]
        fn test_append_directory_as_file() {
            let mut vfs = setup_test_vfs();
            let result = vfs.append("/etc", b"Data");
            assert!(result.is_err());
            assert!(
                result.unwrap_err().to_string().contains("is a directory"),
                "Appending to directory should error"
            );
        }

        #[test]
        fn test_write_and_append_sequence() -> Result<()> {
            let mut vfs = setup_test_vfs();

            // Start with initial content
            vfs.mkfile("/test.txt", None)?;
            vfs.write("/test.txt", b"Initial")?;

            // Append some data
            vfs.append("/test.txt", b" + appended")?;

            // Overwrite completely
            vfs.write("/test.txt", b"Overwritten")?;

            let final_content = vfs.read("/test.txt")?;
            assert_eq!(final_content, b"Overwritten");

            Ok(())
        }

        #[test]
        fn test_read_after_write_and_append() -> Result<()> {
            let mut vfs = setup_test_vfs();

            vfs.mkfile("/log.txt", None)?;
            vfs.write("/log.txt", b"Entry 1\n")?;
            vfs.append("/log.txt", b"Entry 2\n")?;
            vfs.write("/log.txt", b"Overwritten log\n")?;
            vfs.append("/log.txt", b"Final entry\n")?;

            let content = vfs.read("/log.txt")?;
            assert_eq!(content, b"Overwritten log\nFinal entry\n");

            Ok(())
        }
    }

    mod rm {
        use super::*;
    }

    mod cleanup {
        use super::*;
    }
}
