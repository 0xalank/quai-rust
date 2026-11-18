use prost::Message;
use heck::ToUpperCamelCase;
use prost_types::{DescriptorProto, FileDescriptorSet};
use std::{env, fs, path::PathBuf};
fn collect(prefix: &str, values: &[DescriptorProto], out: &mut Vec<(String, DescriptorProto)>) {
    for value in values {
        let name = format!("{prefix}.{}", value.name.as_ref().unwrap());
        out.push((name.clone(), value.clone()));
        collect(&name, &value.nested_type, out);
    }
}
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    assert_eq!(args.len(), 2, "GO_SOURCE OUTPUT_DIRECTORY");
    let source = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    let paths = ["common/proto_common.proto", "core/types/proto_block.proto", "core/rawdb/db.proto", "p2p/pb/quai_messages.proto", "p2p/node/peerManager/peerdb/peer_info.proto"];
    let descriptor = output.join("schema.pb");
    let mut config = prost_build::Config::new();
    config.out_dir(&output).btree_map(["."]).file_descriptor_set_path(&descriptor);
    config.compile_protos(&paths.map(|p|source.join(p)), &[source]).unwrap();
    let descriptor = FileDescriptorSet::decode(fs::read(&descriptor).unwrap().as_slice()).unwrap();
    let mut messages = Vec::new();
    for file in descriptor.file { collect(&format!(".{}", file.package.unwrap()), &file.message_type, &mut messages); }
    let mut generated = String::from("// Schema metadata for bounded preflight before allocating message trees.\nuse prost::Message;\npub const FIELDS: &[&[(u32, u8, Option<usize>)]] = &[\n");
    for (_, value) in &messages {
        generated.push_str("    &[");
        for field in &value.field {
            let typ = field.r#type.unwrap();
            let wire = match typ { 9|11|12 => 2, 4|8|13 => 0, _ => panic!("Unsupported schema field type {typ}") };
            let child = if typ == 11 { format!("Some({})",messages.iter().position(|v|Some(&v.0)==field.type_name.as_ref()).unwrap()) } else { "None".to_string() };
            generated.push_str(&format!("({}, {wire}, {child}),",field.number.unwrap()));
        }
        generated.push_str("],\n");
    }
    generated.push_str("];\npub const NAMES: &[&str] = &[\n");
    for (name, _) in &messages { generated.push_str(&format!("    {name:?},\n")); }
    generated.push_str("];\npub fn canonicalize(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {\n    super::preflight(name, bytes)?;\n    Ok(match name {\n");
    for (name, value) in &messages {
        if value.options.as_ref().and_then(|o|o.map_entry).unwrap_or(false) { continue; }
        let pieces: Vec<_> = name.trim_start_matches('.').split('.').collect();
        assert_eq!(pieces.len(), 2, "Only top-level public messages expected");
        generated.push_str(&format!("        {name:?} => super::{}::{}::decode(bytes).map_err(|e|e.to_string())?.encode_to_vec(),\n",pieces[0],pieces[1].to_upper_camel_case()));
    }
    generated.push_str("        _ => return Err(\"unsupported public message type\".into()),\n    })\n}\n");
    fs::write(output.join("descriptor.rs"), generated).unwrap();
    fs::remove_file(output.join("schema.pb")).unwrap();
}
