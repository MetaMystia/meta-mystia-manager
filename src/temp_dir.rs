use crate::config::TEMP_DIR_NAME;
use crate::metrics::report_event;
use crate::shutdown::register_cleanup;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

type RefCounter = Arc<Mutex<usize>>;
type PathRegistry = Vec<(PathBuf, RefCounter)>;

/// 在 Guard 被 drop 时删除目录
pub struct DirGuard {
    path: PathBuf,
    counter: RefCounter,
}

static REGISTERED_PATHS: OnceLock<Mutex<PathRegistry>> = OnceLock::new();

impl DirGuard {
    fn new(path: PathBuf) -> Self {
        let m = REGISTERED_PATHS.get_or_init(|| Mutex::new(Vec::new()));
        let mut guard = match m.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };

        if let Some((_, counter)) = guard.iter().find(|(p, _)| p == &path) {
            let counter = counter.clone();
            if let Ok(mut count) = counter.lock() {
                *count += 1;
            }
            return Self { path, counter };
        }

        let counter = Arc::new(Mutex::new(1));
        let path_clone = path.clone();

        register_cleanup(move || {
            if path_clone.exists() {
                let _ = std::fs::remove_dir_all(&path_clone);
            }
        });

        guard.push((path.clone(), counter.clone()));

        Self { path, counter }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let should_delete = match self.counter.lock() {
            Ok(mut count) => {
                *count -= 1;
                *count == 0
            }
            Err(_) => true,
        };

        if should_delete && self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
            if let Some(m) = REGISTERED_PATHS.get()
                && let Ok(mut guard) = m.lock()
            {
                guard.retain(|(p, _)| p != &self.path);
            }
        }
    }
}

pub fn create_temp_dir_with_guard(base: &Path) -> std::io::Result<(PathBuf, DirGuard)> {
    let temp_dir = base.join(TEMP_DIR_NAME);

    if let Some(m) = REGISTERED_PATHS.get()
        && let Ok(guard) = m.lock()
        && guard.iter().any(|(p, _)| p == &temp_dir)
    {
        return Ok((temp_dir.clone(), DirGuard::new(temp_dir)));
    }

    if temp_dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&temp_dir)
    {
        report_event(
            "TempDir.CleanupFailed",
            Some(&format!("{};err={}", temp_dir.display(), e)),
        );
    }

    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        report_event(
            "TempDir.CreateFailed",
            Some(&format!("{};err={}", temp_dir.display(), e)),
        );
        return Err(e);
    }

    report_event("TempDir.Created", Some(&temp_dir.display().to_string()));

    Ok((temp_dir.clone(), DirGuard::new(temp_dir)))
}
