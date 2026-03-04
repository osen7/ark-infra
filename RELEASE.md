# Release Guide

## Versioning

- Workspace version is defined in root `Cargo.toml`.
- Keep crate versions aligned unless there is a strong reason to split.

## Pre-release Checklist

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `helm lint charts/ark-infra`
5. `helm template ark charts/ark-infra --namespace ark-system`

## Release Artifacts

- `ark` binary
- `ark-hub` binary
- Helm chart package: `charts/ark-infra`
- Rules bundle: `rules/*.yaml`

## Tagging

```bash
git tag v0.1.0
git push origin v0.1.0
```
