---
applyTo: "**/Cargo.lock"
---

# Cargo.lock updates are always okay

It is fine to have "unrelated" `Cargo.lock` updates in a PR. Lockfile changes are never a problem.

In fact, it is strongly encouraged to do so on a regular basis - we want to keep `Cargo.lock` contents up to date and pointing at latest semver-compatible versions of packages so we test against the same versions our customers will see.