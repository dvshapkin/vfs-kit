use std::path::Path;

pub trait FsBackend {
    fn root(&mut self, path: &str) -> Result<()>;
    fn current_path(&self) -> &Path;
    fn cd(&mut self, path: &str) -> Result<()>;
    fn mkdir(&mut self, name: &str) -> Result<()>;
    fn mkfile(&mut self, name: &str, content: &[u8]) -> Result<()>;
    fn rm(&mut self, path: &str) -> Result<()>;
    fn clean(&mut self) -> Result<()>;
}

type Result<T> = std::result::Result<T, anyhow::Error>;