fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path = "../../vendor/opensnitch/proto/ui.proto";
    let proto_dir = "../../vendor/opensnitch/proto";

    println!("cargo:rerun-if-changed={}", proto_path);

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_path], &[proto_dir])?;

    Ok(())
}
