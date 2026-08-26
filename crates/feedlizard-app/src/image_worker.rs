use feedlizard_image::{DecodedImage, ImageLoader, Request};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

pub enum Event {
    Loaded { url: String, image: DecodedImage },
    Failed { url: String },
}

#[derive(Clone)]
pub struct ImageWorker {
    sender: Sender<Request>,
}

impl ImageWorker {
    pub fn start(cache_directory: PathBuf) -> (Self, Receiver<Event>) {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("feedlizard-images".into())
            .spawn(move || run(cache_directory, request_receiver, event_sender))
            .expect("image worker starts");
        (
            Self {
                sender: request_sender,
            },
            event_receiver,
        )
    }

    pub fn load(&self, request: Request) {
        let _ = self.sender.send(request);
    }
}

fn run(cache_directory: PathBuf, requests: Receiver<Request>, events: Sender<Event>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let loader = match ImageLoader::new(cache_directory) {
        Ok(loader) => loader,
        Err(_) => return,
    };
    while let Ok(request) = requests.recv() {
        let url = request.url.clone();
        let event = match runtime.block_on(loader.load(&request)) {
            Ok(image) => Event::Loaded { url, image },
            Err(_) => Event::Failed { url },
        };
        let _ = events.send(event);
    }
}
