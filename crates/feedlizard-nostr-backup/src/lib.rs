use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use futures::future::join_all;
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io::{Read, Write},
    time::Duration,
};
use thiserror::Error;

pub const EVENT_KIND: u16 = 78;
pub const EVENT_IDENTIFIER: &str = "feedlizard-subscriptions-v1";
pub const DEFAULT_VISIBLE_SNAPSHOTS: usize = 5;
pub const MAX_HISTORY_CANDIDATES: usize = 100;
pub const MAX_CONFIGURED_RELAYS: usize = 8;
pub const MAX_RELAY_EVENTS: usize = MAX_HISTORY_CANDIDATES * MAX_CONFIGURED_RELAYS;
pub const MAX_OPML_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_COMPRESSED_BYTES: usize = 48 * 1024;
pub const MAX_ENVELOPE_BYTES: usize = 60 * 1024;
pub const MAX_EVENT_CONTENT_BYTES: usize = 128 * 1024;
const KEYRING_APPLICATION: &str = "io.github.feedlizard.FeedLizard";
const KEYRING_PURPOSE: &str = "nostr-backup";

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("invalid Nostr private key")]
    InvalidKey,
    #[error("secure secret storage is unavailable: {0}")]
    SecretStorage(String),
    #[error("no Nostr backup key is configured")]
    KeyMissing,
    #[error("backup data is invalid: {0}")]
    InvalidBackup(String),
    #[error("backup exceeds the supported size limit")]
    SizeLimit,
    #[error("Nostr operation failed: {0}")]
    Nostr(String),
    #[error("no valid FeedLizard backup was found")]
    NotFound,
    #[error("no relay accepted the backup")]
    PublishFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyIdentity {
    pub public_key_hex: String,
    pub npub: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedKey {
    pub identity: KeyIdentity,
    pub nsec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPreview {
    pub created_at: i64,
    pub opml: String,
    pub feed_count: usize,
    pub folder_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEvent {
    pub relay: String,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSnapshot {
    pub event_id: String,
    pub created_at: i64,
    pub opml: String,
    pub feed_count: usize,
    pub folder_count: usize,
    pub source_relays: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub event_id: String,
    pub successful_relays: usize,
    pub failed_relays: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub snapshots: Vec<BackupSnapshot>,
    pub has_older: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    format: String,
    created_at: i64,
    encoding: String,
    opml: String,
}

pub fn generate_key() -> Result<GeneratedKey, BackupError> {
    let keys = Keys::generate();
    let nsec = keys
        .secret_key()
        .to_bech32()
        .map_err(|_| BackupError::InvalidKey)?;
    Ok(GeneratedKey {
        identity: identity(&keys)?,
        nsec,
    })
}

pub fn validate_key(nsec: &str) -> Result<KeyIdentity, BackupError> {
    let keys = parse_keys(nsec)?;
    identity(&keys)
}

pub fn create_backup_event(nsec: &str, opml: &str, created_at: i64) -> Result<Event, BackupError> {
    let keys = parse_keys(nsec)?;
    let plaintext = encode_envelope(opml, created_at)?;
    let encrypted = nip44::encrypt(
        keys.secret_key(),
        &keys.public_key(),
        plaintext,
        nip44::Version::V2,
    )
    .map_err(|error| BackupError::Nostr(error.to_string()))?;
    EventBuilder::new(Kind::from_u16(EVENT_KIND), encrypted)
        .tag(Tag::identifier(EVENT_IDENTIFIER))
        .custom_created_at(Timestamp::from_secs(created_at.max(0) as u64))
        .finalize(&keys)
        .map_err(|error| BackupError::Nostr(error.to_string()))
}

pub fn decrypt_backup_event(nsec: &str, event: &Event) -> Result<BackupPreview, BackupError> {
    let keys = parse_keys(nsec)?;
    if event.content.len() > MAX_EVENT_CONTENT_BYTES {
        return Err(BackupError::SizeLimit);
    }
    event
        .verify()
        .map_err(|_| BackupError::InvalidBackup("event signature is invalid".into()))?;
    if event.pubkey != keys.public_key() {
        return Err(BackupError::InvalidBackup(
            "event author does not match the configured key".into(),
        ));
    }
    if event.kind != Kind::from_u16(EVENT_KIND)
        || event.tags.len() != 1
        || !event.tags.iter().any(|tag| {
            tag.as_slice().first().map(|value| value.as_str()) == Some("d")
                && tag.as_slice().get(1).map(|value| value.as_str()) == Some(EVENT_IDENTIFIER)
        })
    {
        return Err(BackupError::InvalidBackup(
            "event is not a FeedLizard subscription backup".into(),
        ));
    }
    let plaintext = nip44::decrypt(keys.secret_key(), &keys.public_key(), &event.content)
        .map_err(|_| BackupError::InvalidBackup("decryption failed".into()))?;
    let preview = decode_envelope(&plaintext)?;
    if preview.created_at < 0 || preview.created_at as u64 != event.created_at.as_secs() {
        return Err(BackupError::InvalidBackup(
            "envelope and event timestamps do not match".into(),
        ));
    }
    Ok(preview)
}

pub fn history_page(history: &[BackupSnapshot], offset: usize, limit: usize) -> HistoryPage {
    let limit = limit.clamp(1, 20);
    let snapshots = history
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    HistoryPage {
        has_older: offset.saturating_add(snapshots.len()) < history.len(),
        snapshots,
    }
}

pub fn validated_history(
    nsec: &str,
    relay_events: impl IntoIterator<Item = RelayEvent>,
) -> Result<Vec<BackupSnapshot>, BackupError> {
    let mut grouped: HashMap<String, (Event, BTreeSet<String>)> = HashMap::new();
    for candidate in relay_events.into_iter().take(MAX_RELAY_EVENTS) {
        let id = candidate.event.id.to_hex();
        let entry = grouped
            .entry(id)
            .or_insert_with(|| (candidate.event.clone(), BTreeSet::new()));
        entry.1.insert(candidate.relay);
    }
    let mut candidates = grouped.into_values().collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut history = candidates
        .into_iter()
        .take(MAX_HISTORY_CANDIDATES)
        .filter_map(|(event, relays)| {
            let preview = decrypt_backup_event(nsec, &event).ok()?;
            Some(BackupSnapshot {
                event_id: event.id.to_hex(),
                created_at: preview.created_at,
                opml: preview.opml,
                feed_count: preview.feed_count,
                folder_count: preview.folder_count,
                source_relays: relays.into_iter().collect(),
            })
        })
        .collect::<Vec<_>>();
    history.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    if history.is_empty() {
        Err(BackupError::NotFound)
    } else {
        Ok(history)
    }
}

pub struct SecureKeyStore {
    keyring: oo7::Keyring,
}

impl SecureKeyStore {
    pub async fn open() -> Result<Self, BackupError> {
        oo7::Keyring::new()
            .await
            .map(|keyring| Self { keyring })
            .map_err(|error| BackupError::SecretStorage(error.to_string()))
    }

    pub async fn store(&self, nsec: &str) -> Result<KeyIdentity, BackupError> {
        let identity = validate_key(nsec)?;
        self.keyring
            .create_item(
                "FeedLizard Nostr Backup Key",
                &keyring_attributes(),
                nsec,
                true,
            )
            .await
            .map_err(|error| BackupError::SecretStorage(error.to_string()))?;
        Ok(identity)
    }

    pub async fn load(&self) -> Result<String, BackupError> {
        let items = self
            .keyring
            .search_items(&keyring_attributes())
            .await
            .map_err(|error| BackupError::SecretStorage(error.to_string()))?;
        let item = items.first().ok_or(BackupError::KeyMissing)?;
        let secret = item
            .secret()
            .await
            .map_err(|error| BackupError::SecretStorage(error.to_string()))?;
        String::from_utf8(secret.as_bytes().to_vec())
            .map_err(|_| BackupError::SecretStorage("stored key is not valid text".into()))
    }

    pub async fn identity(&self) -> Result<Option<KeyIdentity>, BackupError> {
        match self.load().await {
            Ok(nsec) => validate_key(&nsec).map(Some),
            Err(BackupError::KeyMissing) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn remove(&self) -> Result<(), BackupError> {
        self.keyring
            .delete(&keyring_attributes())
            .await
            .map_err(|error| BackupError::SecretStorage(error.to_string()))
    }
}

pub struct RelayClient {
    relays: Vec<String>,
    timeout: Duration,
}

impl RelayClient {
    pub fn new(relays: Vec<String>) -> Result<Self, BackupError> {
        let mut unique = HashSet::new();
        let relays = relays
            .into_iter()
            .filter_map(|relay| {
                let relay = relay.trim().to_owned();
                (relay.starts_with("wss://") && unique.insert(relay.clone())).then_some(relay)
            })
            .take(MAX_CONFIGURED_RELAYS)
            .collect::<Vec<_>>();
        if relays.is_empty() {
            return Err(BackupError::Nostr(
                "at least one secure wss:// relay is required".into(),
            ));
        }
        Ok(Self {
            relays,
            timeout: Duration::from_secs(12),
        })
    }

    async fn connected(&self) -> Result<Client, BackupError> {
        let client = Client::new();
        for relay in &self.relays {
            client
                .add_relay(relay)
                .await
                .map_err(|error| BackupError::Nostr(error.to_string()))?;
        }
        client.connect().await;
        Ok(client)
    }

    pub async fn publish(&self, event: &Event) -> Result<PublishResult, BackupError> {
        let client = self.connected().await?;
        let output = tokio::time::timeout(self.timeout, client.send_event(event))
            .await
            .map_err(|_| BackupError::Nostr("relay publication timed out".into()))?
            .map_err(|error| BackupError::Nostr(error.to_string()))?;
        client.disconnect().await;
        if output.success.is_empty() {
            return Err(BackupError::PublishFailed);
        }
        Ok(PublishResult {
            event_id: output.id().to_hex(),
            successful_relays: output.success.len(),
            failed_relays: output.failed.len(),
        })
    }

    pub async fn fetch_history(&self, nsec: &str) -> Result<Vec<BackupSnapshot>, BackupError> {
        let keys = parse_keys(nsec)?;
        let filter = Filter::new()
            .author(keys.public_key())
            .kind(Kind::from_u16(EVENT_KIND))
            .identifier(EVENT_IDENTIFIER)
            .limit(MAX_HISTORY_CANDIDATES);
        let client = self.connected().await?;
        let results = join_all(self.relays.iter().map(|relay_url| {
            let client = &client;
            let filter = filter.clone();
            async move {
                let relay = client
                    .relay(relay_url)
                    .await
                    .map_err(|error| BackupError::Nostr(error.to_string()))?
                    .ok_or_else(|| BackupError::Nostr("configured relay is unavailable".into()))?;
                let events = relay
                    .fetch_events(filter)
                    .timeout(self.timeout)
                    .await
                    .map_err(|error| BackupError::Nostr(error.to_string()))?;
                Ok::<_, BackupError>((relay_url.clone(), events))
            }
        }))
        .await;
        client.disconnect().await;
        let relay_events =
            results
                .into_iter()
                .filter_map(Result::ok)
                .flat_map(|(relay, events)| {
                    events.into_iter().map(move |event| RelayEvent {
                        relay: relay.clone(),
                        event,
                    })
                });
        validated_history(nsec, relay_events)
    }
}

fn parse_keys(nsec: &str) -> Result<Keys, BackupError> {
    Keys::parse(nsec.trim()).map_err(|_| BackupError::InvalidKey)
}

fn identity(keys: &Keys) -> Result<KeyIdentity, BackupError> {
    Ok(KeyIdentity {
        public_key_hex: keys.public_key().to_hex(),
        npub: keys
            .public_key()
            .to_bech32()
            .map_err(|_| BackupError::InvalidKey)?,
    })
}

fn keyring_attributes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("application", KEYRING_APPLICATION),
        ("purpose", KEYRING_PURPOSE),
    ]
}

fn encode_envelope(opml: &str, created_at: i64) -> Result<String, BackupError> {
    if opml.len() > MAX_OPML_BYTES {
        return Err(BackupError::SizeLimit);
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(opml.as_bytes())
        .map_err(|error| BackupError::InvalidBackup(error.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|error| BackupError::InvalidBackup(error.to_string()))?;
    if compressed.len() > MAX_COMPRESSED_BYTES {
        return Err(BackupError::SizeLimit);
    }
    let encoded = serde_json::to_string(&Envelope {
        format: "feedlizard-subscriptions-v1".into(),
        created_at,
        encoding: "gzip+base64".into(),
        opml: STANDARD.encode(compressed),
    })
    .map_err(|error| BackupError::InvalidBackup(error.to_string()))?;
    if encoded.len() > MAX_ENVELOPE_BYTES {
        return Err(BackupError::SizeLimit);
    }
    Ok(encoded)
}

fn decode_envelope(input: &str) -> Result<BackupPreview, BackupError> {
    if input.len() > MAX_ENVELOPE_BYTES {
        return Err(BackupError::SizeLimit);
    }
    let envelope: Envelope = serde_json::from_str(input)
        .map_err(|error| BackupError::InvalidBackup(error.to_string()))?;
    if envelope.format != "feedlizard-subscriptions-v1" || envelope.encoding != "gzip+base64" {
        return Err(BackupError::InvalidBackup(
            "unsupported backup format".into(),
        ));
    }
    let compressed = STANDARD
        .decode(envelope.opml)
        .map_err(|_| BackupError::InvalidBackup("invalid base64 payload".into()))?;
    if compressed.len() > MAX_COMPRESSED_BYTES {
        return Err(BackupError::SizeLimit);
    }
    let decoder = GzDecoder::new(compressed.as_slice());
    let mut bytes = Vec::new();
    decoder
        .take((MAX_OPML_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| BackupError::InvalidBackup("invalid compressed payload".into()))?;
    if bytes.len() > MAX_OPML_BYTES {
        return Err(BackupError::SizeLimit);
    }
    let opml = String::from_utf8(bytes)
        .map_err(|_| BackupError::InvalidBackup("OPML is not UTF-8".into()))?;
    let library = feedlizard_core::opml::import(&opml)
        .map_err(|error| BackupError::InvalidBackup(format!("invalid OPML: {error}")))?;
    let mut folder_paths = HashSet::new();
    for feed in &library.feeds {
        for depth in 1..=feed.folders.len() {
            folder_paths.insert(feed.folders[..depth].to_vec());
        }
    }
    Ok(BackupPreview {
        created_at: envelope.created_at,
        opml,
        feed_count: library.feeds.len(),
        folder_count: folder_paths.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    const OPML: &str = r#"<?xml version="1.0"?><opml version="2.0"><head><title>FeedLizard</title></head><body><outline text="News"><outline type="rss" text="Example" xmlUrl="https://example.com/feed"/></outline></body></opml>"#;

    fn opml(name: &str, url: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><opml version="2.0"><head><title>FeedLizard</title></head><body><outline text="News"><outline type="rss" text="{name}" xmlUrl="{url}"/></outline></body></opml>"#
        )
    }

    fn relay(relay: &str, event: Event) -> RelayEvent {
        RelayEvent {
            relay: relay.into(),
            event,
        }
    }

    fn encrypted_event(keys: &Keys, plaintext: &str, created_at: i64) -> Event {
        let encrypted = nip44::encrypt(
            keys.secret_key(),
            &keys.public_key(),
            plaintext,
            nip44::Version::V2,
        )
        .unwrap();
        EventBuilder::new(Kind::from_u16(EVENT_KIND), encrypted)
            .tag(Tag::identifier(EVENT_IDENTIFIER))
            .custom_created_at(Timestamp::from_secs(created_at as u64))
            .finalize(keys)
            .unwrap()
    }

    #[test]
    fn generated_and_existing_keys_are_interoperable() {
        let generated = generate_key().unwrap();
        assert!(generated.nsec.starts_with("nsec1"));
        assert_eq!(validate_key(&generated.nsec).unwrap(), generated.identity);
    }

    #[test]
    fn encrypted_backup_round_trips() {
        let key = generate_key().unwrap();
        let event = create_backup_event(&key.nsec, OPML, 1_777_000_000).unwrap();
        let preview = decrypt_backup_event(&key.nsec, &event).unwrap();
        assert_eq!(preview.opml, OPML);
        assert_eq!(preview.created_at, 1_777_000_000);
        assert_eq!(preview.feed_count, 1);
        assert_eq!(preview.folder_count, 1);
        assert_eq!(event.kind.as_u16(), EVENT_KIND);
        assert_eq!(event.tags.len(), 1);
        assert!(!event.content.contains("example.com"));
        assert!(!event.as_json().contains("Example"));
    }

    #[test]
    fn wrong_key_and_corruption_are_rejected() {
        let key = generate_key().unwrap();
        let other = generate_key().unwrap();
        let event = create_backup_event(&key.nsec, OPML, 5).unwrap();
        assert!(decrypt_backup_event(&other.nsec, &event).is_err());
        let mut value = serde_json::to_value(&event).unwrap();
        value["content"] = serde_json::Value::String("corrupt".into());
        let corrupt: Event = serde_json::from_value(value).unwrap();
        assert!(decrypt_backup_event(&key.nsec, &corrupt).is_err());
    }

    #[test]
    fn every_backup_is_a_distinct_discoverable_snapshot() {
        let key = generate_key().unwrap();
        let old =
            create_backup_event(&key.nsec, &opml("Old", "https://old.test/feed"), 10).unwrap();
        let current =
            create_backup_event(&key.nsec, &opml("Current", "https://current.test/feed"), 20)
                .unwrap();
        assert_ne!(old.id, current.id);
        let history = validated_history(
            &key.nsec,
            [relay("relay-a", old), relay("relay-a", current)],
        )
        .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].created_at, 20);
        assert_eq!(history[1].created_at, 10);
        assert!(history[1].opml.contains("old.test"));
    }

    #[test]
    fn history_pages_newest_five_and_loads_older() {
        let key = generate_key().unwrap();
        let events = (0..8)
            .map(|index| {
                relay(
                    "relay-a",
                    create_backup_event(
                        &key.nsec,
                        &opml(
                            &format!("Feed {index}"),
                            &format!("https://{index}.test/feed"),
                        ),
                        100 + index,
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let history = validated_history(&key.nsec, events).unwrap();
        assert_eq!(history.len(), 8);
        let newest = history_page(&history, 0, DEFAULT_VISIBLE_SNAPSHOTS);
        assert_eq!(newest.snapshots.len(), 5);
        assert!(newest.has_older);
        assert_eq!(newest.snapshots[0].created_at, 107);
        let older = history_page(&history, 5, DEFAULT_VISIBLE_SNAPSHOTS);
        assert_eq!(older.snapshots.len(), 3);
        assert!(!older.has_older);
        assert_eq!(older.snapshots[2].created_at, 100);
    }

    #[test]
    fn equal_timestamps_order_by_event_id() {
        let key = generate_key().unwrap();
        let first = create_backup_event(&key.nsec, OPML, 50).unwrap();
        let second = create_backup_event(&key.nsec, OPML, 50).unwrap();
        assert_ne!(first.id, second.id);
        let history =
            validated_history(&key.nsec, [relay("a", first), relay("b", second)]).unwrap();
        assert!(history[0].event_id < history[1].event_id);
    }

    #[test]
    fn relay_histories_aggregate_and_deduplicate() {
        let key = generate_key().unwrap();
        let a = create_backup_event(&key.nsec, OPML, 1).unwrap();
        let b = create_backup_event(&key.nsec, OPML, 2).unwrap();
        let c = create_backup_event(&key.nsec, OPML, 3).unwrap();
        let history = validated_history(
            &key.nsec,
            [
                relay("relay-a", a.clone()),
                relay("relay-a", b.clone()),
                relay("relay-b", a),
                relay("relay-b", b),
                relay("relay-c", c),
            ],
        )
        .unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].created_at, 3);
        assert_eq!(history[0].source_relays, ["relay-c"]);
        assert_eq!(history[1].source_relays, ["relay-a", "relay-b"]);
    }

    #[test]
    fn malformed_wrong_author_unsupported_and_oversized_events_are_rejected() {
        let key = generate_key().unwrap();
        let keys = parse_keys(&key.nsec).unwrap();
        let unsupported = encrypted_event(
            &keys,
            r#"{"format":"feedlizard-subscriptions-v2","created_at":7,"encoding":"gzip+base64","opml":""}"#,
            7,
        );
        assert!(decrypt_backup_event(&key.nsec, &unsupported).is_err());

        let malformed = encrypted_event(&keys, &encode_envelope("not OPML", 8).unwrap(), 8);
        assert!(decrypt_backup_event(&key.nsec, &malformed).is_err());

        let other = generate_key().unwrap();
        let wrong_author = create_backup_event(&other.nsec, OPML, 9).unwrap();
        assert!(decrypt_backup_event(&key.nsec, &wrong_author).is_err());

        let valid = create_backup_event(&key.nsec, OPML, 9).unwrap();
        let unexpected_public_tag = EventBuilder::new(Kind::from_u16(EVENT_KIND), valid.content)
            .tag(Tag::identifier(EVENT_IDENTIFIER))
            .tag(Tag::parse(["client", "unexpected"]).unwrap())
            .custom_created_at(Timestamp::from_secs(9))
            .finalize(&keys)
            .unwrap();
        assert!(decrypt_backup_event(&key.nsec, &unexpected_public_tag).is_err());

        let oversized = EventBuilder::new(
            Kind::from_u16(EVENT_KIND),
            "x".repeat(MAX_EVENT_CONTENT_BYTES + 1),
        )
        .tag(Tag::identifier(EVENT_IDENTIFIER))
        .custom_created_at(Timestamp::from_secs(10))
        .finalize(&keys)
        .unwrap();
        assert!(matches!(
            decrypt_backup_event(&key.nsec, &oversized),
            Err(BackupError::SizeLimit)
        ));
    }

    #[derive(Deserialize)]
    struct HistoryFixture {
        protocol: String,
        event_kind: u16,
        grouping_tag: Vec<String>,
        default_visible: usize,
        same_key_for_all_snapshots: bool,
        equal_timestamp_order: String,
        snapshots: Vec<FixtureSnapshot>,
    }

    #[derive(Deserialize)]
    struct FixtureSnapshot {
        name: String,
        created_at: i64,
        feed_url: String,
        relays: Vec<String>,
    }

    #[test]
    fn cross_platform_history_fixture_matches_protocol() {
        let fixture: HistoryFixture = serde_json::from_str(include_str!(
            "../../../fixtures/compatibility/nostr-history.json"
        ))
        .unwrap();
        assert_eq!(fixture.protocol, EVENT_IDENTIFIER);
        assert_eq!(fixture.event_kind, EVENT_KIND);
        assert_eq!(fixture.grouping_tag, ["d", EVENT_IDENTIFIER]);
        assert_eq!(fixture.default_visible, DEFAULT_VISIBLE_SNAPSHOTS);
        assert!(fixture.same_key_for_all_snapshots);
        assert_eq!(fixture.equal_timestamp_order, "event_id_ascending");

        let key = generate_key().unwrap();
        let mut relay_events = Vec::new();
        let mut unique_ids = HashSet::new();
        for snapshot in &fixture.snapshots {
            let event = create_backup_event(
                &key.nsec,
                &opml(&snapshot.name, &snapshot.feed_url),
                snapshot.created_at,
            )
            .unwrap();
            assert!(unique_ids.insert(event.id));
            for relay_name in &snapshot.relays {
                relay_events.push(relay(relay_name, event.clone()));
            }
        }
        let history = validated_history(&key.nsec, relay_events).unwrap();
        assert_eq!(history.len(), fixture.snapshots.len());
        assert_eq!(history[0].created_at, 1_777_000_500);
        assert_eq!(history[0].source_relays.len(), 3);
        let newest = history_page(&history, 0, fixture.default_visible);
        assert_eq!(newest.snapshots.len(), 5);
        assert!(newest.has_older);
    }

    #[test]
    fn decompressed_size_is_bounded() {
        let oversized = "x".repeat(MAX_OPML_BYTES + 1);
        assert!(matches!(
            encode_envelope(&oversized, 1),
            Err(BackupError::SizeLimit)
        ));
    }

    #[tokio::test]
    #[ignore = "requires an isolated desktop Secret Service session"]
    async fn secret_service_round_trip() {
        let store = SecureKeyStore::open().await.unwrap();
        let generated = generate_key().unwrap();
        store.store(&generated.nsec).await.unwrap();
        assert_eq!(store.identity().await.unwrap(), Some(generated.identity));
        store.remove().await.unwrap();
        assert_eq!(store.identity().await.unwrap(), None);
    }
}
