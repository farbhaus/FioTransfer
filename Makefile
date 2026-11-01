# Build FioTransfer.app into the project folder (signed ad-hoc so macOS opens it).
# The app is the egui front end (`egui-ui`) on the real engine (`core/`); the
# root package is only the CLI and the bench.

APP := FioTransfer.app

.PHONY: app bundle dmg cli bench test clean

app: bundle

bundle:
	scripts/bundle.sh

# Universal .dmg + .zip in dist/ (same script CI uses). VERSION=1.2.3 make dmg
dmg:
	scripts/package.sh $(or $(VERSION),dev)

cli:
	cargo build --release --bin fio
	mkdir -p bin && cp target/release/fio bin/fio

bench:
	cargo build --release --bin fio-bench
	mkdir -p bin && cp target/release/fio-bench bin/fio-bench

test:
	FIO_NO_KEYCHAIN=1 cargo test --workspace

clean:
	rm -rf $(APP) target bin dist build/bin
