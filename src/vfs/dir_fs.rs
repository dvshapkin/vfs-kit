/// This module provides a virtual filesystem (VFS) implementation that maps to a real directory
/// on the host system. It allows file and directory operations (create, read, remove, navigate)
/// within a controlled root path while maintaining internal state consistency.
///
/// Key Features:
/// - **Isolated root**: All operations are confined to a designated root directory (self.root).
/// - **Path normalization**: Automatically resolves . and .. components and removes trailing slashes.
/// - **State tracking**: Maintains an internal set of valid paths (self.entries) to reflect VFS
///   structure.
/// - **Auto‑cleanup**: Optionally removes created artifacts on Drop (when is_auto_clean = true).
/// - **Cross‑platform**: Uses std::path::Path and PathBuf for portable path handling.


use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::anyhow;

use crate::core::{FsBackend, Result};

pub struct DirFS {
    root: PathBuf,                      // host-related absolute normalized path
    cwd: PathBuf,                       // inner absolute normalized path
    entries: HashSet<PathBuf>,          // inner absolute normalized paths
    created_root_parents: Vec<PathBuf>, // host-related absolute normalized paths
    is_auto_clean: bool,
}

/// A virtual filesystem (VFS) implementation that maps to a real directory on the host system.
///
/// `DirFS` provides an isolated, path‑normalized view of a portion of the filesystem, rooted at a
/// designated absolute path (`root`). It maintains an internal state of valid paths and supports
/// standard operations:
/// - Navigate via `cd()` (change working directory).
/// - Create directories (`mkdir()`) and files (`mkfile()`).
/// - Remove entries (`rm()`).
/// - Check existence (`exists()`).
/// - Read and write content (`read()` / `write()`).
///
/// Key features:
/// - **Path normalization**: Automatically resolves `.`, `..`, and trailing slashes.
/// - **State consistency**: Tracks all valid VFS paths to ensure operations reflect
///   the actual VFS structure.
/// - **Isolated root**: All operations are confined to the `root` directory;
///   no access to parent paths.
/// - **Auto‑cleanup**: Optionally removes created parent directories on drop
///   (when `is_auto_clean = true`).
/// - **Cross‑platform**: Uses `std::path::Path` for portable path handling.
///
/// Usage notes:
/// - `DirFS` does not follow symlinks; `rm()` removes the link, not the target.
/// - Permissions are not automatically adjusted; ensure `root` is writable.
/// - Not thread‑safe in current version (wrap in `Mutex` if needed).
/// - Errors are returned via `anyhow::Result` with descriptive messages.
///
/// Example:
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
impl DirFS {
    /// Creates a new DirFs instance with the root directory at `path`.
    /// Checks permissions to create and write into `path`.
    /// * `path` is an absolute host path.
    /// If `path` is not absolute, error returns.
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

        let root = Self::normalize(root);

        let mut created_root_parents = Vec::new();
        if !std::fs::exists(&root)? {
            created_root_parents.extend(Self::mkdir_all(&root)?);
        }

        // check permissions
        if !Self::check_permissions(&root) {
            return Err(anyhow!("Access denied: {:?}", root));
        }

        Ok(Self {
            root,
            cwd: PathBuf::from("/"),
            entries: HashSet::from([PathBuf::from("/")]),
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

    /// Normalizes an arbitrary `path` by processing all occurrences
    /// of '.' and '..' elements. Also, removes final `/`.
    fn normalize<P: AsRef<Path>>(path: P) -> PathBuf {
        let mut result = PathBuf::new();
        for component in path.as_ref().components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    result.pop();
                }
                _ => {
                    result.push(component);
                }
            }
        }
        // remove final /
        if result != PathBuf::from("/") && result.ends_with("/") {
            result.pop();
        }
        result
    }

    fn to_host<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let inner = self.to_inner(path);
        self.root.join(inner.strip_prefix("/").unwrap())
    }

    fn to_inner<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        Self::normalize(self.cwd.join(path))
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

    fn rm_host_artifact<P: AsRef<Path>>(host_path: P) -> Result<()> {
        let host_path = host_path.as_ref();
        if host_path.is_dir() {
            std::fs::remove_dir_all(host_path)?
        } else {
            std::fs::remove_file(host_path)?
        }
        Ok(())
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

    /// Changes the current working directory.
    /// * `path` can be in relative or absolute form, but in both cases it must exist.
    /// An error is returned if the specified `path` does not exist.
    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let target = self.to_inner(path);
        if !self.exists(&target) {
            return Err(anyhow!("{} does not exist", target.display()));
        }
        self.cwd = target;
        Ok(())
    }

    /// Checks if a `path` exists in the vfs.
    /// The `path` can be:
    /// - absolute (starting with '/'),
    /// - relative (relative to the vfs cwd),
    /// - contain '..' or '.'.
    fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        let inner_path = self.to_inner(path);
        self.entries.contains(&inner_path)
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
            if self.entries.contains(parent) {
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
            if !self.entries.contains(&built) {
                let host = self.to_host(&built);
                std::fs::create_dir(&host)?;
                self.entries.insert(built.clone());
            }
        }

        Ok(())
    }

    /// Creates new file in vfs.
    /// * `file_path` must be inner vfs path. It must contain the name of the file,
    /// optionally preceded by existing parent directory.
    /// If the parent directory does not exist, an error is returned.
    fn mkfile<P: AsRef<Path>>(&mut self, file_path: P, content: Option<&[u8]>) -> Result<()> {
        let file_path = self.to_inner(file_path);
        if let Some(parent) = file_path.parent() {
            if let Err(e) = std::fs::exists(parent) {
                return Err(anyhow!("{:?}: {}", parent, e));
            }
        }
        let host = self.to_host(&file_path);
        let mut fd = std::fs::File::create(host)?;
        self.entries.insert(file_path);
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
        if !self.exists(&inner) {
            return Err(anyhow!("file does not exist: {}", path.as_ref().display()));
        }
        let host = self.to_host(&inner);
        if host.is_dir() {
            return Err(anyhow!("{} is a directory", host.display()));
        }

        let mut content = Vec::new();
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
    fn write<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()> {
        let inner = self.to_inner(&path);
        let host = self.to_host(&inner);

        if !self.exists(&inner) {
            return Err(anyhow!("file does not exist: {}", path.as_ref().display()));
        }
        if host.is_dir() {
            return Err(anyhow!("{} is a directory", host.display()));
        }

        std::fs::write(&host, content)?;

        Ok(())
    }

    /// Removes a file or directory at the specified path.
    ///
    /// - `path`: can be absolute (starting with '/') or relative to the current working
    /// directory (cwd).
    /// - If the path is a directory, all its contents are removed recursively.
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
        if path.as_ref().as_os_str() == "/" {
            return Err(anyhow!("invalid path: the root cannot be removed"));
        }

        let inner_path = self.to_inner(path); // Convert to VFS-internal normalized path
        let host_path = self.to_host(&inner_path); // Map to real filesystem path

        // Check if the path exists in the virtual filesystem
        if !self.exists(&inner_path) {
            return Err(anyhow!("{} does not exist", inner_path.display()));
        }

        // Remove from the real filesystem
        Self::rm_host_artifact(host_path)?;

        // Update internal state: collect all entries that start with `inner_path`
        let removed: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|p| p.starts_with(&inner_path)) // Match prefix (includes subpaths)
            .cloned()
            .collect();

        // Remove all matched entries from the set
        for p in removed {
            self.entries.remove(&p);
        }

        Ok(())
    }

    /// Removes all artifacts (dirs and files) in vfs, but preserve its root.
    fn cleanup(&mut self) -> bool {
        let mut is_ok = true;

        // Collect all paths to delete (except the root "/")
        let mut sorted_paths_to_remove: BTreeSet<PathBuf> = BTreeSet::new();
        for entry in &self.entries {
            if entry != &PathBuf::from("/") {
                sorted_paths_to_remove.insert(entry.clone());
            }
        }

        for entry in sorted_paths_to_remove.iter().rev() {
            let host = self.to_host(entry);
            let result = Self::rm_host_artifact(&host);
            if result.is_ok() {
                self.entries.remove(entry);
            } else {
                is_ok = false;
                eprintln!("Unable to remove: {}", host.display());
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
            .filter_map(|p| Self::rm_host_artifact(p).err())
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
            assert!(fs.entries.contains(&PathBuf::from("/")));
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
            let canonical = DirFS::normalize(temp_dir.path().join("subdir"));

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
            assert_eq!(DirFS::normalize("/a/b/c/"), PathBuf::from("/a/b/c"));
            assert_eq!(DirFS::normalize("/a/b/./c"), PathBuf::from("/a/b/c"));
            assert_eq!(DirFS::normalize("/a/b/../c"), PathBuf::from("/a/c"));
            assert_eq!(DirFS::normalize("/"), PathBuf::from("/"));
            assert_eq!(DirFS::normalize("/.."), PathBuf::from("/"));
            assert_eq!(DirFS::normalize(".."), PathBuf::from(""));
            assert_eq!(DirFS::normalize(""), PathBuf::from(""));
            assert_eq!(DirFS::normalize("../a"), PathBuf::from("a"));
            assert_eq!(DirFS::normalize("./a"), PathBuf::from("a"));
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
            assert_eq!(fs.entries.contains(&PathBuf::from("/file.txt")), true);
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
            assert!(result.is_err());
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
        fn test_mkfile_permission_denied() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let temp_dir = setup_test_env();
                let root = temp_dir.path();
                let protected = root.join("protected");
                std::fs::create_dir(&protected).unwrap();
                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o000)).unwrap(); // No access

                let mut fs = DirFS::new(root).unwrap();
                let result = fs.mkfile("/protected/file.txt", None);

                std::fs::set_permissions(&protected, PermissionsExt::from_mode(0o755)).unwrap(); // Grant access

                assert!(result.is_err());
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Permission denied")
                );
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
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("file does not exist: /not/found.txt")
            );

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
            let fs = DirFS::new(temp_dir.path())?;

            let result = fs.write("/parent/child.txt", b"Content");
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("file does not exist")
            );

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
            assert!(fs.entries.contains(&PathBuf::from("/")));
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
            assert!(fs.entries.contains(&PathBuf::from("/")));
            assert!(root.exists()); // The root is not removed
        }
    }

    // Helper function: Creates a temporary directory for tests
    fn setup_test_env() -> TempDir {
        TempDir::new("dirfs_test").unwrap()
    }
}
