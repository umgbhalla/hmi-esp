# Vendor reference workspace

`upstream/` contains shallow Git checkouts of the requested references. It is
a local research cache, not copied product source, and is ignored by the
top-level repository. `sources.lock` records the exact upstream URL, commit,
directory, and observed license for every checkout.

## Reproduce and verify

```sh
./scripts/vendor-sync.sh
./scripts/vendor-check.sh
```

`vendor-sync.sh` refuses to replace a dirty checkout. `vendor-check.sh` is
read-only and fails if a source is missing, dirty, points at another remote,
or has moved away from its pinned commit.

## Cargo patch layer

The root `Cargo.toml` uses `[patch.crates-io]` to route the Rust references to
these exact local sources:

- `mousefood`
- `embedded-graphics-simulator`
- `esp-idf-hal`
- `esp-idf-svc`
- `esp-hal` for separate bare-metal evaluation only

This is a development override, not a claim that all five crates belong in
one firmware image. The recommended first firmware uses `esp-idf-hal` and
`esp-idf-svc`; `esp-hal` represents the alternative bare-metal runtime.

## Source patch policy

Do not edit upstream checkouts as the normal integration mechanism. Put any
unavoidable, reviewable diffs under `patches/<directory>/NNNN-description.patch`
and list them in `patches/series`. Prefer implementing board and protocol code
in the product workspace so upstream updates remain auditable.

No source patch is currently applied. The active vendor patch is the Cargo
path override in the root manifest; the references remain byte-for-byte clean
at their pinned commits.

## License boundary

The Doom and Game Boy references are GPLv2. Study and test against them, but
do not copy their implementation into a differently licensed product without
accepting the GPL obligations. Two repositories declare MIT terms in their
README but do not contain a root license file at the pinned revision; treat
those as reference-only until provenance is clarified.

