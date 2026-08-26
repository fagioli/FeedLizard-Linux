# Third-party software

FeedLizard Linux depends on open-source Rust crates and Fedora/Flatpak platform
libraries. The resolved Cargo graph was audited from package metadata and had
no missing license declarations. Its declared licenses are MPL-compatible
permissive terms including MIT, Apache-2.0, ISC, BSD, Unicode, Zlib, CC0, and
equivalent combinations.

Notable direct components include GTK4 and libadwaita (LGPL-2.1-or-later),
rusqlite and bundled SQLite (MIT/public domain), reqwest and Rustls
(MIT/Apache-2.0/ISC combinations), Servo html5ever through scraper
(MIT/Apache-2.0 and ISC), and the Rust `image` codecs (MIT/Apache-2.0 and
compatible codec licenses).

Optional encrypted subscription backup uses `nostr` and `nostr-sdk` and their
cryptographic dependencies under the MIT license, `oo7` under the MIT license
for Linux Secret Service/Secret portal integration, and `flate2` under
MIT OR Apache-2.0. These permissive terms are compatible with MPL 2.0.

Binary distributors must preserve the license and attribution files shipped by
the corresponding packages. A release audit should regenerate the resolved
dependency list from `Cargo.lock`; this summary is not a substitute for those
upstream license texts.
