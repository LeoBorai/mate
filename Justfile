set dotenv-load
set positional-arguments

# List Tasks
default:
    just --list

# Runs the Development-Kit Container
dkc:
    docker pull ghcr.io/leoborai/dkc:latest
    docker run -it --rm \
        -v $(pwd):/app \
        -w /app \
        ghcr.io/leoborai/dkc:latest

# Perform formatting and linting
fmt:
    cargo clippy --fix --workspace --allow-dirty --allow-staged --all-targets && cargo fmt

# Runs tests
test:
    cargo nextest run

# Checks licences and security advisories
deny:
    cargo deny check
