//! A virtual phone built only from the public API, for the integration suites.
//!
//! Not every test binary uses every helper, hence the crate-wide `dead_code` allowance.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use mtpx_core::{
    CancelToken, Device, DevicePath, Plan, ProgressEvent, PullJob, RelPath, Report,
    TransferOptions, VirtualDeviceConfig, VirtualStorageConfig,
};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
use tempfile::TempDir;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};

const STORAGE: &str = "Internal Storage";
const CAPACITY: u64 = 1024 * 1024 * 1024;
/// Deep enough that a burst of `FileProgress` never stalls the executor on the collector.
const EVENTS_CAPACITY: usize = 1024;

/// An open virtual device, the directory backing its only storage, and a local directory to
/// pull into.
pub struct VirtualPhone {
    pub device: Device,
    pub serial: String,
    backing: TempDir,
    local: TempDir,
}

/// What a full `pull` produced: the plan it ran, the report it ended with, and every event
/// in between.
pub struct Pulled {
    pub plan: Plan,
    pub report: Report,
    pub events: Vec<ProgressEvent>,
}

impl VirtualPhone {
    pub async fn open(test_name: &str) -> Self {
        Self::open_with(test_name, |_| {}).await
    }

    /// Opens a phone whose config was adjusted by `configure` before the device came up.
    pub async fn open_with(
        test_name: &str,
        configure: impl FnOnce(&mut VirtualDeviceConfig),
    ) -> Self {
        let serial = format!("it-{test_name}");
        let backing = tempfile::tempdir().expect("backing dir");
        let local = tempfile::tempdir().expect("local dir");
        let mut config = config(&serial, backing.path());
        configure(&mut config);
        let device = Device::open_virtual(config)
            .await
            .expect("open virtual device");
        Self {
            device,
            serial,
            backing,
            local,
        }
    }

    /// The directory the phone's storage reads from.
    pub fn backing(&self) -> &Path {
        self.backing.path()
    }

    /// The directory pulls land in.
    pub fn local(&self) -> &Path {
        self.local.path()
    }

    /// Writes `files` under the backing directory, creating parents as needed.
    pub fn seed(&self, files: &[(&str, &[u8])]) {
        for (path, content) in files {
            let full = self.backing.path().join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("create parents");
            fs::write(full, content).expect("seed file");
        }
    }

    /// Plans and runs a pull of `remote` into the local directory, collecting every event.
    pub async fn pull(&self, remote: &str, opts: &TransferOptions) -> Pulled {
        let (tx, rx) = events();
        let collector = collect(rx);
        let job = self.plan(remote, opts, &tx).await.expect("plan_pull");
        let plan = job.plan().clone();
        let (report, events) = run_collecting(job, tx, collector).await;
        Pulled {
            plan,
            report,
            events,
        }
    }

    /// Plans a pull of `remote` into the local directory without running it.
    pub async fn plan(
        &self,
        remote: &str,
        opts: &TransferOptions,
        events: &Sender<ProgressEvent>,
    ) -> mtpx_core::Result<PullJob<'_>> {
        let cancel = CancelToken::new();
        self.device
            .plan_pull(&device_path(remote), self.local(), opts, &cancel, events)
            .await
    }

    /// Every file under the local directory, keyed by its `/`-joined relative path. Partials
    /// and sidecars are listed too, so their absence can be asserted.
    pub fn local_tree(&self) -> BTreeMap<String, Vec<u8>> {
        let mut tree = BTreeMap::new();
        read_tree(self.local(), self.local(), &mut tree);
        tree
    }
}

fn read_tree(root: &Path, dir: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            read_tree(root, &path, into);
            continue;
        }
        let relative = path.strip_prefix(root).expect("under root");
        let key = relative.to_string_lossy().replace('\\', "/");
        into.insert(key, fs::read(&path).expect("read file"));
    }
}

fn config(serial: &str, backing_dir: &Path) -> VirtualDeviceConfig {
    VirtualDeviceConfig {
        manufacturer: "mtpx".into(),
        model: "Virtual Phone".into(),
        serial: serial.to_owned(),
        storages: vec![VirtualStorageConfig {
            description: STORAGE.into(),
            capacity: CAPACITY,
            backing_dir: backing_dir.to_path_buf(),
            read_only: false,
        }],
        event_poll_interval: Duration::ZERO,
        watch_backing_dirs: false,
        ..Default::default()
    }
}

pub fn events() -> (Sender<ProgressEvent>, Receiver<ProgressEvent>) {
    mpsc::channel(EVENTS_CAPACITY)
}

/// Drains `rx` on its own task until every sender is dropped; the core awaits most event
/// sends, so a plan or run needs this running concurrently.
pub fn collect(mut rx: Receiver<ProgressEvent>) -> JoinHandle<Vec<ProgressEvent>> {
    tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(event) = rx.recv().await {
            seen.push(event);
        }
        seen
    })
}

/// Runs `job`, then closes the channel and waits for everything the collector saw.
pub async fn run_collecting(
    job: PullJob<'_>,
    events: Sender<ProgressEvent>,
    collector: JoinHandle<Vec<ProgressEvent>>,
) -> (Report, Vec<ProgressEvent>) {
    let report = job.run(&CancelToken::new(), &events).await.expect("run");
    drop(events);
    (report, collector.await.expect("collector"))
}

pub fn device_path(input: &str) -> DevicePath {
    input.parse().expect("device path")
}

pub fn rel(path: &str) -> RelPath {
    RelPath::new(path.split('/')).expect("relative path")
}

/// Deterministic bytes that do not compress or repeat, so a truncated or shifted copy differs.
pub fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9E37_79B9;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xFF) as u8
        })
        .collect()
}

pub fn count(events: &[ProgressEvent], matches: impl Fn(&ProgressEvent) -> bool) -> usize {
    events.iter().filter(|event| matches(event)).count()
}
