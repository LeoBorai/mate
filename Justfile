default:
    @echo "No default task defined."
    just --list

# Builds the example job "http"
build-job-http:
    rm ./http.wasm || true
    cd ./jobs/http && cargo +nightly build --release --target wasm32-wasip2
    mv ./target/wasm32-wasip2/release/http.wasm ./http.wasm
