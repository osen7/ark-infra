# Contributing

## Development Flow

1. Fork and create a feature branch.
2. Keep changes small and focused.
3. Add tests for every behavior change.
4. Update docs for user-visible changes.
5. Open PR with problem statement and validation steps.

## Quality Gates

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `helm lint charts/ark-infra`

## Rule/Policy Changes

- Any rule change must include at least one reproducible mock case.
- Automated remediation logic must support `dry-run` first.

## Commit Guidance

- Prefer one logical change per commit.
- Do not include unrelated formatting-only churn.
