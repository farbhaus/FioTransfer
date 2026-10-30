# Build FioTransfer.app into the project folder (signed ad-hoc so macOS opens it).
# The app is the egui front end (`egui-ui`) on the real engine (`core/`); the
# root package is only the CLI and the bench.
# The same targets the Go Makefile has: app, cli, test, clean.

APP := FioTransfer.app

.PHONY: app bundle cli bench test clean

app: bundle

bundle:
	scripts/bundle.sh

cli:
	cargo build --release --bin fio
	mkdir -p bin && cp target/release/fio bin/fio

bench:
	cargo build --release --bin fio-bench
	mkdir -p bin && cp target/release/fio-bench bin/fio-bench

test:
	cargo test --workspace

clean:
	rm -rf $(APP) target bin
