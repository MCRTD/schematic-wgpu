#!/usr/bin/env sh
# Build the browser viewer into crates/web/www. Serve that folder over
# http(s), e.g. `python3 -m http.server -d crates/web/www 8000`.
#
# Needs: rustup target add wasm32-unknown-unknown
#        cargo install wasm-bindgen-cli --version <same as Cargo.lock>
set -eu
cd "$(dirname "$0")/.."
profile="${PROFILE:-release}"
cargo build -p web --target wasm32-unknown-unknown --profile "$profile"
dir="$profile"
[ "$profile" = dev ] && dir=debug
bindgen="$(command -v wasm-bindgen || echo "${CARGO_HOME:-$HOME/.cargo}/bin/wasm-bindgen")"
"$bindgen" --target web --no-typescript --out-dir crates/web/www/pkg \
  "target/wasm32-unknown-unknown/$dir/web.wasm"
echo "built crates/web/www (serve it and open index.html)"
