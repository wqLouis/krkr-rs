//! Temporary integration check: read a real .ks from the game archive,
//! decode CP932, parse with kag.
use xp3::Xp3Archive;

fn main() {
    let mut arc = Xp3Archive::open("/mnt/DATA/Games/Others/test/patch.xp3").unwrap();
    let raw = arc.read("01_01.ks").unwrap();
    // The game ships UTF-8-with-BOM scenario files; honor BOM detection like
    // the reference text-stream layer (default UTF-8, else CP932).
    let (body, enc) = tvp_util::encoding::strip_bom(&raw);
    let text = match enc {
        Some(tvp_util::encoding::Encoding::Utf16Le) => String::from_utf16_lossy(
            &body
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        ),
        Some(tvp_util::encoding::Encoding::Utf16Be) => String::from_utf16_lossy(
            &body
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        ),
        _ => tvp_util::encoding::decode(body, "cp932")
            .or_else(|_| {
                String::from_utf8(body.to_vec()).map_err(|e| {
                    tvp_util::encoding::EncodingError::InvalidUtf8 {
                        offset: e.utf8_error().valid_up_to(),
                    }
                })
            })
            .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned()),
    };
    println!("encoding: {enc:?}");
    let scenario = kag::parse(&text).unwrap();
    let labels = scenario.labels.len();
    let tags = scenario
        .events
        .iter()
        .filter(|e| matches!(e, kag::Event::Tag { .. }))
        .count();
    let texts = scenario
        .events
        .iter()
        .filter(|e| matches!(e, kag::Event::Text { .. }))
        .count();
    let dirs = scenario
        .events
        .iter()
        .filter(|e| matches!(e, kag::Event::Directive { .. }))
        .count();
    println!(
        "01_01.ks: {} bytes -> {} events ({} labels, {} tags, {} text, {} directives)",
        raw.len(),
        scenario.events.len(),
        labels,
        tags,
        texts,
        dirs
    );
    // first few events
    for e in scenario.events.iter().take(6) {
        println!("  {e:?}");
    }
}
