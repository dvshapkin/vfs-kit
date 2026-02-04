use std::path::Path;

/// FsBackend defines a common API for all virtual file systems (vfs) in the crate.
/// Some functions here use `path` as a parameter or return value.
/// In all cases, `path` will refer to the virtual file system. The exception
/// is the `root()` function, which returns the path in the host file system.
pub trait FsBackend {
    /// Returns root path refer to the host file system.
    fn root(&self) -> &Path;

    /// Returns current working directory related to the vfs root.
    fn cwd(&self) -> &Path;

    /// Changes the current working directory.
    /// `path` can be in relative or absolute form, but in both cases it must exist in vfs.
    /// Error returns in case the `path` is not exist.
    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Returns true, if `path` exists.
    fn exists<P: AsRef<Path>>(&self, path: P) -> bool;

    /// Creates directory and all it parents, if necessary.
    fn mkdir<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Creates new file in vfs.
    fn mkfile<P: AsRef<Path>>(&mut self, name: P, content: Option<&[u8]>) -> Result<()>;
    
    /// Reads the entire contents of a file into a byte vector.
    fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>>;

    /// Writes bytes to an existing file, replacing its entire contents.
    fn write<P: AsRef<Path>>(&self, path: P, content: &[u8]) -> Result<()>;

    /// Removes a file or directory at the specified path.
    fn rm<P: AsRef<Path>>(&mut self, path: P) -> Result<()>;

    /// Removes all artifacts (dirs and files) in vfs, but preserve its root.
    fn cleanup(&mut self) -> bool;
}

pub type Result<T> = std::result::Result<T, anyhow::Error>;
