default:
    @echo "No default task defined."
    just --list

build-example-http:
    cd ./examples/http && cargo b --release --target wasm32-wasip2
    cp ./target/wasm32-wasip2/release/http.wasm ./http.wasm
