default:
    @echo "No default task defined."
    just --list

build-example-http:
    rm ./http.wasm || true
    cd ./examples/http && cargo +nightly build --release --target wasm32-wasip2
    mv ./target/wasm32-wasip2/release/http.wasm ./http.wasm
