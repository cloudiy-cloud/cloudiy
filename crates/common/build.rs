fn main() {
    let proto_file = "../../proto/gpuasas.proto";

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/generated")
        .compile_protos(&[proto_file], &["../../proto"])
        .unwrap_or_else(|e| panic!("Failed to compile protos: {}", e));

    println!("cargo:rerun-if-changed={}", proto_file);
}