use feedlizard_image::{DecodedImage, ImageLoader, Request};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const TRANSIENT_FAILURE_BACKOFF: Duration = Duration::from_secs(5 * 60);
const PERSISTENT_FAILURE_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy)]
struct FailureState {
    retry_at: Instant,
}

#[derive(Default)]
struct FailureCache {
    entries: HashMap<String, FailureState>,
}

impl FailureCache {
    fn permits(&mut self, url: &str, now: Instant) -> bool {
        match self.entries.get(url) {
            Some(failure) if now < failure.retry_at => false,
            Some(_) => {
                self.entries.remove(url);
                true
            }
            None => true,
        }
    }

    fn record(&mut self, url: String, retry_at: Instant) {
        self.entries.insert(url, FailureState { retry_at });
    }

    fn clear(&mut self, url: &str) {
        self.entries.remove(url);
    }
}

pub enum Event {
    Loaded {
        request: Request,
        image: DecodedImage,
    },
    Failed {
        request: Request,
    },
}

#[derive(Clone)]
pub struct ImageWorker {
    sender: Sender<Request>,
    events: Sender<Event>,
    pending: Arc<Mutex<HashSet<Request>>>,
    failures: Arc<Mutex<FailureCache>>,
}

impl ImageWorker {
    pub fn start(cache_directory: PathBuf) -> (Self, Receiver<Event>) {
        Self::start_with_options(cache_directory, 4, std::time::Duration::from_secs(25))
    }

    pub fn start_with_options(
        cache_directory: PathBuf,
        worker_count: usize,
        timeout: std::time::Duration,
    ) -> (Self, Receiver<Event>) {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let requests = Arc::new(Mutex::new(request_receiver));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let failures = Arc::new(Mutex::new(FailureCache::default()));
        for index in 0..worker_count.clamp(1, 12) {
            let requests = Arc::clone(&requests);
            let events = event_sender.clone();
            let pending = Arc::clone(&pending);
            let failures = Arc::clone(&failures);
            let cache_directory = cache_directory.clone();
            thread::Builder::new()
                .name(format!("feedlizard-image-{index}"))
                .spawn(move || {
                    run(
                        cache_directory,
                        timeout,
                        requests,
                        pending,
                        failures,
                        events,
                    )
                })
                .expect("image worker starts");
        }
        (
            Self {
                sender: request_sender,
                events: event_sender,
                pending,
                failures,
            },
            event_receiver,
        )
    }

    pub fn load(&self, request: Request) {
        let permitted = self
            .failures
            .lock()
            .map(|mut failures| failures.permits(&request.url, Instant::now()))
            .unwrap_or(false);
        if !permitted {
            let _ = self.events.send(Event::Failed { request });
            return;
        }
        let should_send = self
            .pending
            .lock()
            .map(|mut pending| pending.insert(request.clone()))
            .unwrap_or(false);
        if should_send
            && self.sender.send(request.clone()).is_err()
            && let Ok(mut pending) = self.pending.lock()
        {
            pending.remove(&request);
        }
    }
}

fn run(
    cache_directory: PathBuf,
    timeout: std::time::Duration,
    requests: Arc<Mutex<Receiver<Request>>>,
    pending: Arc<Mutex<HashSet<Request>>>,
    failures: Arc<Mutex<FailureCache>>,
    events: Sender<Event>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let loader = match ImageLoader::new_with_timeout(cache_directory, timeout) {
        Ok(loader) => loader,
        Err(_) => return,
    };
    loop {
        let request = match requests.lock().map(|receiver| receiver.recv()) {
            Ok(Ok(request)) => request,
            _ => break,
        };
        let event = match runtime.block_on(loader.load(&request)) {
            Ok(image) => {
                if let Ok(mut failures) = failures.lock() {
                    failures.clear(&request.url);
                }
                Event::Loaded {
                    request: request.clone(),
                    image,
                }
            }
            Err(error) => {
                let retry_at = Instant::now() + failure_backoff(&error);
                if let Ok(mut failures) = failures.lock() {
                    failures.record(request.url.clone(), retry_at);
                }
                Event::Failed {
                    request: request.clone(),
                }
            }
        };
        if let Ok(mut pending) = pending.lock() {
            pending.remove(&request);
        }
        let _ = events.send(event);
    }
}

fn failure_backoff(error: &feedlizard_image::ImageError) -> Duration {
    match error {
        feedlizard_image::ImageError::Network(_)
        | feedlizard_image::ImageError::HttpStatus(408 | 429 | 500..=599)
        | feedlizard_image::ImageError::Cache(_) => TRANSIENT_FAILURE_BACKOFF,
        feedlizard_image::ImageError::InvalidUrl
        | feedlizard_image::ImageError::HttpStatus(_)
        | feedlizard_image::ImageError::TooLarge
        | feedlizard_image::ImageError::Unsupported => PERSISTENT_FAILURE_BACKOFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feedlizard_image::ImageError;

    #[test]
    fn suppresses_failed_url_until_retry_deadline() {
        let now = Instant::now();
        let mut failures = FailureCache::default();
        failures.record(
            "https://example.com/icon.png".into(),
            now + Duration::from_secs(60),
        );

        assert!(!failures.permits("https://example.com/icon.png", now));
        assert!(failures.permits(
            "https://example.com/icon.png",
            now + Duration::from_secs(60)
        ));
        assert!(failures.permits("https://example.com/icon.png", now));
    }

    #[test]
    fn failure_cache_is_scoped_by_url_and_can_be_cleared() {
        let now = Instant::now();
        let mut failures = FailureCache::default();
        failures.record(
            "https://example.com/a.png".into(),
            now + Duration::from_secs(60),
        );

        assert!(failures.permits("https://example.com/b.png", now));
        failures.clear("https://example.com/a.png");
        assert!(failures.permits("https://example.com/a.png", now));
    }

    #[test]
    fn persistent_http_failures_back_off_longer_than_transient_failures() {
        assert_eq!(
            failure_backoff(&ImageError::HttpStatus(403)),
            PERSISTENT_FAILURE_BACKOFF
        );
        assert_eq!(
            failure_backoff(&ImageError::HttpStatus(404)),
            PERSISTENT_FAILURE_BACKOFF
        );
        assert_eq!(
            failure_backoff(&ImageError::HttpStatus(503)),
            TRANSIENT_FAILURE_BACKOFF
        );
        assert_eq!(
            failure_backoff(&ImageError::Network("offline".into())),
            TRANSIENT_FAILURE_BACKOFF
        );
    }
}
