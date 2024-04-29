use prost_build::Config;
use std::env;
use std::io::Result;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut config = Config::new();
    config.file_descriptor_set_path(
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable not set"))
            .join("file_descriptor_set_config.bin"),
    );
    config.protoc_arg("--experimental_allow_proto3_optional");
    config.compile_protos(&["src/proto/config.proto"], &["src/"])?;
    // We don't need the file descriptor set for the bep, if we would we would need to create a new
    // prost_build::Config as the file descriptor set would be overwritten.
    prost_build::compile_protos(&["src/proto/bep.proto"], &["src/"])?;
    Ok(())
}
