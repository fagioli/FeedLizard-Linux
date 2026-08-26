use feedlizard_core::parser::FeedFormat;
use feedlizard_nostr_backup::{RelayEvent, create_backup_event, generate_key, validated_history};
use feedlizard_storage::Library;

fn opml(feeds: &[(&str, &str)], folder: &str) -> String {
    let outlines = feeds
        .iter()
        .map(|(name, url)| format!(r#"<outline type="rss" text="{name}" xmlUrl="{url}"/>"#))
        .collect::<String>();
    format!(
        r#"<?xml version="1.0"?><opml version="2.0"><head><title>FeedLizard</title></head><body><outline text="{folder}">{outlines}</outline></body></opml>"#
    )
}

#[test]
fn older_snapshot_preview_cancel_and_merge_preserve_local_library() {
    let key = generate_key().unwrap();
    let older_opml = opml(
        &[
            ("Saved Name", "https://shared.example/feed"),
            ("Historical", "https://historical.example/feed"),
        ],
        "Archived News",
    );
    let newer_opml = opml(
        &[
            ("Saved Name", "https://shared.example/feed"),
            ("Newer", "https://newer.example/feed"),
        ],
        "Current News",
    );
    let older = create_backup_event(&key.nsec, &older_opml, 100).unwrap();
    let newer = create_backup_event(&key.nsec, &newer_opml, 200).unwrap();
    let history = validated_history(
        &key.nsec,
        [
            RelayEvent {
                relay: "relay-a".into(),
                event: older,
            },
            RelayEvent {
                relay: "relay-b".into(),
                event: newer,
            },
        ],
    )
    .unwrap();
    assert_eq!(history.len(), 2);

    let directory = tempfile::tempdir().unwrap();
    let mut library = Library::open(directory.path().join("library.sqlite3")).unwrap();
    library
        .add_subscription(
            "https://local-only.example/feed",
            "Local Only",
            FeedFormat::Rss,
            None,
            1,
        )
        .unwrap();
    library
        .add_subscription(
            "https://shared.example/feed",
            "Already Present",
            FeedFormat::Rss,
            None,
            1,
        )
        .unwrap();

    let selected = &history[1];
    assert_eq!(selected.feed_count, 2);
    assert_eq!(selected.folder_count, 1);

    // Preview and cancellation are read-only: no import has happened yet.
    assert_eq!(library.stats().unwrap().feeds, 2);
    assert!(
        library
            .list_feeds()
            .unwrap()
            .iter()
            .any(|feed| feed.fetch_url == "https://local-only.example/feed")
    );

    let imported = library.import_opml(&selected.opml, 300).unwrap();
    assert_eq!(imported.feeds_added, 1);
    assert_eq!(imported.duplicates, 1);
    let feeds = library.list_feeds().unwrap();
    assert_eq!(feeds.len(), 3);
    assert!(
        feeds
            .iter()
            .any(|feed| feed.fetch_url == "https://local-only.example/feed")
    );
    assert!(
        feeds
            .iter()
            .any(|feed| feed.fetch_url == "https://historical.example/feed")
    );
    assert_eq!(library.list_folders().unwrap()[0].name, "Archived News");
}

#[test]
fn snapshot_round_trip_preserves_folder_and_custom_name() {
    let source_dir = tempfile::tempdir().unwrap();
    let mut source = Library::open(source_dir.path().join("source.sqlite3")).unwrap();
    let feed_id = source
        .add_subscription(
            "https://portable.example/feed",
            "Publisher Name",
            FeedFormat::Rss,
            Some("https://portable.example/"),
            1,
        )
        .unwrap();
    source
        .set_feed_custom_name(&feed_id, Some("My Portable Name"), 2)
        .unwrap();
    let folder = source.create_folder("Portable Folder", None, 2).unwrap();
    source.move_feed(&feed_id, Some(folder.id), 2).unwrap();
    let exported = source.export_opml("synthetic fixture").unwrap();

    let key = generate_key().unwrap();
    let event = create_backup_event(&key.nsec, &exported, 500).unwrap();
    let snapshot = validated_history(
        &key.nsec,
        [RelayEvent {
            relay: "relay-test".into(),
            event,
        }],
    )
    .unwrap()
    .remove(0);

    let restored_dir = tempfile::tempdir().unwrap();
    let mut restored = Library::open(restored_dir.path().join("restored.sqlite3")).unwrap();
    restored.import_opml(&snapshot.opml, 600).unwrap();
    let feed = restored.list_feeds().unwrap().remove(0);
    assert_eq!(feed.custom_name.as_deref(), Some("My Portable Name"));
    assert_eq!(feed.display_name, "My Portable Name");
    assert_eq!(restored.list_folders().unwrap()[0].name, "Portable Folder");
}
