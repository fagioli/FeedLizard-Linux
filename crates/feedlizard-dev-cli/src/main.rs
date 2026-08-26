use feedlizard_network::{
    CacheValidators, CancellationToken, FetchOutcome, FetchPolicy, HttpClient,
};
use feedlizard_refresh::{AddFeedResult, RefreshConfig, RefreshCoordinator};
use feedlizard_storage::{ArticleScope, Library};
use std::{
    env, fs,
    path::Path,
    process::ExitCode,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        return usage();
    };
    match command {
        "init" if arguments.len() == 2 => {
            let library = Library::open(&arguments[1])?;
            println!("schema {}", library.schema_version()?);
        }
        "import-feed-fixture" if arguments.len() == 4 => {
            let mut library = Library::open(&arguments[1])?;
            let input = fs::read_to_string(&arguments[2])?;
            let stats = library.ingest_document(&arguments[3], &input, now())?;
            println!(
                "inserted={} updated={} duplicate_items={}",
                stats.inserted, stats.updated, stats.duplicates_in_document
            );
        }
        "list-feeds" if arguments.len() == 2 => {
            for feed in Library::open(&arguments[1])?.list_feeds()? {
                println!(
                    "{}\t{}\t{}",
                    feed.stable_id, feed.display_name, feed.fetch_url
                );
            }
        }
        "list-unread" if arguments.len() == 2 => print_articles(
            &Library::open(&arguments[1])?
                .article_page(ArticleScope::Unread, 50, None)?
                .items,
        ),
        "stats" if arguments.len() == 2 => {
            let stats = Library::open(&arguments[1])?.stats()?;
            println!(
                "feeds={} folders={} articles={} unread={} starred={}",
                stats.feeds, stats.folders, stats.articles, stats.unread, stats.starred
            );
        }
        "search" if arguments.len() >= 3 => {
            print_articles(&Library::open(&arguments[1])?.search(&arguments[2..].join(" "), 50)?)
        }
        "mark-all-read" if arguments.len() == 2 => {
            let mut library = Library::open(&arguments[1])?;
            println!(
                "changed={}",
                library.mark_all_read(ArticleScope::Library, now())?
            );
        }
        "cleanup" if arguments.len() == 2 => {
            let mut library = Library::open(&arguments[1])?;
            println!(
                "deleted={}",
                library.cleanup_retention(
                    now() - feedlizard_core::domain::RETENTION_SECONDS,
                    10_000
                )?
            );
        }
        "opml-import" if arguments.len() == 3 => {
            let mut library = Library::open(&arguments[1])?;
            let stats = library.import_opml(&fs::read_to_string(&arguments[2])?, now())?;
            println!(
                "feeds_added={} duplicates={} folders_created={} failures={}",
                stats.feeds_added, stats.duplicates, stats.folders_created, stats.failed_entries
            );
        }
        "opml-export" if arguments.len() == 3 => {
            let library = Library::open(&arguments[1])?;
            fs::write(
                &arguments[2],
                library.export_opml("Thu, 01 Jan 1970 00:00:00 +0000")?,
            )?;
            println!("exported={}", arguments[2]);
        }
        "benchmark" if arguments.len() == 3 => {
            benchmark(Path::new(&arguments[1]), arguments[2].parse()?)?
        }
        "fetch" if arguments.len() == 2 => {
            match http_client()?
                .fetch_feed(
                    &arguments[1],
                    &CacheValidators::default(),
                    &CancellationToken::default(),
                )
                .await?
            {
                FetchOutcome::Modified(response) => println!(
                    "status={} bytes={} final_url={} content_type={}",
                    response.status,
                    response.bytes_received,
                    response.final_url,
                    response.content_type.as_deref().unwrap_or("-")
                ),
                FetchOutcome::NotModified(response) => println!(
                    "status={} unchanged final_url={}",
                    response.status, response.final_url
                ),
            }
        }
        "discover" if arguments.len() == 2 => {
            for candidate in http_client()?
                .discover(&arguments[1], &CancellationToken::default())
                .await?
                .candidates
            {
                println!(
                    "{}\t{}\t{}",
                    candidate.rank, candidate.format_hint, candidate.url
                );
            }
        }
        "add" if arguments.len() == 3 => {
            let mut library = Library::open(&arguments[1])?;
            match refresh_coordinator()?
                .add_url(&mut library, &arguments[2], &CancellationToken::default())
                .await?
            {
                AddFeedResult::Added { feed_id, ingest } => println!(
                    "feed_id={feed_id} inserted={} updated={}",
                    ingest.inserted, ingest.updated
                ),
                AddFeedResult::Candidates(result) => {
                    for candidate in result.candidates {
                        println!(
                            "candidate\t{}\t{}\t{}",
                            candidate.rank, candidate.format_hint, candidate.url
                        );
                    }
                }
            }
        }
        "refresh" if arguments.len() == 3 => {
            let mut library = Library::open(&arguments[1])?;
            let result = refresh_coordinator()?
                .refresh_one(&mut library, &arguments[2], &CancellationToken::default())
                .await?;
            print_refresh(&result);
        }
        "refresh-all" if arguments.len() == 2 => {
            let mut library = Library::open(&arguments[1])?;
            let result = refresh_coordinator()?
                .refresh_all(&mut library, &CancellationToken::default())
                .await?;
            for feed in &result.feeds {
                print_refresh(feed);
            }
            println!(
                "total={} completed={} successful={} unchanged={} failed={} cancelled={}",
                result.summary.total,
                result.summary.completed,
                result.summary.successful,
                result.summary.unchanged,
                result.summary.failed,
                result.summary.cancelled
            );
        }
        _ => return usage(),
    }
    Ok(())
}

fn http_client() -> Result<HttpClient, Box<dyn std::error::Error>> {
    Ok(HttpClient::new(FetchPolicy::default())?)
}
fn refresh_coordinator() -> Result<RefreshCoordinator, Box<dyn std::error::Error>> {
    Ok(RefreshCoordinator::new(
        http_client()?,
        RefreshConfig::default(),
    ))
}
fn print_refresh(result: &feedlizard_refresh::RefreshResult) {
    println!(
        "{}\t{:?}\tstatus={}\tinserted={}\tupdated={}\tbytes={}\tfetch_ms={:.3}\twork_ms={:.3}",
        result.feed_id,
        result.state,
        result
            .status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".into()),
        result.inserted,
        result.updated,
        result.bytes_received,
        ms(result.fetch_duration),
        ms(result.parse_ingest_duration)
    );
}

fn print_articles(items: &[feedlizard_storage::ArticleListItem]) {
    for item in items {
        println!(
            "{}\t{}\t{}\t{}",
            item.stable_id,
            if item.is_unread { "unread" } else { "read" },
            if item.is_starred { "starred" } else { "-" },
            item.title
        );
    }
}

fn benchmark(path: &Path, total: usize) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut library = Library::open(path)?;
    let open = started.elapsed();
    let batch = 1_000usize;
    let ingest_started = Instant::now();
    for offset in (0..total).step_by(batch) {
        let count = (total - offset).min(batch);
        let items=(offset..offset+count).map(|index|format!(r#"{{"id":"item-{index}","url":"https://benchmark.invalid/article/{index}","title":"Benchmark article {index}","summary":"Synthetic searchable café content {index}","content_text":"Offline benchmark body {index}","date_published":"2026-01-01T00:00:00Z"}}"#)).collect::<Vec<_>>().join(",");
        let document = format!(
            r#"{{"version":"https://jsonfeed.org/version/1.1","title":"Benchmark {}","items":[{items}]}}"#,
            offset / batch
        );
        library.ingest_document(
            &format!("https://benchmark.invalid/feed/{}", offset / batch),
            &document,
            1_800_000_000 + offset as i64,
        )?;
    }
    let ingest = ingest_started.elapsed();
    let library_started = Instant::now();
    let first_library = library.article_page(ArticleScope::Library, 50, None)?;
    let library_time = library_started.elapsed();
    let unread_started = Instant::now();
    let _ = library.article_page(ArticleScope::Unread, 50, None)?;
    let unread_time = unread_started.elapsed();
    let search_started = Instant::now();
    let search = library.search("searchable café", 50)?;
    let search_time = search_started.elapsed();
    let star_id = first_library.items.first().map(|v| v.stable_id.clone());
    if let Some(id) = star_id {
        library.set_article_starred(&id, true, now())?;
    }
    let starred_started = Instant::now();
    let _ = library.article_page(ArticleScope::Starred, 50, None)?;
    let starred_time = starred_started.elapsed();
    let read_started = Instant::now();
    let changed = library.mark_all_read(ArticleScope::Library, now())?;
    let read_time = read_started.elapsed();
    let retention_started = Instant::now();
    let deleted = library.cleanup_retention(1_900_000_000, 10_000)?;
    let retention_time = retention_started.elapsed();
    library.integrity_check()?;
    println!(
        "environment os={} arch={} rust={} sqlite=rusqlite-bundled",
        env::consts::OS,
        env::consts::ARCH,
        env!("CARGO_PKG_RUST_VERSION")
    );
    println!(
        "articles={total} open_ms={:.3} ingest_ms={:.3} library_50_ms={:.3} unread_50_ms={:.3} starred_50_ms={:.3} search_50_ms={:.3} search_hits={} mark_all_read_ms={:.3} marked={} retention_batch_ms={:.3} deleted={}",
        ms(open),
        ms(ingest),
        ms(library_time),
        ms(unread_time),
        ms(starred_time),
        ms(search_time),
        search.len(),
        ms(read_time),
        changed,
        ms(retention_time),
        deleted
    );
    Ok(())
}

fn ms(value: std::time::Duration) -> f64 {
    value.as_secs_f64() * 1_000.0
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn usage<T>() -> Result<T, Box<dyn std::error::Error>> {
    Err("usage: feedlizard-dev-cli <init|import-feed-fixture|list-feeds|list-unread|stats|search|mark-all-read|cleanup|opml-import|opml-export|benchmark|fetch|discover|add|refresh|refresh-all> ...".into())
}
