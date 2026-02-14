#[derive(Debug, Copy, Clone, PartialEq)]
pub enum EntryType {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    entry_type: EntryType,
    content: Option<Vec<u8>>,
}

impl Entry {
    pub fn new(entry_type: EntryType) -> Entry {
        Entry {
            entry_type,
            content: None,
        }
    }

    pub fn entry_type(&self) -> EntryType {
        self.entry_type
    }

    pub fn is_file(&self) -> bool {
        self.entry_type == EntryType::File
    }

    pub fn is_dir(&self) -> bool {
        self.entry_type == EntryType::Directory
    }

    pub fn content(&self) -> Option<&Vec<u8>> {
        self.content.as_ref()
    }

    pub fn set_content(&mut self, content: &[u8]) {
        self.content = Some(Vec::from(content));
    }

    pub fn append_content(&mut self, content: &[u8]) {
        let mut new_content = if self.content.is_some() {
             self.content.take().unwrap()
        } else {
            Vec::new()
        };
        new_content.extend_from_slice(content);
        self.set_content(&new_content);
    }
}
