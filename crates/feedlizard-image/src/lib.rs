use futures_util::StreamExt;
use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{error::Error, fmt, path::PathBuf, time::Duration};
use url::Url;

pub const MAX_DOWNLOAD_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_SOURCE_PIXELS: u64 = 40_000_000;
pub const MAX_DISK_CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fit {
    Cover,
    Contain,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Request {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub fit: Fit,
}

#[derive(Debug)]
pub struct DecodedImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    InvalidUrl,
    Network(String),
    HttpStatus(u16),
    TooLarge,
    Unsupported,
    Cache(String),
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => write!(formatter, "invalid image URL"),
            Self::Network(value) => write!(formatter, "image request failed: {value}"),
            Self::HttpStatus(status) => write!(formatter, "image request returned HTTP {status}"),
            Self::TooLarge => write!(formatter, "image exceeds safety limits"),
            Self::Unsupported => write!(formatter, "unsupported image"),
            Self::Cache(value) => write!(formatter, "image cache failed: {value}"),
        }
    }
}

impl Error for ImageError {}

#[derive(Clone)]
pub struct ImageLoader {
    client: Client,
    cache_directory: PathBuf,
}

impl ImageLoader {
    pub fn new(cache_directory: PathBuf) -> Result<Self, ImageError> {
        Self::new_with_timeout(cache_directory, Duration::from_secs(25))
    }

    pub fn new_with_timeout(
        cache_directory: PathBuf,
        timeout: Duration,
    ) -> Result<Self, ImageError> {
        let client = Client::builder()
            .user_agent("FeedLizard/0.1 (Linux; image fetch)")
            .https_only(false)
            .redirect(reqwest::redirect::Policy::limited(5))
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .timeout(timeout)
            .build()
            .map_err(|error| ImageError::Network(error.to_string()))?;
        Ok(Self {
            client,
            cache_directory,
        })
    }

    pub async fn load(&self, request: &Request) -> Result<DecodedImage, ImageError> {
        validate_request(request)?;
        let path = self.cache_directory.join(cache_name(&request.url));
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => {
                let bytes = self.download(&request.url).await?;
                tokio::fs::create_dir_all(&self.cache_directory)
                    .await
                    .map_err(|error| ImageError::Cache(error.to_string()))?;
                let temporary = path.with_extension("part");
                tokio::fs::write(&temporary, &bytes)
                    .await
                    .map_err(|error| ImageError::Cache(error.to_string()))?;
                tokio::fs::rename(&temporary, &path)
                    .await
                    .map_err(|error| ImageError::Cache(error.to_string()))?;
                self.trim_cache().await;
                bytes
            }
        };
        decode(&bytes, request)
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, ImageError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| ImageError::Network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(ImageError::HttpStatus(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_DOWNLOAD_BYTES as u64)
        {
            return Err(ImageError::TooLarge);
        }
        let mut output = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ImageError::Network(error.to_string()))?;
            if output.len().saturating_add(chunk.len()) > MAX_DOWNLOAD_BYTES {
                return Err(ImageError::TooLarge);
            }
            output.extend_from_slice(&chunk);
        }
        Ok(output)
    }

    async fn trim_cache(&self) {
        let Ok(mut directory) = tokio::fs::read_dir(&self.cache_directory).await else {
            return;
        };
        let mut entries = Vec::new();
        let mut total = 0_u64;
        while let Ok(Some(entry)) = directory.next_entry().await {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if !metadata.is_file()
                || entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "part")
            {
                continue;
            }
            total = total.saturating_add(metadata.len());
            entries.push((
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                metadata.len(),
                entry.path(),
            ));
        }
        if total <= MAX_DISK_CACHE_BYTES {
            return;
        }
        entries.sort_by_key(|entry| entry.0);
        for (_, size, path) in entries {
            if total <= MAX_DISK_CACHE_BYTES {
                break;
            }
            if tokio::fs::remove_file(path).await.is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

fn validate_request(request: &Request) -> Result<(), ImageError> {
    let url = Url::parse(&request.url).map_err(|_| ImageError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || request.width == 0 || request.height == 0 {
        return Err(ImageError::InvalidUrl);
    }
    Ok(())
}

fn decode(bytes: &[u8], request: &Request) -> Result<DecodedImage, ImageError> {
    let format = image::guess_format(bytes).map_err(|_| ImageError::Unsupported)?;
    if !matches!(
        format,
        ImageFormat::Avif
            | ImageFormat::Gif
            | ImageFormat::Ico
            | ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::WebP
    ) {
        return Err(ImageError::Unsupported);
    }
    let image =
        image::load_from_memory_with_format(bytes, format).map_err(|_| ImageError::Unsupported)?;
    let (width, height) = image.dimensions();
    if u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS {
        return Err(ImageError::TooLarge);
    }
    let resized = match request.fit {
        Fit::Cover => cover(image, request.width, request.height),
        Fit::Contain => image.resize(request.width, request.height, FilterType::Lanczos3),
    };
    let rgba = resized.to_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

fn cover(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    image.resize_to_fill(width, height, FilterType::Lanczos3)
}

fn cache_name(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let mut name = String::with_capacity(digest.len() * 2 + ".image".len());
    for byte in digest {
        write!(&mut name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    name.push_str(".image");
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_urls_and_dimensions() {
        let valid = Request {
            url: "https://example.com/image.png".into(),
            width: 100,
            height: 80,
            fit: Fit::Cover,
        };
        assert_eq!(validate_request(&valid), Ok(()));
        assert_eq!(
            validate_request(&Request {
                url: "file:///tmp/no".into(),
                ..valid
            }),
            Err(ImageError::InvalidUrl)
        );
    }

    #[test]
    fn cache_names_are_stable_and_hide_urls() {
        let name = cache_name("https://example.com/private-topic.jpg");
        assert_eq!(
            name,
            "8855b87882ce33fcaa6edb7b52e2d10e6a992c9fc4d2d27ce26b0cc57a71a23b.image"
        );
        assert!(!name.contains("private"));
    }

    #[test]
    fn decodes_and_resizes_without_network() {
        let source = DynamicImage::new_rgb8(400, 200);
        let mut bytes = std::io::Cursor::new(Vec::new());
        source.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let image = decode(
            &bytes.into_inner(),
            &Request {
                url: "https://example.com/a.png".into(),
                width: 80,
                height: 60,
                fit: Fit::Cover,
            },
        )
        .unwrap();
        assert_eq!((image.width, image.height), (80, 60));
        assert_eq!(image.rgba.len(), 80 * 60 * 4);
    }

    #[test]
    fn decodes_standard_favicon_ico() {
        let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([93, 63, 211, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Ico)
            .unwrap();
        let decoded = decode(
            bytes.get_ref(),
            &Request {
                url: "https://example.com/favicon.ico".into(),
                width: 24,
                height: 24,
                fit: Fit::Contain,
            },
        )
        .unwrap();
        assert_eq!((decoded.width, decoded.height), (24, 24));
    }
}
