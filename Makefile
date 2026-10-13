.PHONY: build release install run migrate test lint fmt bench clean

build:
	cargo build

release:
	cargo build --release -p oddsmith-cli --features parquet

install:
	cargo install --locked --path crates/oddsmith-cli --features parquet

run:
	cargo run -q -p oddsmith-cli -- $(ARGS)

migrate:
	cargo run -q -p oddsmith-cli -- migrate --dir ./migrations

test:
	cargo test

lint:
	cargo fmt --all --check
	cargo clippy --all-targets --features oddsmith-cli/parquet -- -D warnings

fmt:
	cargo fmt --all

bench:
	cargo run --release -p oddsmith-core --example bench

clean:
	cargo clean
	rm -rf output/

# Example usage:
# make release
# make migrate
# make run ARGS="ingest markets --type presidential"
# make run ARGS="backtest --min-price 0.50"
# make run ARGS="analyze all --output-dir ./output"
