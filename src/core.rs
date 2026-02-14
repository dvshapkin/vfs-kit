use std::path::{Path, PathBuf};
use anyhow;
use crate::Entry;

/// FsBackend defines a common API for all virtual file systems (vfs) in the crate.
/// Some functions here use `path` as a parameter or return value.
/// In all cases, `path` will refer to the virtual file system. The exception
/// is the `root()` function, which returns the path in the host file system.
pub trait FsBackend {
    /// Returns root path refer to the host file system.
    fn root(&self) -> &Path;

    /// Returns current working directory related to the vfs root.
    fn cwd(&self) -> &Path;

    /// Returns the path on the host system that matches the specified internal path.
    /// * `inner_path` must exist in VFS
    fn to_host<P: AsRef<Path>>(&self, inner_path: P) -> Result<PathBuf>;

    /// Changes the current working directory.
    /// `path` can be in relative or absolute form, but in both cases it must exist in vfs.
    /// Error returns in case the `path` is not exist.
    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Returns true, if `path` exists.
    fn exists<P: AsRef<Path>>(&self, path: P) -> bool;

    /// Checks if `path` is a directory.
    fn is_dir<P: AsRef<Path>>(&self, path: P) -> Result<bool>;

    /// Checks if `path` is a regular file.
    fn is_file<P: AsRef<Path>>(&self, path: P) -> Result<bool>;
    
    /// Returns an iterator over directory entries.
    /// `path` is a directory, or CWD if None.
    fn ls<P: AsRef<Path>>(&self, path: P) -> Result<impl Iterator<Item = &Path>>;

    /// Returns a recursive iterator over the directory tree starting from a given path.
    fn tree<P: AsRef<Path>>(&self, path: P) -> Result<impl Iterator<Item = &Path>>;

    /// Creates directory and all it parents, if necessary.
    fn mkdir<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Creates new file in vfs.
    fn mkfile<P: AsRef<Path>>(&mut self, file_path: P, content: Option<&[u8]>) -> Result<()>;
    
    /// Reads the entire contents of a file into a byte vector.
    fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>>;

    /// Writes bytes to an existing file, replacing its entire contents.
    fn write<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()>;

    /// Appends bytes to the end of an existing file, preserving its old contents.
    fn append<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()>;

    /// Removes a file or directory at the specified path.
    fn rm<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Removes all artifacts (dirs and files) in vfs, but preserve its root.
    fn cleanup(&mut self) -> bool;
}

pub type Result<T> = std::result::Result<T, anyhow::Error>;

pub mod utils {
    use std::path::{Component, Path, PathBuf};
    use super::Result;
    
    /// Normalizes an arbitrary `path` by processing all occurrences
    /// of '.' and '..' elements. Also, removes final `/`.
    pub fn normalize<P: AsRef<Path>>(path: P) -> PathBuf {
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

    /// Checks that the path consists only of the root component `/`.
    pub fn is_virtual_root<P: AsRef<Path>>(path: P) -> bool {
        let components: Vec<_> = path.as_ref().components().collect();
        components.len() == 1 && components[0] == Component::RootDir
    }

    /// Removes file or directory (recursively) on host.
    pub fn rm_on_host<P: AsRef<Path>>(host_path: P) -> Result<()> {
        let host_path = host_path.as_ref();
        if host_path.is_dir() {
            std::fs::remove_dir_all(host_path)?
        } else {
            std::fs::remove_file(host_path)?
        }
        Ok(())
    }
}