//! The embedded fonts (SIL OFL 1.1): metrics for layout and the data the
//! writer embeds. Widths are kept in PDF glyph space (1/1000 em, rounded),
//! the same numbers the `/W` array declares, so layout and the viewer's
//! own advance never disagree.
use std::sync::OnceLock;
use ttf_parser::Face;

/// One embedded font program.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FontId {
    Regular,
    Bold,
    Mono,
    /// Inter's Latin Extended subset, for characters the others lack.
    Extended,
}

impl FontId {
    pub const ALL: [FontId; 4] = [
        FontId::Regular,
        FontId::Bold,
        FontId::Mono,
        FontId::Extended,
    ];

    pub fn data(self) -> &'static [u8] {
        match self {
            FontId::Regular => include_bytes!("../fonts/inter-latin-400.ttf"),
            FontId::Bold => include_bytes!("../fonts/inter-latin-600.ttf"),
            FontId::Mono => include_bytes!("../fonts/jetbrains-mono-latin-400.ttf"),
            FontId::Extended => include_bytes!("../fonts/inter-latin-ext-400.ttf"),
        }
    }

    /// `/BaseFont`: a subset tag (the files are Latin subsets) and the
    /// PostScript name, distinct per file so viewers never confuse them.
    pub fn base_name(self) -> &'static str {
        match self {
            FontId::Regular => "RBINTR+Inter-Regular",
            FontId::Bold => "RBINTB+Inter-SemiBold",
            FontId::Mono => "RBMONO+JetBrainsMono-Regular",
            FontId::Extended => "RBINTX+Inter-Regular",
        }
    }

    pub fn monospaced(self) -> bool {
        self == FontId::Mono
    }

    /// The font program, zlib-compressed once per process (deterministic).
    pub fn compressed(self) -> &'static [u8] {
        static CACHE: [OnceLock<Vec<u8>>; 4] = [const { OnceLock::new() }; 4];
        CACHE[self as usize]
            .get_or_init(|| miniz_oxide::deflate::compress_to_vec_zlib(self.data(), 6))
    }

    fn face(self) -> Face<'static> {
        Face::parse(self.data(), 0).expect("embedded font parses")
    }
}

/// The text styles layout asks for; each resolves to a chain of fonts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    Sans,
    SansBold,
    Mono,
}

impl Family {
    fn chain(self) -> [FontId; 2] {
        match self {
            Family::Sans => [FontId::Regular, FontId::Extended],
            Family::SansBold => [FontId::Bold, FontId::Extended],
            Family::Mono => [FontId::Mono, FontId::Extended],
        }
    }
}

/// Font descriptor facts, in glyph space.
pub struct Metrics {
    pub ascent: i32,
    pub descent: i32,
    pub cap_height: i32,
    pub bbox: [i32; 4],
}

/// Parsed faces for one render.
pub struct Fonts {
    faces: [Face<'static>; 4],
}

impl Fonts {
    pub fn load() -> Self {
        Fonts {
            faces: FontId::ALL.map(FontId::face),
        }
    }

    fn face(&self, font: FontId) -> &Face<'static> {
        &self.faces[font as usize]
    }

    fn scale(&self, font: FontId, units: i32) -> i32 {
        let per_em = i32::from(self.face(font).units_per_em());
        // Round half away from zero, in integers: no float formatting.
        let scaled = units * 1000;
        if scaled >= 0 {
            (scaled + per_em / 2) / per_em
        } else {
            (scaled - per_em / 2) / per_em
        }
    }

    /// The font and glyph for `c` in `family`, or `?` when no font in its
    /// chain has it.
    pub fn glyph(&self, family: Family, c: char) -> (FontId, u16) {
        for font in family.chain() {
            if let Some(glyph) = self.face(font).glyph_index(c) {
                return (font, glyph.0);
            }
        }
        let font = family.chain()[0];
        let glyph = self.face(font).glyph_index('?').map_or(0, |g| g.0);
        (font, glyph)
    }

    /// Advance width in glyph space (1/1000 em).
    pub fn width(&self, font: FontId, glyph: u16) -> i32 {
        let advance = self
            .face(font)
            .glyph_hor_advance(ttf_parser::GlyphId(glyph))
            .unwrap_or(0);
        self.scale(font, i32::from(advance))
    }

    pub fn metrics(&self, font: FontId) -> Metrics {
        let face = self.face(font);
        let bbox = face.global_bounding_box();
        Metrics {
            ascent: self.scale(font, i32::from(face.ascender())),
            descent: self.scale(font, i32::from(face.descender())),
            cap_height: self.scale(
                font,
                i32::from(face.capital_height().unwrap_or(face.ascender())),
            ),
            bbox: [
                self.scale(font, i32::from(bbox.x_min)),
                self.scale(font, i32::from(bbox.y_min)),
                self.scale(font, i32::from(bbox.x_max)),
                self.scale(font, i32::from(bbox.y_max)),
            ],
        }
    }
}
