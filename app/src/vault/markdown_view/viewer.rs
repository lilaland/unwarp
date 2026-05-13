//! Read-only markdown viewer view.
//!
//! Holds a vault file path + its parsed [`FormattedText`]. Renders via
//! [`FormattedTextElement`], the same renderer the changelog and HOA
//! onboarding views use, so headings / lists / tables / hyperlinks work
//! without us hand-walking the AST.
//!
//! Design notes (TDD §5.3):
//! - Read-only by design — vault files like `brew/*.md` and `*.mirror.md`
//!   open in this viewer. Editable files (`notes/`, `runbooks/`,
//!   `projects/*/notes.md`) get a separate editor-backed view in a
//!   follow-up commit.
//! - File I/O happens synchronously in `new()` and `Reload`. Vault notes
//!   are small (~KBs); async is overkill and complicates the loading state.

use std::path::{Path, PathBuf};

use markdown_parser::{
    parse_markdown_with_gfm_tables, FormattedText, FormattedTextFragment, FormattedTextLine,
};
use thiserror::Error;
use warpui::{
    elements::{Container, Element, FormattedTextElement, HighlightedHyperlink},
    AppContext, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
};

use crate::appearance::Appearance;

const VIEWER_FONT_SIZE: f32 = 14.0;
const VIEWER_HORIZONTAL_PADDING: f32 = 24.0;
const VIEWER_VERTICAL_PADDING: f32 = 16.0;

/// Errors surfaced when reading or parsing a vault markdown file.
#[derive(Debug, Error, Clone)]
pub enum ViewerError {
    #[error("file not found: {0}")]
    NotFound(String),

    #[error("could not read file {path}: {message}")]
    Read { path: String, message: String },

    #[error("file is not UTF-8: {0}")]
    NotUtf8(String),

    #[error("could not parse markdown: {0}")]
    Parse(String),
}

/// Actions the viewer can dispatch to itself or its parent.
#[derive(Clone, Debug)]
pub enum VaultMarkdownViewerAction {
    /// Re-read and re-parse the file from disk. Used by the manual
    /// refresh button and (later) by the vault watcher when the file
    /// changes out of band.
    Reload,
    /// User clicked a hyperlink in the rendered markdown. The parent view
    /// decides how to handle URLs vs. relative paths.
    OpenUrl(String),
}

/// Read-only markdown viewer for vault files.
pub struct VaultMarkdownViewer {
    file_path: PathBuf,
    parsed: Result<FormattedText, ViewerError>,
    highlighted_link: HighlightedHyperlink,
}

impl VaultMarkdownViewer {
    pub fn new(file_path: PathBuf, _ctx: &mut ViewContext<Self>) -> Self {
        let parsed = read_and_parse(&file_path);
        Self {
            file_path,
            parsed,
            highlighted_link: HighlightedHyperlink::default(),
        }
    }

    /// Path of the file currently being displayed.
    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// Reload the file from disk. Public so callers can wire it to a
    /// filesystem-watcher event in a future commit; the manual `Reload`
    /// action also routes here.
    pub fn reload(&mut self, ctx: &mut ViewContext<Self>) {
        self.parsed = read_and_parse(&self.file_path);
        ctx.notify();
    }

    fn render_error(&self, err: &ViewerError, appearance: &Appearance) -> Box<dyn Element> {
        // Wrap the error message in a FormattedText so it benefits from the
        // same font/styling pipeline; nothing we render here needs special
        // affordances.
        let text = FormattedText::new(vec![FormattedTextLine::Line(vec![
            FormattedTextFragment::plain_text(format!("⚠ {err}")),
        ])]);
        wrap(
            FormattedTextElement::new(
                text,
                VIEWER_FONT_SIZE,
                appearance.ui_font_family(),
                appearance.monospace_font_family(),
                appearance
                    .theme()
                    .sub_text_color(appearance.theme().background())
                    .into_solid(),
                self.highlighted_link.clone(),
            )
            .finish(),
        )
    }
}

fn read_and_parse(path: &Path) -> Result<FormattedText, ViewerError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ViewerError::NotFound(path.display().to_string()),
        _ => ViewerError::Read {
            path: path.display().to_string(),
            message: e.to_string(),
        },
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| ViewerError::NotUtf8(path.display().to_string()))?;
    parse_markdown_with_gfm_tables(&text).map_err(|e| ViewerError::Parse(e.to_string()))
}

fn wrap(inner: Box<dyn Element>) -> Box<dyn Element> {
    Container::new(inner)
        .with_padding_left(VIEWER_HORIZONTAL_PADDING)
        .with_padding_right(VIEWER_HORIZONTAL_PADDING)
        .with_padding_top(VIEWER_VERTICAL_PADDING)
        .with_padding_bottom(VIEWER_VERTICAL_PADDING)
        .finish()
}

impl Entity for VaultMarkdownViewer {
    type Event = ();
}

impl TypedActionView for VaultMarkdownViewer {
    type Action = VaultMarkdownViewerAction;

    fn handle_action(
        &mut self,
        action: &VaultMarkdownViewerAction,
        ctx: &mut ViewContext<Self>,
    ) {
        match action {
            VaultMarkdownViewerAction::Reload => self.reload(ctx),
            VaultMarkdownViewerAction::OpenUrl(url) => {
                // v1: open in OS default handler. Vault-relative paths can
                // be intercepted at a parent view in a follow-up.
                ctx.open_url(url.as_str());
            }
        }
    }
}

impl View for VaultMarkdownViewer {
    fn ui_name() -> &'static str {
        "VaultMarkdownViewer"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, _ctx: &mut ViewContext<Self>) {}

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);

        match &self.parsed {
            Err(err) => self.render_error(err, appearance),
            Ok(parsed) => wrap(
                FormattedTextElement::new(
                    parsed.clone(),
                    VIEWER_FONT_SIZE,
                    appearance.ui_font_family(),
                    appearance.monospace_font_family(),
                    appearance
                        .theme()
                        .main_text_color(appearance.theme().background())
                        .into_solid(),
                    self.highlighted_link.clone(),
                )
                .register_default_click_handlers(move |url, ctx, _| {
                    ctx.dispatch_typed_action(VaultMarkdownViewerAction::OpenUrl(
                        url.url.to_string(),
                    ));
                })
                .finish(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_and_parse_returns_not_found_for_missing_file() {
        let p = std::path::PathBuf::from("/definitely/not/here.md");
        let err = read_and_parse(&p).unwrap_err();
        match err {
            ViewerError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn read_and_parse_returns_not_utf8_for_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("binary.md");
        // Invalid UTF-8 byte sequence
        std::fs::write(&p, [0xff, 0xfe, 0xfd]).unwrap();
        let err = read_and_parse(&p).unwrap_err();
        assert!(matches!(err, ViewerError::NotUtf8(_)));
    }

    #[test]
    fn read_and_parse_parses_simple_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("note.md");
        std::fs::write(
            &p,
            "# Heading\n\nBody paragraph with [a link](https://example.com).\n",
        )
        .unwrap();
        let parsed = read_and_parse(&p).unwrap();
        // First non-LineBreak line should be a Heading.
        let first = parsed
            .lines
            .iter()
            .find(|l| !matches!(l, FormattedTextLine::LineBreak))
            .expect("expected at least one non-blank line");
        assert!(matches!(first, FormattedTextLine::Heading(_)));
    }

    #[test]
    fn read_and_parse_parses_gfm_table() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("tab.md");
        std::fs::write(
            &p,
            "| col | val |\n|---|---|\n| a | 1 |\n| b | 2 |\n",
        )
        .unwrap();
        let parsed = read_and_parse(&p).unwrap();
        let has_table = parsed
            .lines
            .iter()
            .any(|l| matches!(l, FormattedTextLine::Table(_)));
        assert!(has_table, "expected GFM table to parse as Table line");
    }
}
