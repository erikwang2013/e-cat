fn main() {
    tonic_build::configure()
        .compile_protos(
            &["proto/errors.proto", "proto/metadata.proto"],
            &["proto"],
        )
        .unwrap();
}
