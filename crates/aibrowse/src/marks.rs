//! Set-of-marks screenshots: the viewport JPEG with every visible actionable element
//! outlined and labelled with its ref, so a vision-capable agent can map what it sees to
//! the refs it acts on.
//!
//! The marks are drawn here, on the captured image. Nothing is ever injected into the
//! page: an overlay in the DOM would be visible to the site's scripts (and to mutation
//! observers watching for automation), and would shift layout under the agent.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use aibrowse_perception::Mark;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};

/// Go Mono Bold, subset to ASCII (~9 KB): monospaced, so label widths are exact, and
/// bold enough to stay legible after JPEG compression at small sizes.
const FONT: &[u8] = include_bytes!("../assets/Go-Mono-Bold-ascii.ttf");
/// Label text height in CSS pixels: small enough not to hide the page, large enough for
/// vision models to read reliably.
const TEXT_PX: f32 = 13.0;
const PAD: u32 = 2;
const BORDER: u32 = 2;
/// Re-encoding quality; matches the plain screenshots closely enough in size.
const QUALITY: u8 = 80;

/// Saturated colours that white text reads well on, cycled so that neighbouring boxes
/// are told apart.
const PALETTE: [[u8; 3]; 6] = [
    [215, 25, 60],
    [0, 95, 205],
    [0, 130, 65],
    [145, 40, 175],
    [200, 85, 0],
    [0, 120, 135],
];

/// Draws `marks` on a base64 JPEG screenshot of a viewport `viewport_width` CSS pixels
/// wide, and returns the result as a base64 JPEG. Mark boxes are in viewport CSS pixels;
/// screenshots are captured at CSS-pixel size, so the scale is normally 1.
pub fn draw_marks(
    jpeg_base64: &str,
    marks: &[Mark],
    viewport_width: f64,
) -> Result<String, String> {
    let bytes = STANDARD
        .decode(jpeg_base64)
        .map_err(|e| format!("screenshot is not base64: {e}"))?;
    let mut img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .map_err(|e| format!("cannot decode the screenshot: {e}"))?
        .to_rgb8();
    let scale = if viewport_width > 0.0 {
        f64::from(img.width()) / viewport_width
    } else {
        1.0
    };
    let labelled: Vec<(String, [f64; 4])> = marks
        .iter()
        .map(|m| (m.reference.to_string(), m.rect.map(|v| v * scale)))
        .collect();
    annotate(&mut img, &labelled);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, QUALITY)
        .encode_image(&img)
        .map_err(|e| format!("cannot encode the marked screenshot: {e}"))?;
    Ok(STANDARD.encode(out))
}

/// Outlines each box and puts its label next to it. Boxes are drawn first and labels
/// last, so no box line ever crosses a label.
fn annotate(img: &mut RgbImage, marks: &[(String, [f64; 4])]) {
    let font = FontRef::try_from_slice(FONT).expect("embedded font parses");
    let scaled = font.as_scaled(PxScale::from(TEXT_PX));
    let boxes: Vec<Option<Rect>> = marks
        .iter()
        .map(|(_, r)| Rect::from_css(*r, img.width(), img.height()))
        .collect();
    for (i, rect) in boxes.iter().enumerate() {
        if let Some(rect) = rect {
            outline(img, *rect, color(i));
        }
    }
    let mut placed: Vec<Rect> = Vec::new();
    for (i, ((label, _), rect)) in marks.iter().zip(&boxes).enumerate() {
        let Some(rect) = rect else { continue };
        let width = label
            .chars()
            .map(|c| scaled.h_advance(font.glyph_id(c)))
            .sum::<f32>()
            .ceil() as u32
            + 2 * PAD;
        let height = scaled.height().ceil() as u32 + 2 * PAD;
        let spot = place_label(*rect, width, height, img.width(), img.height(), &placed);
        fill(img, spot, color(i));
        text(img, &font, spot.x + PAD, spot.y + PAD, label);
        placed.push(spot);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    /// Rounds a CSS box to pixels and clips it to the image; `None` when nothing is left.
    fn from_css([x, y, w, h]: [f64; 4], width: u32, height: u32) -> Option<Self> {
        let x0 = x.round().clamp(0.0, f64::from(width)) as u32;
        let y0 = y.round().clamp(0.0, f64::from(height)) as u32;
        let x1 = (x + w).round().clamp(0.0, f64::from(width)) as u32;
        let y1 = (y + h).round().clamp(0.0, f64::from(height)) as u32;
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// Where a label goes: just above the box's top-left corner (the convention people read
/// first), else inside it, below it, or above its right end, whichever is the first not
/// to cover an earlier label. Crowded spots fall back to the first choice; a full search
/// would cost more than the occasional overlap.
fn place_label(target: Rect, w: u32, h: u32, width: u32, height: u32, placed: &[Rect]) -> Rect {
    let clamp = |x: i64, y: i64| Rect {
        x: x.clamp(0, i64::from(width.saturating_sub(w))) as u32,
        y: y.clamp(0, i64::from(height.saturating_sub(h))) as u32,
        w,
        h,
    };
    let (x, y) = (i64::from(target.x), i64::from(target.y));
    let candidates = [
        clamp(x, y - i64::from(h)),
        clamp(x, y),
        clamp(x, y + i64::from(target.h)),
        clamp(x + i64::from(target.w) - i64::from(w), y - i64::from(h)),
    ];
    candidates
        .iter()
        .find(|c| !placed.iter().any(|p| p.overlaps(c)))
        .copied()
        .unwrap_or(candidates[0])
}

fn color(i: usize) -> Rgb<u8> {
    Rgb(PALETTE[i % PALETTE.len()])
}

fn outline(img: &mut RgbImage, r: Rect, c: Rgb<u8>) {
    let t = BORDER.min(r.w).min(r.h);
    fill(img, Rect { h: t, ..r }, c);
    fill(
        img,
        Rect {
            y: r.y + r.h - t,
            h: t,
            ..r
        },
        c,
    );
    fill(img, Rect { w: t, ..r }, c);
    fill(
        img,
        Rect {
            x: r.x + r.w - t,
            w: t,
            ..r
        },
        c,
    );
}

fn fill(img: &mut RgbImage, r: Rect, c: Rgb<u8>) {
    for y in r.y..(r.y + r.h).min(img.height()) {
        for x in r.x..(r.x + r.w).min(img.width()) {
            img.put_pixel(x, y, c);
        }
    }
}

/// White text with its top-left at (x, y), alpha-blended by glyph coverage.
fn text(img: &mut RgbImage, font: &FontRef, x: u32, y: u32, label: &str) {
    let scaled = font.as_scaled(PxScale::from(TEXT_PX));
    let mut caret = x as f32;
    let baseline = y as f32 + scaled.ascent();
    for c in label.chars() {
        let glyph = font
            .glyph_id(c)
            .with_scale_and_position(PxScale::from(TEXT_PX), point(caret, baseline));
        caret += scaled.h_advance(glyph.id);
        let Some(outlined) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outlined.px_bounds();
        outlined.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i64 + i64::from(gx);
            let py = bounds.min.y as i64 + i64::from(gy);
            if px < 0 || py < 0 || px >= i64::from(img.width()) || py >= i64::from(img.height()) {
                return;
            }
            let pixel = img.get_pixel_mut(px as u32, py as u32);
            for channel in &mut pixel.0 {
                let blended = f32::from(*channel) * (1.0 - coverage) + 255.0 * coverage;
                *channel = blended.round() as u8;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb<u8> = Rgb([255, 255, 255]);

    fn blank() -> RgbImage {
        RgbImage::from_pixel(200, 120, WHITE)
    }

    #[test]
    fn outlines_the_box_and_labels_it_above() {
        let mut img = blank();
        annotate(&mut img, &[("e12".into(), [40.0, 50.0, 80.0, 30.0])]);
        let red = color(0);
        // The border, on all four sides.
        assert_eq!(*img.get_pixel(40, 65), red);
        assert_eq!(*img.get_pixel(119, 65), red);
        assert_eq!(*img.get_pixel(80, 50), red);
        assert_eq!(*img.get_pixel(80, 79), red);
        // Inside and outside the box the page is untouched.
        assert_eq!(*img.get_pixel(80, 65), WHITE);
        assert_eq!(*img.get_pixel(150, 100), WHITE);
        // The label sits right above the top-left corner: a red plate with white text.
        let plate = (40..66).flat_map(|x| (34..50).map(move |y| (x, y)));
        let (mut background, mut ink) = (0, 0);
        for (x, y) in plate {
            match *img.get_pixel(x, y) {
                p if p == red => background += 1,
                p if p == WHITE => ink += 1,
                _ => {}
            }
        }
        assert!(background > 150, "label background: {background}");
        assert!(ink > 15, "label text: {ink}");
        // Nothing to the right of the label's width.
        assert_eq!(*img.get_pixel(110, 40), WHITE);
    }

    #[test]
    fn labels_of_neighbours_do_not_cover_each_other() {
        let first = Rect {
            x: 10,
            y: 40,
            w: 60,
            h: 20,
        };
        let a = place_label(first, 30, 17, 200, 120, &[]);
        let b = place_label(first, 30, 17, 200, 120, &[a]);
        assert_eq!((a.x, a.y), (10, 23));
        assert!(!a.overlaps(&b), "{a:?} {b:?}");
    }

    #[test]
    fn labels_stay_inside_the_image() {
        let corner = Rect {
            x: 190,
            y: 0,
            w: 10,
            h: 10,
        };
        let spot = place_label(corner, 30, 17, 200, 120, &[]);
        assert!(spot.x + spot.w <= 200 && spot.y + spot.h <= 120, "{spot:?}");
    }

    #[test]
    fn round_trips_through_jpeg() {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 75)
            .encode_image(&blank())
            .unwrap();
        let plain = STANDARD.encode(&jpeg);
        let mark = Mark {
            reference: aibrowse_browser::ElementRef::parse("e7").unwrap(),
            rect: [20.0, 30.0, 50.0, 20.0],
        };
        let marked = draw_marks(&plain, &[mark], 200.0).unwrap();
        assert_ne!(marked, plain);
        let decoded = image::load_from_memory(&STANDARD.decode(&marked).unwrap())
            .unwrap()
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (200, 120));
        let border = decoded.get_pixel(20, 40);
        assert!(border[0] > 150 && border[1] < 100, "{border:?}");
    }
}
