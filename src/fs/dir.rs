use std::path::{Path, PathBuf};

use anyhow::anyhow;

use crate::core::{FsBackend, Result};

pub struct DirFs {
    root: PathBuf,                      // host-related path
    cwd: PathBuf,                       // inner absolute path form
    entries: Vec<PathBuf>,              // inner absolute path form
    created_root_parents: Vec<PathBuf>, // host-related paths
    is_auto_clean: bool,
}

impl DirFs {
    /// `path` is a host-related path.
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self> {
        let root = root.as_ref();
        if root.exists() && !root.is_dir() {
            return Err(anyhow!("root must be a directory"));
        }
        let mut fs = Self {
            root: PathBuf::new(),
            cwd: PathBuf::from("/"),
            entries: Vec::new(),
            created_root_parents: Vec::new(),
            is_auto_clean: true,
        };
        if !root.exists() {
            let created = fs.mkdir_all(root)?;
            if !created.is_empty() {
                fs.created_root_parents.extend(created);
            }
        }
        fs.entries.push(PathBuf::from("/"));
        fs.root = root.canonicalize()?;
        Ok(fs)
    }

    pub fn set_auto_clean(&mut self, clean: bool) {
        self.is_auto_clean = clean;
    }

    /// Returns full absolute inner path.
    fn as_inner_path<P: AsRef<Path>>(&self, path: P) -> Option<PathBuf> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Some(self.cwd.clone());
        }
        if path.as_os_str() == "." {
            return Some(self.cwd.clone());
        }
        if path.as_os_str() == ".." {
            if let Some(parent) = self.cwd.parent() {
                return Some(parent.to_path_buf());
            }
            return None;
        }
        let mut path = path.to_path_buf();
        if path.is_relative() {
            path = self.cwd.join(path);
        }
        Some(Self::normalize(path))
    }

    fn normalize<P: AsRef<Path>>(path: P) -> PathBuf {
        let path = path.as_ref();
        let mut result = PathBuf::new();

        for component in path.components() {
            match component {
                // Пропускаем текущую директорию (.)
                std::path::Component::CurDir => {}
                // Обрабатываем родительскую директорию (..)
                std::path::Component::ParentDir => {
                    // Если в результате уже есть компоненты, удаляем последний (поднимаемся на уровень выше)
                    if let Some(comp) = result.parent() {
                        result = comp.to_path_buf();
                    }
                }
                // Остальные компоненты (нормальные имена директорий/файлов) добавляем в результат
                _ => result.push(component),
            }
        }
        result
    }

    fn host_path<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        if path.as_ref().is_absolute() {
            return Err(anyhow!("path must be relative"));
        }
        Ok(self.root.join(path))
    }

    /// Make directories recursively.
    /// `path` is a host-related path.
    /// Returns vector of created directories.
    fn mkdir_all<P: AsRef<Path>>(&mut self, path: P) -> Result<Vec<PathBuf>> {
        let path = Self::normalize(path);
        let mut created = Vec::new();
        let mut prefix = PathBuf::new();
        for part in path.components() {
            prefix.push(part);
            if prefix.exists() {
                // Если существует — убеждаемся, что это директория
                if !prefix.is_dir() {
                    return Err(anyhow!(
                        "path '{}' exists but is not a directory",
                        prefix.display()
                    ));
                }
                continue; // Уже существует и это директория — идём дальше
            }
            std::fs::create_dir(prefix.as_path())?;
            created.push(prefix.to_path_buf());
        }
        Ok(created)
    }
}

impl FsBackend for DirFs {
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
    /// Error returns in case the `path` not exist.
    fn cd<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if let Some(path) = self.as_inner_path(path.as_ref()) {
            if !self.exists(&path) {
                return Err(anyhow!("the path don't exist: '{}'", path.display()));
            }
            self.cwd = path;
        }
        Ok(())
    }

    /// Creates directory and all it parents, if necessary.
    fn mkdir<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        if let Some(path) = self.as_inner_path(path.as_ref()) {
            if self.exists(&path) {
                return Err(anyhow!("directory `{}` already exists", path.display()));
            }
            let host_path = self.host_path(&path)?;
            let created = self.mkdir_all(&host_path)?;
            if !created.is_empty() {
                self.entries.extend(created);
            }
        }
        Ok(())
    }

    fn mkfile(&mut self, name: &str, content: &[u8]) -> Result<()> {
        todo!()
    }

    /// Returns true, if `path` exists.
    fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        self.entries.contains(&path.as_ref().to_path_buf()) // TODO: very slow!!!
    }

    fn rm(&mut self, path: &str) -> Result<()> {
        todo!()
    }

    fn clean(&mut self) -> Result<()> {
        for entry in self.entries.iter().rev() {
            let host_path = self.host_path(entry)?;
            if host_path.is_dir() {
                std::fs::remove_dir(host_path)?;
            } else {
                std::fs::remove_file(host_path)?;
            }
        }
        self.entries.clear();
        for parent in self.created_root_parents.iter().rev() {
            std::fs::remove_dir(parent)?;
        }
        self.created_root_parents.clear();
        Ok(())
    }
}

impl Drop for DirFs {
    fn drop(&mut self) {
        if self.is_auto_clean {
            if let Err(e) = self.clean() {
                eprintln!("Failed to clean DirFs: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_inner_path() {
        let mut fs = DirFs::new("/").unwrap();
        fs.cwd = PathBuf::from("/current/working/dir");

        let path = Path::new("/foo/bar");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/foo/bar"));

        let path = Path::new("");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working/dir"));

        let path = Path::new(".");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working/dir"));

        let path = Path::new("..");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working"));

        let path = Path::new("../..");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current"));

        let path = Path::new("../../..");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/"));

        let path = Path::new("../../../../../..");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/"));

        let path = Path::new("../foo");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working/foo"));

        let path = Path::new("./foo");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working/dir/foo"));

        let path = Path::new("/foo/././bar");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/foo/bar"));

        let path = Path::new("/foo/./../bar");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/bar"));

        let path = Path::new("foo/./../bar");
        let normalized = fs.as_inner_path(path).unwrap();
        assert_eq!(normalized.as_path(), Path::new("/current/working/dir/bar"));
    }

    #[test]
    fn test_new() {}
}
