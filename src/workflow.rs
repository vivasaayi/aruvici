use crate::config::App;
pub fn generate(app: &App) -> String {
    include_str!("../templates/tauri-release.yml")
        .replace("APP_NAME", &app.name)
        .replace("OWNER/REPO", &app.github)
}
