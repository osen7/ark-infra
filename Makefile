.PHONY: test lint fmt demo stress helm-lint helm-template

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

demo:
	./scripts/run-demo.sh

stress:
	cargo run -p ark-core --bin graph-stress -- --events 100000 --resources 8 --pids 1024

helm-lint:
	helm lint charts/ark-infra

helm-template:
	helm template ark charts/ark-infra --namespace ark-system >/tmp/ark-rendered.yaml
