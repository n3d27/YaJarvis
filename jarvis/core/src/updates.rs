use crate::{
    net,
    plugins::{self, Host},
    settings::Store,
};
use jarvis_station::{FAILURE, IDENTITY, Image, MAX_BINARY, NEXT, PENDING, PREVIOUS, Pending};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

/// Custom builds (version contains `+spotify`) are never replaced by upstream releases.
pub fn locked(version: &str) -> bool {
    version.contains("+spotify")
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
pub struct Build {
    arch: String,
    asset: String,
    modules: Vec<String>,
    bytes: u64,
}

#[derive(Deserialize)]
struct Builds {
    version: String,
    builds: Vec<Build>,
}

struct Candidate {
    image: Image,
    url: String,
    digest: String,
    notes: String,
}

#[derive(Default)]
struct Status {
    checked: Option<Instant>,
    candidate: Option<Candidate>,
    error: String,
    busy: String,
}

pub struct Updater {
    data: PathBuf,
    legacy_run: PathBuf,
    store: Store,
    host: Arc<Host>,
    status: Mutex<Status>,
    operation: tokio::sync::Mutex<()>,
}

impl Updater {
    fn can_apply(&self, image: &Image) -> bool {
        plugins::newer(&image.version, crate::version())
            || (image.version == crate::version()
                && !same_modules(&image.modules, &plugins::compiled_modules()))
    }

    pub fn new(data: PathBuf, store: Store, host: Arc<Host>) -> Self {
        Self {
            data,
            legacy_run: PathBuf::from("/tmp/jarvisd"),
            store,
            host,
            status: Mutex::default(),
            operation: tokio::sync::Mutex::new(()),
        }
    }

    fn legacy_next_path(&self) -> Option<PathBuf> {
        let script = std::fs::read_to_string(self.data.join("jarvisd.sh")).ok()?;
        (script.contains("apply_update") && script.contains("jarvisd.next"))
            .then(|| self.legacy_run.join("jarvisd.next"))
    }

    pub fn can_self_update(&self) -> bool {
        std::env::var_os("JARVIS_READY_PATH").is_some() || self.legacy_next_path().is_some()
    }

    pub fn repo(&self) -> String {
        self.store.with(|s| {
            if s.update_repo.is_empty() {
                crate::DEFAULT_REPO.to_owned()
            } else {
                s.update_repo.clone()
            }
        })
    }

    fn required(&self) -> Vec<String> {
        self.host
            .inner
            .lock()
            .unwrap()
            .plugins
            .values()
            .filter(|p| p.manifest.kind == "module")
            .map(|p| p.manifest.id.clone())
            .collect()
    }

    pub fn available(&self) -> bool {
        self.status
            .lock()
            .unwrap()
            .candidate
            .as_ref()
            .is_some_and(|c| self.can_apply(&c.image))
    }

    pub fn invalidate(&self) {
        let mut status = self.status.lock().unwrap();
        status.checked = None;
        status.candidate = None;
    }

    pub fn check_due(&self) -> bool {
        self.status
            .lock()
            .unwrap()
            .checked
            .is_none_or(|checked| checked.elapsed() >= Duration::from_secs(6 * 3600))
    }

    pub fn snapshot(&self) -> Value {
        let s = self.status.lock().unwrap();
        let settings = self.store.get();
        let previous = std::fs::read_to_string(self.data.join(FAILURE)).unwrap_or_default();
        json!({"version":crate::version(),"repo":self.repo(),"check":settings.update_check,"auto":settings.update_auto,
            "asset":format!("jarvisd-{}", jarvis_station::arch()), "stats":!settings.no_stats,
            "latest":s.candidate.as_ref().map(|c| &c.image.version),"notes":s.candidate.as_ref().map(|c| &c.notes),
            "available":s.candidate.as_ref().is_some_and(|c| self.can_apply(&c.image)),
            "checked":s.checked.map(|t| if t.elapsed()<Duration::from_secs(3600) {"недавно"} else {"ранее"}),
            "busy":s.busy,"error":if s.error.is_empty() { previous } else { s.error.clone() }})
    }

    async fn candidate(&self, required: Vec<String>) -> Result<Candidate, String> {
        let repo = self.repo();
        if !plugins::catalog::valid_repo(&repo) {
            return Err("Репозиторий должен быть в формате владелец/имя".into());
        }
        let release: Release = serde_json::from_value(
            net::get_json(&format!(
                "{}/repos/{repo}/releases/latest",
                plugins::catalog::github_api()
            ))
            .await?,
        )
        .map_err(|_| "Некорректный ответ о релизе")?;
        let find = |name: &str| -> Result<&str, String> {
            release
                .assets
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.browser_download_url.as_str())
                .ok_or_else(|| format!("В релизе нет {name}"))
        };
        let manifest = net::download(find("rust-builds.json")?, 256 << 10).await?;
        let builds: Builds =
            serde_json::from_slice(&manifest).map_err(|_| "Некорректный каталог Rust-сборок")?;
        if release.tag_name.trim_start_matches('v') != builds.version {
            return Err("Версия каталога не совпадает с релизом".into());
        }
        let build = select(&builds.builds, jarvis_station::arch(), &required)
            .ok_or("В релизе нет сборки ровно с выбранными Rust-модулями")?;
        let catalog = net::download(find("SHA256SUMS")?, 256 << 10).await?;
        let digest = jarvis_station::checksum(
            std::str::from_utf8(&catalog).map_err(|_| "Некорректный SHA256SUMS")?,
            &build.asset,
        )
        .ok_or("В SHA256SUMS нет контрольной суммы сборки")?;
        Ok(Candidate {
            image: Image {
                format: IDENTITY.into(),
                version: builds.version,
                arch: build.arch.clone(),
                modules: build.modules.clone(),
            },
            url: find(&build.asset)?.into(),
            digest,
            notes: release
                .body
                .unwrap_or_default()
                .chars()
                .take(4000)
                .collect(),
        })
    }

    pub async fn check(&self) -> Result<(), String> {
        let result = self.candidate(self.required()).await;
        let mut s = self.status.lock().unwrap();
        s.checked = Some(Instant::now());
        let result = match result {
            Ok(candidate) => {
                s.candidate = Some(candidate);
                s.error.clear();
                Ok(())
            }
            Err(error) => {
                s.candidate = None;
                s.error = error.clone();
                Err(error)
            }
        };
        result
    }

    pub async fn install_build(&self, module: Option<&str>) -> Result<(), String> {
        let _guard = self
            .operation
            .try_lock()
            .map_err(|_| "Обновление уже идёт")?;
        self.require_supervisor(module)?;
        self.status.lock().unwrap().busy = "скачиваю обновление".into();
        let result = async {
            let mut required = self.required();
            if let Some(id) = module {
                required.push(id.to_string());
            }
            let candidate = self.candidate(required).await?;
            if !self.can_apply(&candidate.image) {
                return Err(if module.is_some() {
                    "Для этой версии ядра сборка модуля ещё не опубликована"
                } else {
                    "Обновлений нет"
                }
                .into());
            }
            let path = std::env::temp_dir()
                .join(format!("jarvis-update-{}", crate::settings::rand_hex(6)));
            let result = async {
                let mut response = net::direct()
                    .get(&candidate.url)
                    .send()
                    .await
                    .map_err(|e| net::short(&e))?
                    .error_for_status()
                    .map_err(|e| net::short(&e))?;
                let mut file = tokio::fs::File::create(&path)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut received = 0u64;
                while let Some(chunk) = response.chunk().await.map_err(|e| net::short(&e))? {
                    received += chunk.len() as u64;
                    if received > MAX_BINARY {
                        return Err("Сборка больше 32 МБ".into());
                    }
                    file.write_all(&chunk).await.map_err(|e| e.to_string())?;
                }
                file.flush().await.map_err(|e| e.to_string())?;
                drop(file);
                self.stage(&path, Some(&candidate), module).await
            }
            .await;
            let _ = tokio::fs::remove_file(path).await;
            result
        }
        .await;
        self.status.lock().unwrap().busy.clear();
        result
    }

    pub async fn upload(
        &self,
        data: axum::body::Bytes,
        module: Option<&str>,
    ) -> Result<(), String> {
        let _guard = self
            .operation
            .try_lock()
            .map_err(|_| "Обновление уже идёт")?;
        self.require_supervisor(module)?;
        let path =
            std::env::temp_dir().join(format!("jarvis-upload-{}", crate::settings::rand_hex(6)));
        let written = tokio::fs::write(&path, &data)
            .await
            .map_err(|e| e.to_string());
        drop(data);
        let result = match written {
            Ok(()) => self.stage(&path, None, module).await,
            Err(e) => Err(e),
        };
        let _ = tokio::fs::remove_file(path).await;
        result
    }

    fn require_supervisor(&self, module: Option<&str>) -> Result<(), String> {
        if std::env::var_os("JARVIS_READY_PATH").is_some() {
            if self.data.join(PENDING).exists() {
                return Err("Сборка уже ждёт перезапуска".into());
            }
            return Ok(());
        }
        if let Some(next) = self.legacy_next_path() {
            if module.is_some() {
                return Err(
                    "Для установки Rust-модуля сначала обновите службу запуска установщиком".into(),
                );
            }
            if next.exists() {
                return Err("Сборка уже ждёт перезапуска".into());
            }
            return Ok(());
        }
        Err("Сначала обновите службу запуска установщиком".into())
    }

    async fn stage(
        &self,
        path: &Path,
        official: Option<&Candidate>,
        module: Option<&str>,
    ) -> Result<(), String> {
        let digest = jarvis_station::sha256(path).map_err(|e| e.to_string())?;
        if official.is_some_and(|candidate| candidate.digest != digest) {
            return Err("Контрольная сумма не совпала".into());
        }
        let image = identify(path).await?;
        let mut expected = self.required();
        if let Some(id) = module {
            if !expected.iter().any(|m| m == id) {
                expected.push(id.to_owned());
            }
        }
        if let Some(candidate) = official {
            if candidate.image.version != image.version
                || !same_modules(&candidate.image.modules, &image.modules)
            {
                return Err("Сборка отличается от каталога релиза".into());
            }
        }
        if !same_modules(&expected, &image.modules) {
            return Err(
                "Загруженная сборка должна содержать ровно установленные и выбранные Rust-модули"
                    .into(),
            );
        }
        if std::env::var_os("JARVIS_READY_PATH").is_some() {
            let pending = Pending {
                version: image.version,
                sha256: digest,
                module: module.map(str::to_owned),
            };
            let next = self.data.join(NEXT);
            let saved = self.data.join(PREVIOUS);
            if saved.exists() {
                std::fs::remove_file(saved)
                    .or_else(|e| {
                        if e.kind() == std::io::ErrorKind::NotFound {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| e.to_string())?;
            }
            let copied = (|| -> std::io::Result<()> {
                std::fs::copy(path, &next)?;
                std::fs::File::open(&next)?.sync_all()?;
                jarvis_station::atomic(
                    &self.data.join(PENDING),
                    &serde_json::to_vec(&pending).map_err(std::io::Error::other)?,
                )
            })();
            if let Err(error) = copied {
                if !self.data.join(PENDING).exists() {
                    let _ = std::fs::remove_file(next);
                }
                return Err(error.to_string());
            }
        } else if let Some(next) = self.legacy_next_path() {
            if module.is_some() {
                return Err(
                    "Для установки Rust-модуля сначала обновите службу запуска установщиком".into(),
                );
            }
            if let Some(parent) = next.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let part = next.with_extension("next.part");
            let copied = (|| -> std::io::Result<()> {
                std::fs::copy(path, &part)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
                }
                std::fs::File::open(&part)?.sync_all()?;
                std::fs::rename(&part, &next)
            })();
            if let Err(error) = copied {
                let _ = std::fs::remove_file(&part);
                return Err(error.to_string());
            }
        } else {
            return Err("Сначала обновите службу запуска установщиком".into());
        }
        let _ = std::fs::remove_file(self.data.join(FAILURE));
        let host = self.host.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _ = tokio::time::timeout(Duration::from_secs(5), host.stop_all()).await;
            std::process::exit(0);
        });
        Ok(())
    }
}

pub(crate) fn same_modules(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left.iter().all(|id| right.contains(id))
        && right.iter().all(|id| left.contains(id))
}

fn select<'a>(builds: &'a [Build], arch: &str, required: &[String]) -> Option<&'a Build> {
    builds
        .iter()
        .filter(|b| {
            b.arch == arch
                && (1..=MAX_BINARY).contains(&b.bytes)
                && b.asset.starts_with("jarvisd-")
                && same_modules(&b.modules, required)
        })
        .min_by_key(|b| b.bytes)
}

async fn identify(path: &Path) -> Result<Image, String> {
    let bytes = tokio::fs::metadata(path)
        .await
        .map_err(|e| e.to_string())?
        .len();
    if !(1 << 20..=MAX_BINARY).contains(&bytes) {
        return Err("Неверный размер сборки".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut cmd = tokio::process::Command::new(path);
    let output = tokio::time::timeout(
        Duration::from_secs(6),
        cmd.arg("core-info").kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| "Проверка сборки заняла слишком много времени")?
    .map_err(|_| "Сборка не запускается на колонке")?;
    let image: Image =
        serde_json::from_slice(&output.stdout).map_err(|_| "Ожидается сборка ядра Rust")?;
    if !output.status.success()
        || image.format != IDENTITY
        || image.arch != jarvis_station::arch()
        || image.version.is_empty()
    {
        return Err("Неподходящая сборка ядра".into());
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_build_is_locked() {
        assert!(locked("0.10.16+spotify"));
        assert!(!locked("0.10.16"));
        assert!(!locked("dev"));
    }
    #[test]
    fn selects_only_the_exact_installed_module_set() {
        let mk = |arch: &str, bytes, ids: &[&str]| Build {
            arch: arch.into(),
            asset: format!("jarvisd-{arch}-{bytes}"),
            bytes,
            modules: ids.iter().map(|id| id.to_string()).collect(),
        };
        let builds = [
            mk("arm64", 100, &[]),
            mk("arm64", 200, &["vpn", "training"]),
            mk("armv7", 10, &["training"]),
        ];
        assert_eq!(select(&builds, "arm64", &[]).unwrap().bytes, 100);
        assert_eq!(
            select(&builds, "arm64", &["training".into(), "vpn".into()])
                .unwrap()
                .bytes,
            200
        );
        assert!(select(&builds, "arm64", &["training".into()]).is_none());
        assert!(select(&builds, "arm64", &["unavailable".into()]).is_none());
    }

    #[test]
    fn detects_legacy_jarvisd_sh_only_with_apply_update() {
        let dir =
            std::env::temp_dir().join(format!("jarvis-upd-{}", crate::settings::rand_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("settings.json")).unwrap();
        let host = crate::plugins::Host::new(dir.clone(), crate::plugins::Hooks::default());
        let updater = Updater::new(dir.clone(), store, host);

        assert!(updater.legacy_next_path().is_none());

        std::fs::write(dir.join("jarvisd.sh"), "exec \"$D/jarvis-supervisor\"").unwrap();
        assert!(updater.legacy_next_path().is_none());

        std::fs::write(
            dir.join("jarvisd.sh"),
            "apply_update() { cp \"$RUN/jarvisd.next\" \"$D/jarvisd\"; }",
        )
        .unwrap();
        assert_eq!(
            updater.legacy_next_path(),
            Some(PathBuf::from("/tmp/jarvisd/jarvisd.next"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
