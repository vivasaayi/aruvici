// Copy into an application's src-tauri/src/isolation.rs. This module deliberately
// is not compiled into aruvici (the CLI has no Tauri dependency).
use std::path::PathBuf;
use tauri::Manager;

// Keep this equal to the application's EXISTING release bundle ID.
const RELEASE_ID: &str = "com.example.notes";

pub fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(debug_assertions)]
    {
        // Test entrypoints should create a TempDir if ARUVICI_TEST_DATA is absent.
        // Never run tests through the release branch below.
        if let Some(path) = std::env::var_os("ARUVICI_TEST_DATA") {
            return Ok(PathBuf::from(path));
        }
        let expected = app.path().data_dir()?.join(format!("{RELEASE_ID}.dev"));
        if let Some(path) = std::env::var_os("ARUVICI_DEV_DATA") {
            // Explicitly require the audited conventional location. Do not accept
            // an arbitrary environment path that might be the production DB.
            if PathBuf::from(path) != expected {
                return Err("ARUVICI_DEV_DATA differs from the audited debug path".into());
            }
        }
        Ok(expected)
    }
    #[cfg(not(debug_assertions))]
    {
        // Preserve the pre-existing production location when adapting this module.
        Ok(app.path().data_dir()?.join(RELEASE_ID))
    }
}

pub fn window_title(title: &str) -> String {
    if cfg!(debug_assertions) { format!("{title} — DEV") } else { title.to_string() }
}

pub fn label_dev_windows(app: &tauri::AppHandle) -> tauri::Result<()> {
    #[cfg(debug_assertions)]
    for window in app.webview_windows().values() {
        let title = window.title()?;
        if !title.ends_with(" — DEV") { window.set_title(&window_title(&title))?; }
    }
    Ok(())
}
