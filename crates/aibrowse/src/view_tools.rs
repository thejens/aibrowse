//! The tools that look at the page without changing it: `observe` (the outline, by
//! region, with an optional marked screenshot), `read` and `screenshot`.

use aibrowse_navigator::{DEFAULT_DIGEST_TOKENS, Jev, digest};
use aibrowse_perception::{DEFAULT_PAGE_CHARS, Detail};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData as McpError, tool, tool_router};

use crate::respond::{
    dialog_block, failure, model_for_marks, outline_with_model, screenshot as capture, text,
};
use crate::server::Aibrowse;
use crate::tools::{ObserveParams, ReadParams, ReadView, ScreenshotParams};

#[tool_router(router = view_router, vis = "pub(crate)")]
impl Aibrowse {
    #[tool(
        description = "Outline of the active page: page type, forms, landmarks, headings and actionable elements with refs (e.g. [e123]) to pass to act. detail=full adds text content; region narrows it to one part of the page; screenshot=true adds a JPEG, marks=true labels refs on it."
    )]
    async fn observe(
        &self,
        Parameters(p): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let detail = p.detail.map_or(Detail::Interactive, Detail::from);
        let (outline, model) = outline_with_model(live, detail, p.region.as_deref()).await;
        let mut content = vec![ContentBlock::text(outline)];
        if p.screenshot.unwrap_or(false) {
            let marks = model.as_ref().filter(|_| p.marks.unwrap_or(false));
            match capture(live, marks).await {
                Ok(data) => content.push(ContentBlock::image(data, "image/jpeg")),
                Err(err) => return Ok(failure(err)),
            }
        }
        Ok(CallToolResult::success(content))
    }

    #[tool(
        description = "The active page's main content as markdown (headings, paragraphs, lists, links, tables, code), paginated by ~20k characters. Use it to read articles, docs, results and messages. view=digest returns a short version instead (~500 tokens: the page's own key passages, chosen for focus, never rewritten), to judge a page or pick out one fact cheaply."
    )]
    async fn read(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        if let Some(blocked) = dialog_block(live) {
            return Ok(failure(blocked));
        }
        if p.view == Some(ReadView::Digest) {
            // Jev picks the passages; without it, each section's opening is kept.
            let jev = self
                .jev
                .get_or_try_init(|| async { Jev::from_env() })
                .await
                .ok()
                .cloned();
            let budget = p
                .max_tokens
                .unwrap_or(DEFAULT_DIGEST_TOKENS)
                .clamp(100, 4000);
            return Ok(
                match digest(live.tab(), jev.as_ref(), p.focus.as_deref(), budget).await {
                    Ok(digest) => text(digest),
                    Err(err) => failure(err),
                },
            );
        }
        Ok(
            match aibrowse_perception::read(live.tab(), p.offset.unwrap_or(0), DEFAULT_PAGE_CHARS)
                .await
            {
                Ok(markdown) => text(markdown),
                Err(err) => failure(err),
            },
        )
    }

    #[tool(
        description = "Screenshot of the visible part of the active page (JPEG, CSS-pixel size). marks=true outlines actionable elements and labels them with their refs."
    )]
    async fn screenshot(
        &self,
        Parameters(p): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut guard = match self.session().await {
            Ok(guard) => guard,
            Err(err) => return Ok(err),
        };
        let live = guard.as_mut().expect("session ensured");
        let model = if p.marks.unwrap_or(false) {
            model_for_marks(live).await
        } else {
            None
        };
        Ok(match capture(live, model.as_ref()).await {
            Ok(data) => CallToolResult::success(vec![ContentBlock::image(data, "image/jpeg")]),
            Err(err) => failure(err),
        })
    }
}
