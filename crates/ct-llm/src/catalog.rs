//! Model list shown in Settings: the CLI's live list when fetched, else the on-disk cache
//! from a previous run, else the hardcoded fallback. Never fails; every problem just means
//! the next source in that order is used.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::client::CliError;
use crate::models::ModelInfo;

const STANDARD_EFFORT: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn model(value: &str, display: &str, effort: &[&str]) -> ModelInfo {
    ModelInfo {
        value: value.to_string(),
        display_name: display.to_string(),
        description: String::new(),
        effort_levels: effort.iter().map(|s| s.to_string()).collect(),
    }
}

/// Used until (or unless) the CLI reports its own list. `default` is the CLI's own default.
pub fn fallback_models() -> Vec<ModelInfo> {
    vec![
        model("default", "기본값 (자동)", &STANDARD_EFFORT),
        model("opus", "Opus", &STANDARD_EFFORT),
        model("sonnet", "Sonnet", &STANDARD_EFFORT),
        model("haiku", "Haiku", &[]),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogSource {
    Fallback,
    Cache,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn() + Send + Sync>;

struct State {
    models: Vec<ModelInfo>,
    source: CatalogSource,
    listeners: Vec<(ListenerId, Listener)>,
    next_id: u64,
}

pub struct ModelCatalog {
    cache_path: Option<PathBuf>,
    state: Mutex<State>,
}

fn model_to_json(m: &ModelInfo) -> Value {
    json!({
        "value": m.value,
        "display_name": m.display_name,
        "description": m.description,
        "effort_levels": m.effort_levels,
    })
}

fn model_from_json(raw: &Value) -> Option<ModelInfo> {
    let obj = raw.as_object()?;
    let value = obj.get("value")?.as_str().filter(|v| !v.is_empty())?;
    let effort_levels = match obj.get("effort_levels") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|l| l.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?,
        Some(_) => return None,
    };
    let display_name = obj
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(value);
    let description = obj.get("description").and_then(Value::as_str).unwrap_or("");
    Some(ModelInfo {
        value: value.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        effort_levels,
    })
}

fn load_cache(path: &Path) -> Option<Vec<ModelInfo>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("Model cache unreadable: {:?}", e.kind());
            }
            return None;
        }
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        tracing::warn!("Model cache is not valid JSON");
        return None;
    };
    let models: Option<Vec<ModelInfo>> = data
        .get("models")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .and_then(|a| a.iter().map(model_from_json).collect());
    if models.is_none() {
        tracing::warn!("Model cache is empty, has an unexpected shape or invalid entries");
    }
    models
}

fn save_cache(path: &Path, models: &[ModelInfo]) -> std::io::Result<()> {
    let fetched_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let payload = json!({
        "fetched_at": fetched_at.to_string(),
        "models": models.iter().map(model_to_json).collect::<Vec<_>>(),
    });
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let text = serde_json::to_string_pretty(&payload).map_err(std::io::Error::other)?;
    let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn with_default_first(mut models: Vec<ModelInfo>) -> Vec<ModelInfo> {
    if !models.iter().any(|m| m.value == "default") {
        models.insert(0, model("default", "기본값 (자동)", &STANDARD_EFFORT));
    }
    models
}

impl ModelCatalog {
    pub fn new(cache_path: Option<PathBuf>) -> Self {
        let cached = cache_path.as_deref().and_then(load_cache);
        let (models, source) = match cached {
            Some(m) => (m, CatalogSource::Cache),
            None => (fallback_models(), CatalogSource::Fallback),
        };
        let models = with_default_first(models);
        tracing::info!(source = ?source, count = models.len(), "Model list loaded");
        Self {
            cache_path,
            state: Mutex::new(State {
                models,
                source,
                listeners: Vec::new(),
                next_id: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self) -> Vec<ModelInfo> {
        self.lock().models.clone()
    }

    pub fn source(&self) -> CatalogSource {
        self.lock().source
    }

    /// The callback runs on the thread that calls `refresh` (normally a background
    /// thread), so it must hand off to the UI thread itself.
    pub fn add_listener(&self, f: Box<dyn Fn() + Send + Sync>) -> ListenerId {
        let mut st = self.lock();
        let id = ListenerId(st.next_id);
        st.next_id += 1;
        st.listeners.push((id, Arc::from(f)));
        id
    }

    pub fn remove_listener(&self, id: ListenerId) {
        self.lock().listeners.retain(|(i, _)| *i != id);
    }

    /// Replaces the list with `fetch()`'s result and persists it. Returns false (list
    /// unchanged) if the fetch fails or returns nothing.
    pub fn refresh(&self, fetch: impl FnOnce() -> Result<Vec<ModelInfo>, CliError>) -> bool {
        let fetched = match fetch() {
            Ok(m) => m,
            Err(e) => {
                tracing::info!(error = ?e, "Model list refresh failed");
                return false;
            }
        };
        if fetched.is_empty() {
            tracing::info!("Model list refresh returned no models");
            return false;
        }
        let models = with_default_first(fetched);
        let count = models.len();
        {
            let mut st = self.lock();
            st.models = models.clone();
            st.source = CatalogSource::Live;
        }
        tracing::info!(count, "Model list refreshed: source=live");
        if let Some(path) = &self.cache_path
            && let Err(e) = save_cache(path, &models)
        {
            tracing::warn!("Model cache not written: {:?}", e.kind());
        }
        let listeners: Vec<Listener> = self
            .lock()
            .listeners
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener())).is_err() {
                tracing::debug!("Model list listener panicked");
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn info(value: &str, name: &str, desc: &str, levels: &[&str]) -> ModelInfo {
        ModelInfo {
            value: value.into(),
            display_name: name.into(),
            description: desc.into(),
            effort_levels: levels.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn default_info() -> ModelInfo {
        info(
            "default",
            "Default (recommended)",
            "",
            &["low", "medium", "high"],
        )
    }
    fn new_model() -> ModelInfo {
        info("claude-opus-6", "Opus 6", "newest", &["low", "high"])
    }
    fn values(models: &[ModelInfo]) -> Vec<String> {
        models.iter().map(|m| m.value.clone()).collect()
    }
    fn live() -> Result<Vec<ModelInfo>, CliError> {
        Ok(vec![default_info(), new_model()])
    }
    fn failing() -> Result<Vec<ModelInfo>, CliError> {
        Err(CliError::Timeout)
    }
    fn empty() -> Result<Vec<ModelInfo>, CliError> {
        Ok(vec![])
    }

    #[test]
    fn get_without_cache_returns_fallback() {
        let models = ModelCatalog::new(None).get();
        assert_eq!(values(&models), ["default", "opus", "sonnet", "haiku"]);
        assert_eq!(models[0].display_name, "기본값 (자동)");
        assert!(models[3].effort_levels.is_empty());
        assert_eq!(
            models[1].effort_levels,
            ["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn refresh_replaces_list() {
        let c = ModelCatalog::new(None);
        assert!(c.refresh(live));
        assert!(values(&c.get()).contains(&"claude-opus-6".to_string()));
    }

    #[test]
    fn refresh_writes_cache_and_new_catalog_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        ModelCatalog::new(Some(path.clone())).refresh(live);
        assert_eq!(
            ModelCatalog::new(Some(path)).get(),
            vec![default_info(), new_model()]
        );
    }

    #[test]
    fn refresh_failure_keeps_previous_list() {
        let c = ModelCatalog::new(None);
        let before = c.get();
        assert!(!c.refresh(failing));
        assert_eq!(c.get(), before);
    }

    #[test]
    fn refresh_empty_list_is_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        let c = ModelCatalog::new(Some(path.clone()));
        let before = c.get();
        assert!(!c.refresh(empty));
        assert_eq!(c.get(), before);
        assert!(!path.exists());
    }

    #[test]
    fn missing_default_entry_is_inserted_first() {
        let c = ModelCatalog::new(None);
        c.refresh(|| Ok(vec![info("opus", "Opus 5.5", "", &["low", "max"])]));
        assert_eq!(values(&c.get())[..2], ["default", "opus"]);
    }

    #[test]
    fn corrupt_cache_falls_back() {
        for content in [
            "{not json",
            r#"{"models": "oops"}"#,
            r#"{"models": []}"#,
            r#"{"models": [{"display_name": "no value"}]}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("models_cache.json");
            std::fs::write(&path, content).unwrap();
            let c = ModelCatalog::new(Some(path));
            assert_eq!(values(&c.get()), ["default", "opus", "sonnet", "haiku"]);
            assert_eq!(c.source(), CatalogSource::Fallback);
        }
    }

    #[test]
    fn unwritable_cache_does_not_fail_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        std::fs::create_dir(&path).unwrap();
        let c = ModelCatalog::new(Some(path));
        assert!(c.refresh(live));
        assert!(values(&c.get()).contains(&"claude-opus-6".to_string()));
    }

    #[test]
    fn cache_file_is_valid_json_with_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        ModelCatalog::new(Some(path.clone())).refresh(live);
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(data["models"][1]["value"], "claude-opus-6");
        assert_eq!(data["models"][1]["effort_levels"][1], "high");
        assert!(data["fetched_at"].is_string());
        assert!(!dir.path().join("models_cache.json.tmp").exists());
    }

    #[test]
    fn source_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        let c = ModelCatalog::new(Some(path.clone()));
        assert_eq!(c.source(), CatalogSource::Fallback);
        assert!(!c.refresh(failing));
        assert!(!c.refresh(empty));
        assert_eq!(c.source(), CatalogSource::Fallback);
        c.refresh(live);
        assert_eq!(c.source(), CatalogSource::Live);
        assert_eq!(ModelCatalog::new(Some(path)).source(), CatalogSource::Cache);
    }

    #[test]
    fn listener_called_once_after_success_and_sees_updated_state() {
        let c = Arc::new(ModelCatalog::new(None));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (c2, s2) = (c.clone(), seen.clone());
        c.add_listener(Box::new(move || {
            s2.lock().unwrap().push((c2.source(), values(&c2.get())));
        }));
        c.refresh(live);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, CatalogSource::Live);
        assert!(seen[0].1.contains(&"claude-opus-6".to_string()));
    }

    #[test]
    fn listener_not_called_on_failure_or_empty() {
        let c = ModelCatalog::new(None);
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        c.add_listener(Box::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }));
        c.refresh(failing);
        c.refresh(empty);
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn panicking_listener_does_not_break_refresh_or_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models_cache.json");
        let c = ModelCatalog::new(Some(path.clone()));
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        c.add_listener(Box::new(|| panic!("widget already deleted")));
        c.add_listener(Box::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(c.refresh(live));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert!(path.exists());
    }

    #[test]
    fn remove_listener_stops_notifications() {
        let c = ModelCatalog::new(None);
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let id = c.add_listener(Box::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }));
        c.remove_listener(id);
        c.refresh(live);
        assert_eq!(n.load(Ordering::SeqCst), 0);
        c.remove_listener(id); // unknown id: must not panic
    }
}
