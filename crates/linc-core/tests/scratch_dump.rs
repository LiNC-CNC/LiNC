use linc_core::gcode::parse_preview;

#[test]
fn dump_g7x_probe() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs/gcode/g7x-probe.ngc");
    let text = std::fs::read_to_string(&path).unwrap();
    let axes = vec!["X".to_string(), "Z".to_string()];
    let preview = parse_preview(&text, &axes, None, [0.0; 3]);
    println!("error: {:?}", preview.error);
    println!(
        "incomplete: {:?} {:?}",
        preview.incomplete, preview.incomplete_reason
    );
    for (i, s) in preview.segments.iter().enumerate() {
        println!(
            "{i:4} line{:4?} {:?} X{:8.3} Z{:8.3} -> X{:8.3} Z{:8.3}",
            s.line, s.kind, s.start[0], s.start[2], s.end[0], s.end[2]
        );
    }
    let n = preview.segments.len();
    println!("segments: {n}");
}
