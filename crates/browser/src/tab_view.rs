//! What the agent sees of the page's geometry: the viewport, and screenshots of it.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::error::Result;
use crate::tab::{Tab, Viewport};

impl Tab {
    pub async fn viewport(&self) -> Result<Viewport> {
        let metrics: LayoutMetrics = self
            .session
            .call_as("Page.getLayoutMetrics", json!({}))
            .await?;
        Ok(Viewport {
            width: metrics.css_visual_viewport.client_width,
            height: metrics.css_visual_viewport.client_height,
            scroll_x: metrics.css_visual_viewport.page_x,
            scroll_y: metrics.css_visual_viewport.page_y,
            content_width: metrics.css_content_size.width,
            content_height: metrics.css_content_size.height,
            // The legacy `visualViewport` is in device pixels; the css* variant isn't.
            device_pixel_ratio: if metrics.css_visual_viewport.client_width > 0.0 {
                metrics.visual_viewport.client_width / metrics.css_visual_viewport.client_width
            } else {
                1.0
            },
        })
    }

    /// A JPEG of the current viewport at CSS-pixel size, base64-encoded.
    ///
    /// On HiDPI displays Chrome would capture at device resolution (2880x1514 for a
    /// 1440x757 viewport on Retina): four times the image tokens, and pixel coordinates
    /// that don't match the Page Model's. Downscaling to CSS pixels makes screenshots
    /// identical in size and geometry whether headless or not, on any display.
    pub async fn screenshot(&self) -> Result<String> {
        let viewport = self.viewport().await?;
        let response = self
            .session
            .call_with_timeout(
                "Page.captureScreenshot",
                json!({
                    "format": "jpeg",
                    "quality": 75,
                    // Clip coordinates are document coordinates in CSS pixels.
                    "clip": {
                        "x": viewport.scroll_x,
                        "y": viewport.scroll_y,
                        "width": viewport.width,
                        "height": viewport.height,
                        "scale": 1.0 / viewport.device_pixel_ratio,
                    },
                }),
                Duration::from_secs(15),
            )
            .await?;
        Ok(response["data"].as_str().unwrap_or_default().to_owned())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutMetrics {
    css_visual_viewport: VisualViewport,
    visual_viewport: VisualViewport,
    css_content_size: ContentSize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisualViewport {
    page_x: f64,
    page_y: f64,
    client_width: f64,
    client_height: f64,
}

#[derive(Deserialize)]
struct ContentSize {
    width: f64,
    height: f64,
}
