//! 临时目录创建与退出清理。

use crate::config::TEMP_DIR_NAME;
use crate::shutdown::register_cleanup;
use crate::telemetry::report_event;

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

type RefCounter = Arc<Mutex<usize>>;
type PathRegistry = Vec<(PathBuf, RefCounter)>;

/// 临时目录守卫：引用计数归零时删除目录，进程退出时由清理回调兜底。
pub struct DirGuard {
    counter: RefCounter,
    path: PathBuf,
}

static REGISTERED_PATHS: OnceLock<Mutex<PathRegistry>> = OnceLock::new();

impl DirGuard {
    fn new(path: PathBuf) -> Self {
        let m = REGISTERED_PATHS.get_or_init(|| Mutex::new(Vec::new()));
        let mut guard = m.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some((_, counter)) = guard.iter().find(|(p, _)| p == &path) {
            let counter = counter.clone();
            *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            return Self { counter, path };
        }

        let counter = Arc::new(Mutex::new(1));
        let path_clone = path.clone();

        register_cleanup(move || {
            if path_clone.exists() {
                let _ = fs::remove_dir_all(&path_clone);
            }
        });

        guard.push((path.clone(), counter.clone()));
        drop(guard);

        Self { counter, path }
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let should_delete = {
            let mut count = self.counter.lock().unwrap_or_else(PoisonError::into_inner);
            *count = count.saturating_sub(1);
            *count == 0
        };

        if should_delete && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
            if let Some(m) = REGISTERED_PATHS.get()
                && let Ok(mut guard) = m.lock()
            {
                guard.retain(|(p, _)| p != &self.path);
            }
        }
    }
}

/// 在给定目录下创建临时目录，并返回退出时自动清理的守卫。
pub fn create_temp_dir_with_guard(base: &Path) -> io::Result<(PathBuf, DirGuard)> {
    let temp_dir = base.join(TEMP_DIR_NAME);

    if let Some(m) = REGISTERED_PATHS.get()
        && let Ok(guard) = m.lock()
        && guard.iter().any(|(p, _)| p == &temp_dir)
    {
        return Ok((temp_dir.clone(), DirGuard::new(temp_dir)));
    }

    if temp_dir.exists()
        && let Err(e) = fs::remove_dir_all(&temp_dir)
    {
        report_event(
            "TempDir.CleanupFailed",
            Some(&format!("{};err={}", temp_dir.display(), e)),
        );
    }

    if let Err(e) = fs::create_dir_all(&temp_dir) {
        report_event(
            "TempDir.CreateFailed",
            Some(&format!("{};err={}", temp_dir.display(), e)),
        );
        return Err(e);
    }

    report_event("TempDir.Created", Some(&temp_dir.display().to_string()));

    Ok((temp_dir.clone(), DirGuard::new(temp_dir)))
}
