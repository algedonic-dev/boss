use std::fs::File;

/// Release a held lock when its owner leaves, even if a child inherited
/// the same open file description before exec (backlog 51875c3e).
#[derive(Debug)]
pub(crate) struct FileLock(File);

impl FileLock {
    pub(crate) fn held(file: File) -> Self {
        Self(file)
    }

    #[cfg(test)]
    pub(crate) fn try_clone(&self) -> std::io::Result<File> {
        self.0.try_clone()
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
