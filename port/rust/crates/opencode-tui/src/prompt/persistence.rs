//! `util/persistence.ts`'s `readText`, `writeText` and `appendText`, off the runtime thread: writes are fire and
//! forget (TS catches and ignores their errors), each creating the parent directory first.
use std::path::Path;

/// `readText(path)`.
pub async fn read_text(path: &str) -> Option<String> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok()).await.ok().flatten()
}

fn parent(path: &str) {
    if let Some(dir) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
}

/// `writeText(path, content)`.
pub fn write_text(path: &str, content: String) {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        parent(&path);
        let _ = std::fs::write(&path, content);
    });
}

/// `appendText(path, content)`.
pub fn append_text(path: &str, content: String) {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        parent(&path);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = file.write_all(content.as_bytes());
        }
    });
}
