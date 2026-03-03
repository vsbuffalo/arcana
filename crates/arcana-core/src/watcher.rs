use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::config::ArcanaConfig;

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

pub struct VaultWatcher {
    root: PathBuf,
    config: ArcanaConfig,
}

impl VaultWatcher {
    pub fn new(root: PathBuf, config: ArcanaConfig) -> Self {
        VaultWatcher { root, config }
    }

    /// Start watching. Returns a receiver for watch events.
    /// The watcher runs on its own OS thread (notify is sync).
    pub fn start(self) -> crate::errors::Result<WatchHandle> {
        let (tx, rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let root = self.root.clone();
        let config = self.config.clone();

        let thread = std::thread::spawn(move || {
            let event_tx = tx;
            let watch_root = root.clone();
            let watch_config = config;

            let debouncer_result = new_debouncer(
                Duration::from_millis(500),
                move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
                    match events {
                        Ok(events) => {
                            for event in events {
                                let path = &event.path;

                                // Skip non-markdown files
                                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                                    continue;
                                }

                                // Skip excluded paths
                                if let Ok(rel) = path.strip_prefix(&watch_root) {
                                    if watch_config.is_excluded(rel) {
                                        continue;
                                    }
                                }

                                let watch_event = match event.kind {
                                    DebouncedEventKind::Any => {
                                        if path.exists() {
                                            WatchEvent::Modified(path.clone())
                                        } else {
                                            WatchEvent::Deleted(path.clone())
                                        }
                                    }
                                    DebouncedEventKind::AnyContinuous => {
                                        WatchEvent::Modified(path.clone())
                                    }
                                    _ => WatchEvent::Modified(path.clone()),
                                };

                                debug!("watch event: {:?}", watch_event);
                                if event_tx.send(watch_event).is_err() {
                                    return; // Receiver dropped
                                }
                            }
                        }
                        Err(e) => {
                            error!("watch error: {}", e);
                        }
                    }
                },
            );

            match debouncer_result {
                Ok(mut debouncer) => {
                    if let Err(e) = debouncer.watcher().watch(&root, RecursiveMode::Recursive) {
                        error!("failed to start watcher: {}", e);
                        return;
                    }
                    info!("watching vault at {}", root.display());

                    // Block until stop signal
                    let _ = stop_rx.recv();
                    debug!("watcher shutting down");
                }
                Err(e) => {
                    error!("failed to create debouncer: {}", e);
                }
            }
        });

        Ok(WatchHandle {
            rx,
            _stop_tx: stop_tx,
            _thread: thread,
        })
    }
}

pub struct WatchHandle {
    pub rx: mpsc::Receiver<WatchEvent>,
    _stop_tx: mpsc::Sender<()>,
    _thread: std::thread::JoinHandle<()>,
}

impl WatchHandle {
    pub fn recv(&self) -> Option<WatchEvent> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<WatchEvent> {
        self.rx.try_recv().ok()
    }

    /// Collect all pending events into paths for reindexing
    pub fn drain_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            let path = match event {
                WatchEvent::Created(p) | WatchEvent::Modified(p) | WatchEvent::Deleted(p) => p,
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }
}
