use super::{App, bad, ok, reply};
use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;

pub async fn get(State(app): State<App>) -> Response {
    reply(StatusCode::OK, app.updater.snapshot())
}

#[derive(Deserialize)]
pub struct Settings {
    repo: Option<String>,
    check: Option<bool>,
    auto: Option<bool>,
    stats: Option<bool>,
}

pub async fn put(State(app): State<App>, Json(input): Json<Settings>) -> Response {
    let repo = input.repo.as_deref().map(str::trim);
    if repo.is_some_and(|r| !r.is_empty() && !crate::plugins::catalog::valid_repo(r)) {
        return bad("Репозиторий должен быть в формате владелец/имя");
    }
    match app.store.update(|settings| -> std::io::Result<()> {
        if let Some(repo) = repo {
            settings.update_repo = repo.into();
        }
        if let Some(check) = input.check {
            settings.update_check = check;
        }
        if let Some(auto) = input.auto {
            settings.update_auto = auto;
        }
        if let Some(stats) = input.stats {
            settings.no_stats = !stats;
        }
        Ok(())
    }) {
        Ok(()) => ok(),
        Err(error) => bad(error.to_string()),
    }
}

pub async fn check(State(app): State<App>) -> Response {
    match app.updater.check().await {
        Ok(()) => get(State(app)).await,
        Err(error) => bad(error),
    }
}

pub async fn install(State(app): State<App>) -> Response {
    if crate::updates::locked(crate::version()) {
        return bad("Сборка со Spotify: обновлять через build-jarvis.sh");
    }
    if app.engine.talking() || app.engine.music.playing() {
        return bad("Остановите разговор и музыку перед обновлением");
    }
    match app.updater.install_build(None).await {
        Ok(()) => ok(),
        Err(error) => bad(error),
    }
}

#[derive(Deserialize)]
pub struct Upload {
    module: Option<String>,
}

pub async fn upload(State(app): State<App>, Query(input): Query<Upload>, data: Bytes) -> Response {
    if crate::updates::locked(crate::version()) {
        return bad("Сборка со Spotify: обновлять через build-jarvis.sh");
    }
    if app.engine.talking() || app.engine.music.playing() {
        return bad("Остановите разговор и музыку перед обновлением");
    }
    if input
        .module
        .as_deref()
        .is_some_and(|id| !crate::plugins::valid_id(id))
    {
        return bad("Неверный идентификатор нового модуля");
    }
    match app.updater.upload(data, input.module.as_deref()).await {
        Ok(()) => ok(),
        Err(error) => bad(error),
    }
}

/// Сборка со Spotify не заменяет себя сама: проверка релизов остаётся, установка нет.
fn core_install_allowed(version: &str) -> bool {
    !crate::updates::locked(version)
}

pub fn automatic(app: App) {
    if !app.updater.can_self_update()
        || std::env::var_os("JARVIS_NO_AUTOUPDATE").is_some()
    {
        return;
    }
    let data = app.data_dir.clone();
    // The supervisor keeps the old image until startup is confirmed. Afterwards,
    // its copy could still contain the machine code of a removed module.
    if std::env::var_os("JARVIS_READY_PATH").is_some() {
        tokio::spawn(async move {
            for _ in 0..35 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if !data.join(jarvis_station::PENDING).exists() {
                    let _ = tokio::fs::remove_file(data.join(jarvis_station::PREVIOUS)).await;
                    break;
                }
            }
        });
    }
    tokio::spawn(async move {
        use std::time::Duration;
        tokio::time::sleep(Duration::from_secs(120)).await;
        let mut plugins_checked = None;
        loop {
            let (enabled, auto, now) = app
                .store
                .with(|s| (s.update_check, s.update_auto, s.now()));
            if enabled {
                if app.updater.check_due() {
                    let _ = app.updater.check().await;
                }
                if auto && (3..6).contains(&now.hour()) && super::plugins::idle(&app) {
                    let installing_core = core_install_allowed(crate::version()) && app.updater.available() && app.updater.install_build(None).await.is_ok();
                    if !installing_core && plugins_checked != Some(now.date()) && super::plugins::automatic(&app).await {
                        plugins_checked = Some(now.date());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(20 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::core_install_allowed;

    #[test]
    fn automatic_install_is_blocked_only_for_the_spotify_build() {
        assert!(!core_install_allowed("0.10.16+spotify"));
        assert!(core_install_allowed("0.10.16"));
    }
}
