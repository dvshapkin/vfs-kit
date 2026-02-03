use std::collections::{BTreeSet, HashSet};
use std::io::Write;
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

impl DirFS {
    /// Creates a new DirFs instance with the root directory at `path`.
    /// Checks permissions to create and write into `path`.
    /// `path` is an absolute host path.
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
    /// `path` is an absolute host path.
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

    fn rm_host_artifact<P: AsRef<Path>>(&self, host_path: P) -> Result<()> {
        let host_path = host_path.as_ref();
        if host_path.is_dir() {
            std::fs::remove_dir(host_path)?
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
    /// `path` can be in relative or absolute form, but in both cases it must exist.
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
    /// `path` - inner vfs path.
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
    /// `file_path` must be inner vfs path. It must contain the name of the file,
    /// optionally preceded by existing parent directory.
    /// If the parent directory does not exist, an error is returned.
    fn mkfile<P: AsRef<Path>>(&mut self, file_path: P, content: Option<&[u8]>) -> Result<()> {
        let file_path = Self::normalize(self.cwd.join(file_path));
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

    /// Removes vfs artifact (file or directory).
    /// `path` must be existed.
    fn rm(&mut self, path: &str) -> Result<()> {
        todo!()
    }

    /// Removes all artifacts (dirs and files) in vfs,
    /// but preserve its root.
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
            let result = self.rm_host_artifact(&host);
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

        let errors: Vec<_> = self.created_root_parents
            .iter()
            .rev()
            .filter_map(|p| self.rm_host_artifact(p).err())
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
