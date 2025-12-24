default:
    @echo "No default task defined."
    just --list

build-example-simple:
    cd ./examples/simple && cargo b --release --target wasm32-unknown-unknown
    cp ./target/wasm32-unknown-unknown/release/simple.wasm ./simple.wasm

build-example-complex:
    cd ./examples/complex && cargo b --release --target wasm32-unknown-unknown
    cp ./target/wasm32-unknown-unknown/release/complex.wasm ./complex.wasm

build-example-http:
    cd ./examples/http && cargo b --release --target wasm32-unknown-unknown
    cp ./target/wasm32-unknown-unknown/release/http.wasm ./http.wasm
