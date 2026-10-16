//! How the browser presents its screen.
//!
//! Headful Chrome reports the real display it runs on. Headless Chrome has no display and
//! by default reports an 800x600 screen at 1x with a "HeadlessChrome" user agent, all
//! classic headless tells. So each profile records one display (detected once, from the
//! machine's main screen) and headless launches present exactly that display (the user
//! agent is handled in `calibrate`). A profile therefore looks like the same machine
//! whether or not a window is shown.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Display {
    /// Screen size in CSS pixels (what `screen.width/height` report).
    pub width: u32,
    pub height: u32,
    pub device_pixel_ratio: f64,
    /// 30 on wide-gamut (P3) Mac panels, 24 on typical external monitors.
    pub color_depth: u32,
    /// Space taken by the OS menu bar; `screen.availHeight = height - work_area_top`.
    pub work_area_top: u32,
    /// The panel covers the P3 gamut: CSS `(color-gamut: p3)` matches.
    #[serde(default = "detected_wide_gamut")]
    pub wide_gamut: bool,
}

// Profiles created before this field existed take it from the machine, which is what
// their headful sessions have been showing all along.
fn detected_wide_gamut() -> bool {
    Display::detect().wide_gamut
}

impl Default for Display {
    /// A 1920x1080 external monitor, the most common desktop configuration.
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            device_pixel_ratio: 1.0,
            color_depth: 24,
            work_area_top: 0,
            wide_gamut: false,
        }
    }
}

impl Display {
    /// The machine's main display, or the default when it can't be determined.
    pub fn detect() -> Self {
        detect_main_display().unwrap_or_default()
    }

    /// Value for headless Chrome's `--screen-info` switch. Its geometry is in device
    /// pixels, so CSS sizes are scaled up by the pixel ratio.
    pub fn screen_info_switch(&self) -> String {
        let px = |css: u32| (f64::from(css) * self.device_pixel_ratio).round() as u32;
        format!(
            "--screen-info={{0,0 {}x{} devicePixelRatio={} colorDepth={} workAreaTop={} isInternal=true}}",
            px(self.width),
            px(self.height),
            self.device_pixel_ratio,
            self.color_depth,
            px(self.work_area_top)
        )
    }

    /// CSS media features headless Chrome can't derive from `--screen-info` (it has no
    /// panel to ask), as `Emulation.setEmulatedMedia` features. Headful Chrome reads
    /// them from the real display, so without these a headless page sees an sRGB
    /// screen where the same profile headful has a P3 one.
    ///
    /// `(dynamic-range: high)` differs the same way on HDR (XDR) panels, but neither
    /// `--screen-info` nor media emulation can set it; it remains a known gap.
    pub(crate) fn media_features(&self) -> serde_json::Value {
        serde_json::json!([
            { "name": "color-gamut", "value": if self.wide_gamut { "p3" } else { "srgb" } },
        ])
    }
}

#[cfg(target_os = "macos")]
fn detect_main_display() -> Option<Display> {
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let screens = json["SPDisplaysDataType"]
        .as_array()?
        .iter()
        .flat_map(|gpu| {
            gpu["spdisplays_ndrvs"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        });
    let main = screens
        .into_iter()
        .find(|s| s["spdisplays_main"] == "spdisplays_yes")?;
    // "_spdisplays_resolution": "1800 x 1169 @ 120.00Hz" (points),
    // "_spdisplays_pixels": "3600 x 2338" (backing pixels).
    let dims = |key: &str| -> Option<(u32, u32)> {
        let text = main[key].as_str()?;
        let mut parts = text.split(['x', '@']).map(|p| p.trim().parse::<u32>());
        Some((parts.next()?.ok()?, parts.next()?.ok()?))
    };
    let (width, height) = dims("_spdisplays_resolution")?;
    let (pixels_wide, _) = dims("_spdisplays_pixels").unwrap_or((width, height));
    let internal = main["spdisplays_connection_type"] == "spdisplays_internal";
    // e.g. "spdisplays_built-in-liquid-retina-xdr", or the Pro Display XDR.
    let xdr = main["spdisplays_display_type"]
        .as_str()
        .is_some_and(|t| t.contains("xdr"));
    Some(Display {
        width,
        height,
        device_pixel_ratio: (f64::from(pixels_wide) / f64::from(width)).round().max(1.0),
        // Chrome reports 30 bits for built-in (wide-gamut) panels.
        color_depth: if internal { 30 } else { 24 },
        // The menu bar is 40pt on notched built-in panels and 25pt elsewhere.
        work_area_top: if internal { 40 } else { 25 },
        // Built-in Mac panels and XDR displays are P3.
        wide_gamut: internal || xdr,
    })
}

/// The X screen `$DISPLAY` names (on a server, the Xvfb screen headful Chrome draws on).
#[cfg(target_os = "linux")]
fn detect_main_display() -> Option<Display> {
    std::env::var_os("DISPLAY")?;
    let output = Command::new("xdpyinfo").output().ok()?;
    parse_xdpyinfo(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn detect_main_display() -> Option<Display> {
    None
}

/// Reads the default screen from `xdpyinfo` output:
/// "dimensions:    1920x1080 pixels (508x285 millimeters)" and
/// "depth of root window:    24 planes".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_xdpyinfo(text: &str) -> Option<Display> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
    };
    let (width, height) = field("dimensions:")?.split_once('x')?;
    Some(Display {
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        // X reports device pixels; Chrome on Linux scales only when told to
        // (--force-device-scale-factor or desktop settings a bare X server lacks).
        device_pixel_ratio: 1.0,
        color_depth: field("depth of root window:")?.parse().ok()?,
        // Without a desktop environment there is no panel taking screen space.
        work_area_top: 0,
        // An X server reports no color gamut; Chrome on Linux presents sRGB.
        wide_gamut: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_info_switch_format() {
        let display = Display {
            width: 1800,
            height: 1169,
            device_pixel_ratio: 2.0,
            color_depth: 30,
            work_area_top: 40,
            wide_gamut: true,
        };
        assert_eq!(
            display.screen_info_switch(),
            "--screen-info={0,0 3600x2338 devicePixelRatio=2 colorDepth=30 workAreaTop=80 isInternal=true}"
        );
        assert_eq!(
            display.media_features(),
            serde_json::json!([{ "name": "color-gamut", "value": "p3" }])
        );
        let external = Display::default();
        assert_eq!(external.media_features()[0]["value"], "srgb");
    }

    #[test]
    fn xdpyinfo_screen() {
        let text = "name of display:    :99\nscreen #0:\n  dimensions:    1920x1080 pixels (508x285 millimeters)\n  resolution:    96x96 dots per inch\n  depth of root window:    24 planes\n";
        assert_eq!(parse_xdpyinfo(text), Some(Display::default()));
        assert_eq!(parse_xdpyinfo("screen #0:\n"), None);
    }
}
