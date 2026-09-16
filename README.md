# TidyGrid

TidyGrid is a small, machine-readable CLI for backing up and rearranging an
iPhone Home Screen from macOS. It talks directly to SpringBoard over USB: no
screen mirroring, dragging, GUI, account, or cloud service.

> **Compatibility:** Apple Silicon macOS and a USB-connected, unlocked, trusted
> iPhone. TidyGrid uses an undocumented iOS service, so test a snapshot after
> major iOS updates before applying a layout.

## Install

```bash
brew install goldmar/tap/tidygrid
```

## Use

Save the phone's current layout:

```bash
tidygrid devices
tidygrid snapshot --output current.snapshot.json
```

Copy the snapshot, edit only its `state` value, then validate and review it:

```bash
cp current.snapshot.json plan.layout.json
tidygrid validate plan.layout.json --inventory current.snapshot.json
tidygrid diff current.snapshot.json plan.layout.json
tidygrid check --baseline current.snapshot.json --plan plan.layout.json
```

Apply only after `check` succeeds:

```bash
tidygrid apply --baseline current.snapshot.json --plan plan.layout.json
```

Specify `--device UDID` on phone commands when more than one iPhone is plugged
in. Every command prints JSON. An invalid validation or check exits with status
2; connection, file, and write failures exit with status 1.

## Write safety

`apply` refuses to write unless all of these are true:

- the phone still exactly matches the supplied baseline snapshot;
- the plan contains every current icon exactly once;
- pages, folders, and the dock fit the grid reported by that phone.

Before a write, the current layout is saved under `~/.tidygrid/backups/`. After
the write, TidyGrid reads the layout back. A mismatch triggers an automatic
attempt to restore and verify the baseline. `restore BACKUP` also saves the
layout it is replacing, so a mistaken restore remains recoverable.

## What it can and cannot do

TidyGrid can read and rewrite pages, folders, app order, and the
dock. It does not install or delete apps, expose Screen Time usage, preserve
widgets, or represent arbitrary gaps between icons. iOS may normalize a layout
that uses unsupported structures; that is why every write is read back.

## Develop

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

The repository deliberately contains only the Rust CLI and release plumbing.
No personal layout snapshots are included.

## Origin and license

TidyGrid is a CLI-only derivative of
[IconState](https://github.com/Jubstaaa/iconstate) by İlker Balcılar. The
original project and this derivative are licensed under the MIT License. See
[LICENSE](LICENSE) and [NOTICE](NOTICE).
