[package]
name = "ref-normalizer"
version = "0.2.0"
edition = "2021"
description = "Deterministic terminal file reference normalizer and vector-safe editor dispatcher"
license = "MIT"

[dependencies]
clap = { version = "4.5", features = ["derive", "env"] }
percent-encoding = "2.3"
regex = "1.11"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"