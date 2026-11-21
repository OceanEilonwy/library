// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real [`Backend`]: drives, the library, MusicBrainz, the key store,
//! settings on disk, the folder picker and the browser.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

use url::Url;

use super::controller::{Backend, EventSender};
use super::film::{self, FilmJob, FilmOutput, FilmScan, FilmTools, ViewingSupport};
use super::pipeline::{self, Endpoints, RipOutput, RipRequest, Services, UploadOutput};
use super::settings::{self, Settings};
use super::state::{ConnectMethod, Event, MUSIC_TYPE, RipJob};
use super::{AppError, Result};
use crate::core::discid;
use crate::drive::fake::FakeDriveProvider;
use crate::drive::{DriveDescriptor, DriveProvider};
use crate::library::api::{ItemSummary, TokenResponse, WhoAmI};
use crate::library::connect::{self, BrowserOpener, SystemBrowser, device, loopback};
use crate::library::keystore::{self, KeyStore, StoredConnection};
use crate::library::{LibraryClient, LibraryError};
use crate::model::{DiscInfo, ReleaseMetadata, Toc, UploadPlan};
use crate::musicbrainz::ReleaseCandidate;

/// What a [`LiveBackend`] is built from.
pub struct LiveConfig {
    /// Outside services.
    pub endpoints: Endpoints,
    /// Where drives come from.
    pub drives: Arc<dyn DriveProvider>,
    /// Where the connection is kept.
    pub keys: Box<dyn KeyStore>,
    /// How web pages are opened.
    pub browser: Arc<dyn BrowserOpener>,
    /// Settings file (`None`: settings are not saved).
    pub settings_path: Option<PathBuf>,
    /// Settings to start with.
    pub settings: Settings,
    /// Where rips are written before upload.
    pub work_root: PathBuf,
}

/// The real backend.
pub struct LiveBackend {
    http: reqwest::Client,
    services: Services,
    drives: Arc<dyn DriveProvider>,
    keys: Box<dyn KeyStore>,
    browser: Arc<dyn BrowserOpener>,
    settings_path: Option<PathBuf>,
    settings: Mutex<Settings>,
    work_root: PathBuf,
    library: Mutex<Option<LibraryClient>>,
}

impl LiveBackend {
    /// A backend from `config`.
    pub fn new(config: LiveConfig) -> Result<Self> {
        let http = crate::library::http_client()?;
        Ok(LiveBackend {
            services: Services::new(http.clone(), &config.endpoints),
            http,
            drives: config.drives,
            keys: config.keys,
            browser: config.browser,
            settings_path: config.settings_path,
            settings: Mutex::new(config.settings),
            work_root: config.work_root,
            library: Mutex::new(None),
        })
    }

    /// The desktop app's backend: settings and the connection in the OS
    /// config directory and keyring, real drives (or the simulated disc in
    /// `fake_disc`), the system browser.
    pub fn desktop(fake_disc: Option<PathBuf>) -> Result<Self> {
        let config_dir = settings::config_dir().ok_or_else(|| AppError::Settings {
            path: PathBuf::from("~"),
            reason: "this computer has no folder for settings".to_owned(),
        })?;
        let settings_path = config_dir.join("settings.json");
        let settings = Settings::load(&settings_path).unwrap_or_else(|error| {
            tracing::warn!(%error, "using default settings");
            Settings::default()
        });
        let drives: Arc<dyn DriveProvider> = match fake_disc {
            Some(dir) => Arc::new(FakeDriveProvider::new(dir)),
            None => system_drives(),
        };
        let work_root = directories::ProjectDirs::from("org", "The Library", "library-rip")
            .map(|dirs| dirs.cache_dir().join("rips"))
            .unwrap_or_else(|| std::env::temp_dir().join("library-rip"));
        LiveBackend::new(LiveConfig {
            endpoints: Endpoints::from_environment()
                .map_err(|error| AppError::Library(LibraryError::Url(error)))?,
            drives,
            keys: Box::new(keystore::default_store(&config_dir)),
            browser: Arc::new(SystemBrowser),
            settings_path: Some(settings_path),
            settings,
            work_root,
        })
    }

    /// The services used while ripping.
    pub fn services(&self) -> &Services {
        &self.services
    }

    /// The connected library client.
    pub fn library(&self) -> Result<LibraryClient> {
        self.library
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(AppError::Library(LibraryError::NotConnected))
    }

    fn set_library(&self, client: Option<LibraryClient>) {
        *self.library.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }

    /// A fresh folder for one rip under the work root.
    fn new_work_dir(&self) -> PathBuf {
        let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
        let mut folder = self.work_root.join(format!("rip-{stamp}"));
        let mut attempt = 2;
        while folder.exists() {
            folder = self.work_root.join(format!("rip-{stamp}-{attempt}"));
            attempt += 1;
        }
        folder
    }

    async fn finish_connect(&self, address: Url, token: TokenResponse) -> Result<WhoAmI> {
        let connection = StoredConnection {
            library_url: address.clone(),
            library_name: token.library.name.clone(),
            key_id: token.key_id,
            key_name: token.key_name.clone(),
            api_key: token.api_key.clone(),
        };
        if let Err(error) = self.keys.save(&connection) {
            tracing::warn!(%error, "the connection could not be saved; it lasts until Library Rip closes");
        }
        let client = LibraryClient::new(self.http.clone(), address, Some(token.api_key));
        let who = client.whoami().await?;
        self.set_library(Some(client));
        Ok(who)
    }
}

/// Real drives when built with libcdio; none otherwise.
fn system_drives() -> Arc<dyn DriveProvider> {
    #[cfg(feature = "cdio")]
    {
        Arc::new(crate::drive::cdio::CdioProvider)
    }
    #[cfg(not(feature = "cdio"))]
    {
        Arc::new(NoDrives)
    }
}

/// No drives at all (builds without libcdio).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDrives;

impl DriveProvider for NoDrives {
    fn list(&self) -> crate::drive::Result<Vec<DriveDescriptor>> {
        Ok(Vec::new())
    }
    fn open(&self, device: &str) -> crate::drive::Result<Box<dyn crate::drive::DiscDrive>> {
        Err(crate::drive::DriveError::Open {
            device: device.to_owned(),
            reason: "this build of Library Rip cannot use real drives".to_owned(),
        })
    }
}

/// Keeps the connection in memory only (headless runs without a key file).
#[derive(Debug, Default)]
pub struct MemoryKeyStore(Mutex<Option<StoredConnection>>);

impl KeyStore for MemoryKeyStore {
    fn load(&self) -> crate::library::Result<Option<StoredConnection>> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
    fn save(&self, connection: &StoredConnection) -> crate::library::Result<()> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(connection.clone());
        Ok(())
    }
    fn clear(&self) -> crate::library::Result<()> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

/// Tells the app which page the browser was sent to (so it can show the
/// address in case no browser opens) before opening it.
struct ReportingBrowser<'a> {
    inner: &'a dyn BrowserOpener,
    events: &'a EventSender,
}

impl BrowserOpener for ReportingBrowser<'_> {
    fn open(&self, url: &Url) -> std::io::Result<()> {
        self.events.send(Event::BrowserOpened {
            url: url.to_string(),
        });
        if let Err(error) = self.inner.open(url) {
            // The page address is on screen; the user can open it by hand.
            tracing::warn!(%error, "could not open the browser");
        }
        Ok(())
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AppError::Task(error.to_string()))
}

impl Backend for LiveBackend {
    fn settings(&self) -> Settings {
        self.settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn saved_address(&self) -> Option<String> {
        match self.keys.load() {
            Ok(connection) => connection.map(|c| c.library_url.to_string()),
            Err(error) => {
                tracing::warn!(%error, "could not read the saved connection");
                None
            }
        }
    }

    async fn restore(&self) -> Result<WhoAmI> {
        let connection = self
            .keys
            .load()?
            .ok_or(AppError::Library(LibraryError::Unauthorised))?;
        let client = LibraryClient::new(
            self.http.clone(),
            connection.library_url,
            Some(connection.api_key),
        );
        match client.whoami().await {
            Ok(who) => {
                self.set_library(Some(client));
                Ok(who)
            }
            Err(LibraryError::Unauthorised) => {
                if let Err(error) = self.keys.clear() {
                    tracing::warn!(%error, "could not forget the revoked key");
                }
                Err(LibraryError::Unauthorised.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn connect(
        &self,
        address: Url,
        method: ConnectMethod,
        events: EventSender,
    ) -> Result<WhoAmI> {
        // Confirms it is a library and that this version is accepted.
        LibraryClient::new(self.http.clone(), address.clone(), None)
            .info()
            .await?;
        let name = connect::computer_name();
        let token = match method {
            ConnectMethod::Browser => {
                let browser = ReportingBrowser {
                    inner: &*self.browser,
                    events: &events,
                };
                loopback::connect(
                    &self.http,
                    &address,
                    &name,
                    &browser,
                    loopback::DEFAULT_TIMEOUT,
                )
                .await?
            }
            ConnectMethod::DeviceCode => {
                let code = device::request_code(&self.http, &address, &name).await?;
                events.send(Event::DeviceCodeShown {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                });
                device::poll_for_key(&self.http, &address, &code).await?
            }
        };
        self.finish_connect(address, token).await
    }

    async fn disconnect(&self) -> Result<()> {
        let client = self
            .library
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let cleared = self.keys.clear();
        if let Some(client) = client {
            match client.revoke().await {
                Ok(()) | Err(LibraryError::Unauthorised) => {}
                Err(error) => return Err(error.into()),
            }
        }
        cleared.map_err(AppError::from)
    }

    async fn list_drives(&self) -> Result<Vec<DriveDescriptor>> {
        let drives = Arc::clone(&self.drives);
        Ok(blocking(move || drives.list()).await??)
    }

    async fn read_disc(&self, device: String) -> Result<DiscInfo> {
        let drives = Arc::clone(&self.drives);
        let drive = blocking(move || drives.open(&device)).await??;
        let (_drive, info) = pipeline::disc_info(drive).await?;
        Ok(info)
    }

    fn film_supported(&self) -> bool {
        true
    }

    async fn scan_film(&self, device: String) -> Result<FilmScan> {
        use crate::video::makemkv::{self, MakeMkv};
        use crate::video::{VideoError, ffmpeg::Ffmpeg};
        let never = AtomicBool::new(false);
        let makemkv = makemkv::discover()
            .await
            .ok_or(VideoError::MakeMkvMissing)?;
        let drives = makemkv.drives(&never).await?;
        let drive = MakeMkv::pick_drive(&drives, &device)?;
        let disc = makemkv.scan(drive.index, &never).await?;
        let viewing = ViewingSupport::from_check(&Ffmpeg::discover().await);
        Ok(FilmScan {
            disc_index: drive.index,
            drive_name: drive.name.clone(),
            disc,
            viewing,
        })
    }

    async fn rip_film(
        &self,
        job: FilmJob,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> Result<FilmOutput> {
        use crate::video::VideoError;
        use crate::video::ffmpeg::Ffmpeg;
        use crate::video::makemkv;
        let makemkv = makemkv::discover()
            .await
            .ok_or(VideoError::MakeMkvMissing)?;
        let ffmpeg = if job.viewing_copy {
            Ffmpeg::discover().await.ok()
        } else {
            None
        };
        let type_slug = film_type_slug(&self.library()?).await?;
        let tools = FilmTools {
            makemkv_version: makemkv.version(&cancel).await,
            makemkv: &makemkv,
            ffmpeg: ffmpeg.as_ref(),
            type_slug: &type_slug,
        };
        let forward = move |event| events.send(Event::FilmProgress(event));
        film::rip_film(&job, &tools, &self.new_work_dir(), &cancel, &forward).await
    }

    async fn find_releases(&self, disc: DiscInfo) -> Result<Vec<ReleaseCandidate>> {
        let ids = discid::disc_ids(&disc.toc)?;
        Ok(self
            .services
            .musicbrainz
            .lookup_disc(&ids.musicbrainz, &disc.toc)
            .await?)
    }

    async fn search_releases(&self, query: String) -> Result<Vec<ReleaseCandidate>> {
        Ok(self.services.musicbrainz.search(&query).await?)
    }

    async fn load_release(&self, candidate: ReleaseCandidate, toc: Toc) -> Result<ReleaseMetadata> {
        load_release(&self.services, &candidate, &toc).await
    }

    async fn search_items(&self, query: String) -> Result<Vec<ItemSummary>> {
        Ok(self.library()?.search_items(&query, MUSIC_TYPE).await?)
    }

    async fn rip(
        &self,
        job: RipJob,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> Result<RipOutput> {
        let drives = Arc::clone(&self.drives);
        let device = job.device.clone();
        let drive = blocking(move || drives.open(&device)).await??;
        let request = RipRequest {
            release: &job.release,
            target: job.target.clone(),
            settings: &job.settings,
            services: &self.services,
            work_dir: self.new_work_dir(),
            cancel,
        };
        let forward = move |event| events.send(Event::Progress(event));
        pipeline::rip(drive, request, &forward).await
    }

    async fn upload(
        &self,
        plan: UploadPlan,
        folder: PathBuf,
        events: EventSender,
        cancel: Arc<AtomicBool>,
    ) -> Result<UploadOutput> {
        let library = self.library()?;
        let forward = move |event| events.send(Event::Progress(event));
        let output = pipeline::upload(&plan, &library, &cancel, &forward).await?;
        remove_work_dir(&self.work_root, &folder);
        Ok(output)
    }

    async fn eject(&self, device: String) -> Result<()> {
        let drives = Arc::clone(&self.drives);
        blocking(move || drives.open(&device).and_then(|mut drive| drive.eject())).await??;
        Ok(())
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner) = settings.clone();
        match self.settings_path.clone() {
            Some(path) => blocking(move || settings.save(&path)).await?,
            None => Ok(()),
        }
    }

    async fn pick_folder(&self) -> Option<PathBuf> {
        rfd::AsyncFileDialog::new()
            .set_title("Choose a folder for local copies")
            .pick_folder()
            .await
            .map(|folder| folder.path().to_owned())
    }

    async fn open_url(&self, url: String) -> Result<()> {
        let parsed = Url::parse(&url).map_err(|error| AppError::Library(error.into()))?;
        let browser = Arc::clone(&self.browser);
        blocking(move || browser.open(&parsed))
            .await?
            .map_err(|source| AppError::Browser { url, source })
    }
}

/// A release's details plus its front cover (a missing cover is not an
/// error).
pub async fn load_release(
    services: &Services,
    candidate: &ReleaseCandidate,
    toc: &Toc,
) -> Result<ReleaseMetadata> {
    let mut release = services
        .musicbrainz
        .release(&candidate.release_mbid, candidate.medium_position)
        .await?;
    if candidate.has_front_cover {
        match services
            .musicbrainz
            .front_cover(&candidate.release_mbid)
            .await
        {
            Ok(cover) => release.cover = cover,
            Err(error) => tracing::warn!(%error, "no cover"),
        }
    }
    super::state::fit_tracks(&mut release, toc);
    Ok(release)
}

/// Removes a finished rip's work folder, but only inside the work root.
/// The slug of the library's film type: `film`, else the first type that
/// looks films up.
async fn film_type_slug(library: &LibraryClient) -> Result<String> {
    let types = library.types().await?;
    types
        .iter()
        .find(|t| t.slug == "film")
        .or_else(|| types.iter().find(|t| t.lookup.as_deref() == Some("film")))
        .map(|t| t.slug.clone())
        .ok_or_else(|| {
            AppError::Task(
                "the library has no Film type to put this disc under; add one on the Librarian's desk"
                    .to_owned(),
            )
        })
}

fn remove_work_dir(work_root: &Path, folder: &Path) {
    if !folder.starts_with(work_root) || folder == work_root {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(folder) {
        tracing::warn!(%error, folder = %folder.display(), "could not remove the work folder");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_folders_are_only_removed_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("rip-1");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = tempfile::tempdir().unwrap();
        remove_work_dir(root.path(), outside.path());
        assert!(outside.path().exists());
        remove_work_dir(root.path(), root.path());
        assert!(root.path().exists());
        remove_work_dir(root.path(), &inside);
        assert!(!inside.exists());
    }
}
