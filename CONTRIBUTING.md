# Contributing to FeedLizard Linux

Thank you for helping make a thoughtful native RSS reader for Linux.

Before submitting a change, open an issue for work that changes identity,
storage, privacy, security, or major product behavior. Small bug fixes,
documentation improvements, tests, and translations can be proposed directly.

Run `cargo fmt --check`, `cargo test --workspace`, and
`cargo clippy --workspace --all-targets -- -D warnings` from this directory.
New parser behavior needs deterministic offline fixtures. UI work should be
checked at wide and narrow sizes, in light and dark appearance, with keyboard
navigation and accessible labels.

Contributions are accepted under MPL 2.0. A Contributor License Agreement is
not required. By adding `Signed-off-by: Name <email>` to commits, contributors
certify the Developer Certificate of Origin 1.1. Community translations should
be submitted through the same review process and remain source-controlled.

Do not include FeedLizard logos or other protected branding in unofficial build
artifacts. Do not configure the official Lightning address in forks unless the
FeedLizard project has authorized that build.
