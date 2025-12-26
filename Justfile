default:
    @echo "No default task defined."
    just --list

build-example-http:
    rm ./http.wasm || true
    cd ./examples/http && cargo b --release --target wasm32-wasip2
    cp ./target/wasm32-wasip2/release/http.wasm ./http.wasm

build-example-simple:
    rm ./simple.wasm || true
    cd ./examples/simple && cargo b --release --target wasm32-wasip2
    cp ./target/wasm32-wasip2/release/simple.wasm ./simple.wasm
