.PHONY: record verify study test fmt
record:
	cargo run --release -p feed --bin record -- --out data
verify:
	cargo run --release -p feed --bin verify_book -- --dir data
study:
	cargo run --release -p study -- --data data/days --out artifacts
test:
	cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --all
fmt:
	cargo fmt
