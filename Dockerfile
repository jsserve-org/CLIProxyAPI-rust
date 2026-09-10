FROM rust:1.93-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --locked --release

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /build/target/release/cliproxyapi-rs /usr/local/bin/cliproxyapi-rs
EXPOSE 8317 8318
ENTRYPOINT ["/usr/local/bin/cliproxyapi-rs"]
CMD ["--config", "/etc/cliproxyapi/config.yaml"]

