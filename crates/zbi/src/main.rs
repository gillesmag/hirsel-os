use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("list") if args.len() == 3 => {
            let bytes = fs::read(&args[2]).unwrap_or_else(|e| {
                eprintln!("zbi: failed to read {}: {e}", args[2]);
                process::exit(1);
            });
            let items = zbi::parse_container(&bytes).unwrap_or_else(|e| {
                eprintln!("zbi: {e}");
                process::exit(1);
            });
            for (idx, item) in items.iter().enumerate() {
                println!("{idx:02}: {item}");
                if item.header.item_type == zbi::ZBI_TYPE_CMDLINE {
                    let text = String::from_utf8_lossy(&item.payload);
                    println!("    cmdline: {:?}", text.trim_end_matches('\0'));
                }
            }
        }
        Some("append-cmdline") if args.len() == 5 => {
            let bytes = fs::read(&args[2]).unwrap_or_else(|e| {
                eprintln!("zbi: failed to read {}: {e}", args[2]);
                process::exit(1);
            });
            let mut items = zbi::parse_container(&bytes).unwrap_or_else(|e| {
                eprintln!("zbi: {e}");
                process::exit(1);
            });
            items.push(zbi::cmdline_item(&args[4]));
            let out = zbi::serialize_container(&items);
            fs::write(&args[3], out).unwrap_or_else(|e| {
                eprintln!("zbi: failed to write {}: {e}", args[3]);
                process::exit(1);
            });
        }
        _ => {
            eprintln!("usage:");
            eprintln!("  zbi list <image.zbi>");
            eprintln!("  zbi append-cmdline <in.zbi> <out.zbi> <cmdline>");
            process::exit(2);
        }
    }
}
