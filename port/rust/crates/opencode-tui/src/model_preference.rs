//! `model-preference.ts`: the recent, favorite and per-model variant choices in `<state>/model.json`, read and
//! written under the file's lock (`Flock`, in `<state>/locks`), written atomically (`writeJsonAtomic`), and
//! reloaded for subscribers when the file changes (a watch of its directory, 50 ms debounced).
//!
//! Writes run in order, off the event loop (`spawn_blocking`), as TS chains them on `pending`.
//! TODO(port-tui): the directory watch is inotify (Linux); on macOS changes made by another process are seen at the
//! next write.
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use opencode_protocol::Json;
use opencode_protocol::wire::Object;

/// `ModelPreferenceModel`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

impl ModelRef {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> ModelRef {
        ModelRef { provider_id: provider_id.into(), model_id: model_id.into() }
    }
}

/// `ModelPreference`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPreference {
    pub recent: Vec<ModelRef>,
    pub favorite: Vec<ModelRef>,
    /// `variant`: model key to variant ("default" kept explicitly), in insertion order.
    pub variant: Vec<(String, String)>,
}

impl ModelPreference {
    pub fn variant_of(&self, key: &str) -> Option<&str> {
        self.variant.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

fn str_of<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    match value.as_object()?.get(key) {
        Some(Json::String(s)) => Some(s),
        _ => None,
    }
}

fn models(value: Option<&Json>) -> Vec<ModelRef> {
    let Some(Json::Array(items)) = value else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let provider = str_of(item, "providerID").filter(|s| !s.is_empty())?;
            let model = str_of(item, "modelID").filter(|s| !s.is_empty())?;
            Some(ModelRef::new(provider, model))
        })
        .collect()
}

fn variants(value: Option<&Json>) -> Vec<(String, String)> {
    let Some(Json::Object(object)) = value else { return Vec::new() };
    // Explicit "default" is kept: it overrides an agent's configured variant.
    object
        .iter()
        .filter_map(|(key, item)| match item {
            Json::String(s) if !key.is_empty() && !s.is_empty() => Some((key.clone(), s.clone())),
            _ => None,
        })
        .collect()
}

/// `normalizeModelVariant`.
pub fn normalize_model_variant(value: Option<&str>) -> Option<String> {
    value.filter(|v| *v != "default").map(str::to_string)
}

/// `modelPreferenceKey`.
pub fn model_preference_key(model: &ModelRef) -> String {
    format!("{}/{}", model.provider_id, model.model_id)
}

/// `recentModels(model, recent)`.
pub fn recent_models(model: &ModelRef, recent: &[ModelRef]) -> Vec<ModelRef> {
    let mut seen = std::collections::HashSet::new();
    std::iter::once(model)
        .chain(recent.iter())
        .filter(|item| seen.insert(model_preference_key(item)))
        .take(10)
        .cloned()
        .collect()
}

/// `favoriteModels(model, favorite, enabled)`.
pub fn favorite_models(model: &ModelRef, favorite: &[ModelRef], enabled: bool) -> Vec<ModelRef> {
    let key = model_preference_key(model);
    let current: Vec<ModelRef> = favorite.iter().filter(|item| model_preference_key(item) != key).cloned().collect();
    if enabled { std::iter::once(model.clone()).chain(current).collect() } else { current }
}

/// `cycleModelVariant(current, variants)`.
pub fn cycle_model_variant(current: Option<&str>, variants: &[String]) -> Option<String> {
    let named: Vec<&String> = variants.iter().filter(|v| *v != "default").collect();
    if named.is_empty() {
        return None;
    }
    let Some(value) = normalize_model_variant(current) else { return Some(named[0].clone()) };
    let index = named.iter().position(|v| **v == value)?;
    if index == named.len() - 1 {
        return None;
    }
    Some(named[index + 1].clone())
}

/// `decodeModelPreference`: the document with its three fields normalized (other keys kept).
fn decode(value: Option<Json>) -> (Object, ModelPreference) {
    let root = match value {
        Some(Json::Object(o)) => o,
        _ => Object::new(),
    };
    let preference = ModelPreference {
        recent: models(root.get("recent")),
        favorite: models(root.get("favorite")),
        variant: variants(root.get("variant")),
    };
    (root, preference)
}

fn models_json(list: &[ModelRef]) -> Json {
    Json::Array(
        list.iter()
            .map(|m| {
                let mut o = Object::new();
                o.insert("providerID".into(), Json::String(m.provider_id.clone()));
                o.insert("modelID".into(), Json::String(m.model_id.clone()));
                Json::Object(o)
            })
            .collect(),
    )
}

/// A change to the stored preference (`patch`): only the fields set.
#[derive(Default)]
pub struct Patch {
    pub recent: Option<Vec<ModelRef>>,
    pub favorite: Option<Vec<ModelRef>>,
    pub variant: Option<Vec<(String, String)>>,
}

fn read(path: &PathBuf) -> (Object, ModelPreference) {
    let parsed = std::fs::read_to_string(path).ok().and_then(|text| opencode_protocol::wire::json::parse(&text).ok());
    decode(parsed)
}

/// `writeJsonAtomic(filePath, value)`.
fn write_json_atomic(path: &PathBuf, value: &Json) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    let uuid: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let temporary = PathBuf::from(format!("{}.{}.{}.tmp", path.display(), std::process::id(), uuid));
    if let Err(error) = std::fs::write(&temporary, opencode_tui_sync::client::stringify(value)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn update_blocking(path: &PathBuf, locks: &PathBuf, change: Box<dyn FnOnce(&ModelPreference) -> Patch + Send>) -> Result<(), String> {
    let lock = crate::config::file::flock::acquire(&path.to_string_lossy(), locks)?;
    let (mut document, current) = read(path);
    let patch = change(&current);
    if let Some(recent) = patch.recent {
        document.insert("recent".into(), models_json(&recent));
    }
    if let Some(favorite) = patch.favorite {
        document.insert("favorite".into(), models_json(&favorite));
    }
    if let Some(variant) = patch.variant {
        document.insert(
            "variant".into(),
            Json::Object(variant.into_iter().filter(|(k, v)| !k.is_empty() && !v.is_empty()).map(|(k, v)| (k, Json::String(v))).collect()),
        );
    }
    let result = write_json_atomic(path, &Json::Object(document)).map_err(|e| e.to_string());
    let released = lock.release();
    result?;
    released
}

type Listener = Rc<dyn Fn(ModelPreference)>;

struct Inner {
    path: PathBuf,
    locks: PathBuf,
    /// The last queued write (`pending`): the next one waits for it.
    pending: RefCell<Option<tokio::task::JoinHandle<()>>>,
    revision: Cell<u64>,
    listeners: RefCell<Vec<(u64, Listener)>>,
    next: Cell<u64>,
    watching: Cell<bool>,
    reload: Cell<Option<u64>>,
}

/// `createModelPreferenceRepository(filePath)`.
#[derive(Clone)]
pub struct Repository(Rc<Inner>);

impl Repository {
    pub fn new(path: PathBuf) -> Repository {
        let locks = path.parent().map(|p| p.join("locks")).unwrap_or_else(|| PathBuf::from("locks"));
        Repository(Rc::new(Inner {
            path,
            locks,
            pending: RefCell::new(None),
            revision: Cell::new(0),
            listeners: RefCell::new(Vec::new()),
            next: Cell::new(0),
            watching: Cell::new(false),
            reload: Cell::new(None),
        }))
    }

    /// `update(change)`: after the writes before it, under the lock.
    fn update(&self, change: impl FnOnce(&ModelPreference) -> Patch + Send + 'static) {
        let previous = self.0.pending.borrow_mut().take();
        let path = self.0.path.clone();
        let locks = self.0.locks.clone();
        let change: Box<dyn FnOnce(&ModelPreference) -> Patch + Send> = Box::new(change);
        let handle = tokio::task::spawn_local(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = tokio::task::spawn_blocking(move || update_blocking(&path, &locks, change)).await;
        });
        *self.0.pending.borrow_mut() = Some(handle);
    }

    /// `addRecent(model)`.
    pub fn add_recent(&self, model: ModelRef) {
        self.update(move |current| Patch { recent: Some(recent_models(&model, &current.recent)), ..Patch::default() });
    }

    /// `setFavorite(model, enabled)`.
    pub fn set_favorite(&self, model: ModelRef, enabled: bool) {
        self.update(move |current| Patch { favorite: Some(favorite_models(&model, &current.favorite, enabled)), ..Patch::default() });
    }

    /// `saveVariant(model, value)`.
    pub fn save_variant(&self, model: ModelRef, value: Option<String>) {
        let key = model_preference_key(&model);
        let next = normalize_model_variant(value.as_deref()).unwrap_or_else(|| "default".into());
        self.update(move |current| {
            let mut variant = current.variant.clone();
            match variant.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = next,
                None => variant.push((key, next)),
            }
            Patch { variant: Some(variant), ..Patch::default() }
        });
    }

    /// `refresh()`: the stored preference, after the pending writes, to every listener (the latest load wins).
    fn refresh(&self) {
        let revision = self.0.revision.get() + 1;
        self.0.revision.set(revision);
        let previous = self.0.pending.borrow_mut().take();
        let path = self.0.path.clone();
        let this = self.clone();
        let handle = tokio::task::spawn_local(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let loaded = tokio::task::spawn_blocking(move || read(&path).1).await.unwrap_or_default();
            if this.0.revision.get() != revision {
                return;
            }
            let listeners: Vec<Listener> = this.0.listeners.borrow().iter().map(|(_, l)| l.clone()).collect();
            for listener in listeners {
                listener(loaded.clone());
            }
        });
        *self.0.pending.borrow_mut() = Some(handle);
    }

    /// `subscribe(listener)`: loads now, and again whenever the file changes; the returned function unsubscribes.
    pub fn subscribe(&self, listener: impl Fn(ModelPreference) + 'static) -> Box<dyn FnOnce()> {
        let id = self.0.next.get() + 1;
        self.0.next.set(id);
        self.0.listeners.borrow_mut().push((id, Rc::new(listener)));
        self.refresh();
        if !self.0.watching.get() {
            self.0.watching.set(true);
            self.watch();
        }
        let this = self.clone();
        Box::new(move || {
            this.0.listeners.borrow_mut().retain(|(i, _)| *i != id);
            if let Some(timer) = this.0.reload.take() {
                opencode_tui_core::renderer::clear_timeout(timer);
            }
        })
    }

    #[cfg(target_os = "linux")]
    fn watch(&self) {
        let Some(dir) = self.0.path.parent().map(|p| p.to_path_buf()) else { return };
        let name = self.0.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        let spawned = std::thread::Builder::new().name("model-preference-watch".into()).spawn(move || watch_directory(&dir, sender));
        if spawned.is_err() {
            return;
        }
        let this = Rc::downgrade(&self.0);
        tokio::task::spawn_local(async move {
            while let Some(changed) = receiver.recv().await {
                if changed != name && !changed.starts_with(&format!("{name}.")) {
                    continue;
                }
                let Some(inner) = this.upgrade() else { return };
                if inner.listeners.borrow().is_empty() {
                    continue;
                }
                let repository = Repository(inner);
                if let Some(timer) = repository.0.reload.take() {
                    opencode_tui_core::renderer::clear_timeout(timer);
                }
                let target = repository.clone();
                let timer = opencode_tui_core::renderer::set_timeout(50, move || {
                    target.0.reload.set(None);
                    target.refresh();
                });
                repository.0.reload.set(Some(timer));
            }
        });
    }

    #[cfg(not(target_os = "linux"))]
    fn watch(&self) {}
}

/// Reads the directory's inotify events and sends each changed file's name (`fs.watch(dir)`).
#[cfg(target_os = "linux")]
fn watch_directory(dir: &std::path::Path, sender: tokio::sync::mpsc::UnboundedSender<String>) {
    use std::os::unix::ffi::OsStrExt;
    let _ = std::fs::create_dir_all(dir);
    // SAFETY: inotify_init1 has no preconditions; a negative result is an error.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if fd < 0 {
        return;
    }
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return };
    let mask = libc::IN_CREATE | libc::IN_MODIFY | libc::IN_MOVED_TO | libc::IN_DELETE | libc::IN_CLOSE_WRITE;
    // SAFETY: fd is our inotify descriptor and path a NUL-terminated string.
    if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
        // SAFETY: fd is ours.
        unsafe { libc::close(fd) };
        return;
    }
    let mut buffer = [0u8; 4096];
    loop {
        // SAFETY: the buffer is valid for its length.
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read <= 0 {
            break;
        }
        let mut offset = 0usize;
        let header = std::mem::size_of::<libc::inotify_event>();
        while offset + header <= read as usize {
            // SAFETY: the kernel wrote a whole event at this offset; read_unaligned copes with the buffer's alignment.
            let event: libc::inotify_event = unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast()) };
            let start = offset + header;
            let end = (start + event.len as usize).min(read as usize);
            let raw = &buffer[start..end];
            let name = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
            if sender.send(name).is_err() {
                // SAFETY: fd is ours.
                unsafe { libc::close(fd) };
                return;
            }
            offset = end;
        }
    }
    // SAFETY: fd is ours.
    unsafe { libc::close(fd) };
}
