build:
  cargo build

# Test everything
test:
  cargo nextest run

format:
  cargo fmt --all
  find . -type f -iname "*.toml" -print0 | xargs -0 taplo format

lint:
  cargo clippy --all --all-features -- -D warnings

lintfix:
  cargo clippy --fix --allow-staged --allow-dirty --all-features
  just format

check:
  cargo check --all-features

refresh:
  cargo clean && cargo update