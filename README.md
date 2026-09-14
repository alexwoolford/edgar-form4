# edgar-form4

Nightly **Form 4 / 4/A open-market purchases** (transaction code **P**) from EDGAR. A **label**, not a leading observable: code P *is* the purchase disclosure. This crate does not emit a cluster score and does not parse footnotes.

Production is a systemd oneshot on Linux (`deploy/install.sh`). Capture contract: [docs/CAPTURE.md](docs/CAPTURE.md). Ops: [docs/DAILY_OPS.md](docs/DAILY_OPS.md).

```bash
export SEC_USER_AGENT='edgar-form4 you@real-domain'
cargo run --release -- ingest --date 2026-09-11
cargo run --release -- status
cargo run --release -- lookup AAPL
```

`cargo test` uses fixtures only. It does not need the network or a real User-Agent.

Pin `capturable-state` git tag `v0.1.1`. Never `path = "../capturable-state"`.
