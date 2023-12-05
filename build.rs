use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(&["src/proto/bep.proto"], &["src/"])?;
    prost_build::compile_protos(&["src/proto/config.proto"], &["src/"])?;
    Ok(())
}
