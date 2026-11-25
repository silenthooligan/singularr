//! Parse release names given as arguments and print each field with its evidence.
//!
//! `cargo run -p singularr-core --example parse_name -- "Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG"`

use singularr_core::release::parse;

fn main() {
    for name in std::env::args().skip(1) {
        let p = parse(&name);
        println!("{name}");
        println!(
            "  kind {:?}, title {:?}, year {:?}, season {:?}, episodes {:?}, air date {:?}",
            p.kind, p.title, p.year, p.season, p.episodes, p.air_date
        );
        println!(
            "  quality {:?} (source {:?}, resolution {:?}), service {:?}",
            p.quality().map(|q| q.label()),
            p.source,
            p.resolution,
            p.service
        );
        println!(
            "  video {:?} {:?} bit, hdr {:?}, audio {:?} {:?} atmos {}",
            p.video_codec, p.bit_depth, p.hdr, p.audio_codec, p.audio_channels, p.atmos
        );
        println!(
            "  editions {:?}, fps {:?}, ai {}, proper {}, repack {}, languages {:?}",
            p.editions, p.fps, p.ai_upscaled, p.proper, p.repack, p.languages
        );
        println!("  group {:?}, collective {:?}", p.group, p.collective);
        for n in &p.notes {
            println!(
                "    {:<14} {:>3}  {:<28} -> {}",
                n.field,
                n.at,
                format!("{:?}", n.token),
                n.read_as
            );
        }
    }
}
