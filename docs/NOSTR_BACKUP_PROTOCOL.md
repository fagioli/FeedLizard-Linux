# FeedLizard encrypted subscription snapshot protocol v1

This protocol is platform-independent. Linux Secret Service and Apple Keychain
are local storage details and do not alter events, discovery, or plaintext.

## Product semantics

Each explicit **Back Up Now** creates a complete, independent encrypted OPML
snapshot. It is not a delta and does not replace, update, or delete an older
snapshot. There is no automatic backup, synchronization, pruning, recovery
service, or background Nostr activity.

## Identity and event model

- The identity is an ordinary secp256k1 Nostr key pair. Import and recovery use
  its standard Bech32 `nsec`; there is no FeedLizard-specific key format.
- Each snapshot is a NIP-78 normal application-data event of kind `78`.
- Every event has exactly one public grouping tag:
  `["d", "feedlizard-subscriptions-v1"]`.
- Kind 78 is used because NIP-78 defines it for multiple events of the same
  application type. Kind 30078 is addressable and is deliberately not used:
  relays may discard older events sharing an address, which conflicts with
  historical recovery points.
- The standard signed Nostr event ID is the opaque snapshot identifier. NIP-44
  uses a fresh random nonce, so separately created snapshots remain distinct
  even when OPML and timestamps happen to match.
- The author is the configured key's public key. The event is signed by that
  key and its ID and signature are verified before decryption.

No feed count, folder count, feed URL, site URL, name, or folder name is placed
in a public tag or event field.

## Encryption and envelope

The content is NIP-44 version 2 encrypted to the author's own public key. The
private key is therefore both the signing credential and recovery credential.
Before encryption, FeedLizard serializes this UTF-8 JSON object:

```json
{
  "format": "feedlizard-subscriptions-v1",
  "created_at": 1777000000,
  "encoding": "gzip+base64",
  "opml": "..."
}
```

`created_at` is a non-negative Unix UTC timestamp and MUST equal the signed
Nostr event `created_at`. The decrypted envelope timestamp is authoritative for
ordering and user display after that equality check succeeds. Implementations
MUST reject a mismatch rather than silently choosing one timestamp.

`opml` is ordinary FeedLizard-compatible OPML 2.0, compressed with gzip and
then Base64. It contains subscriptions, folder paths, site URLs, feed formats,
and custom names where representable. It does not contain articles, read state,
stars, images, Reader cache, or unrelated settings. Feed and folder counts are
computed only after decrypting and validating this OPML.

Protocol v1 limits event content to 128 KiB, the decrypted JSON envelope to
60 KiB, compressed OPML to 48 KiB, and decompressed OPML to 4 MiB. Limits are
enforced during decoding and decompression, before unbounded allocation.

## Discovery and validation

A clean installation needs the same `nsec` and at least one reachable selected
or default relay. For each relay, clients query:

- author: the public key derived from the configured `nsec`;
- kind: `78`;
- `#d`: `feedlizard-subscriptions-v1`;
- newest first, with a bounded result limit.

FeedLizard currently accepts at most eight configured relays and requests at
most 100 events from each. It considers at most 100 unique candidate event IDs
per history load after aggregation. Older-history network pagination may
extend that bound in a future protocol-compatible client, but no implementation
may issue an unbounded query or decrypt an unbounded number of candidates.

Every candidate independently passes all of these checks before appearing:

1. bounded event content;
2. recomputed event ID and valid Schnorr signature;
3. expected author;
4. kind 78 and exact FeedLizard grouping tag;
5. NIP-44 v2 authenticated decryption;
6. supported envelope format and encoding;
7. envelope/event timestamp equality;
8. Base64, compressed, and decompressed size limits;
9. valid bounded OPML.

Malformed, forged, wrong-author, wrong-key, unsupported-version, corrupt, or
oversized events are ignored and cause no SQLite mutation.

## Aggregation, deduplication, and ordering

Relay histories are a union, not a consensus operation. A valid snapshot found
on one relay remains usable. The same signed event returned by multiple relays
is shown once, deduplicated by its validated event ID. Implementations may keep
the supplying relay set for diagnostics, but normal UI does not expose it.

Validated snapshots sort by decrypted `created_at` descending. Equal timestamps
sort by lowercase event ID ascending. This deterministic secondary ordering is
part of protocol v1. The first result is labelled **Latest**.

The normal UI initially presents the five newest valid snapshots. If more of
the bounded result set is available it offers **Show Older Backups**. Five is a
presentation policy, not a remote retention or deletion policy. FeedLizard
sends no automatic deletion or pruning requests.

## Restore

Selecting a history row opens a preview; selection alone never mutates SQLite.
The preview derives backup time, feed count, folder count, feeds to add, and
already-present feeds from validated decrypted data and the local library.
Only **Restore This Backup** performs the existing transactional OPML merge.
Merge prevents duplicate subscriptions, restores supported folders and names,
and preserves local-only subscriptions. Cancel performs zero mutations. There
is no destructive Replace operation.

## Privacy and key custody

Historical snapshots expose more metadata than one addressable event. Relay
operators may observe the public key, kind, grouping tag, publication times and
frequency, approximate ciphertext sizes, selected relays, IP addresses, and
connection timing. Encryption does not hide that metadata.

Relays do not learn OPML, subscription or website URLs, feed or custom names,
folder names, feed/folder counts, or article information. Private keys are
never placed in events, OPML, SQLite, settings, logs, or diagnostics. Relays
may retain, omit, replay, censor, or delete snapshots; FeedLizard makes no
permanence or deletion guarantee. A lost private key cannot be recovered by
FeedLizard.
