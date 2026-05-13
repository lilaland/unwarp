//! Ollama monitoring panel view.
//!
//! v1: read-only display of running + pulled models, polled every 2s.
//! Pull/unload UI deferred to v1.1.

use std::time::Duration;

use warpui::{
    elements::{
        Container, CrossAxisAlignment, Element, Empty, Flex, MainAxisSize, ParentElement, Text,
    },
    AppContext, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
};

use crate::settings::UnwarpSettings;
use settings::Setting as _;

use super::client::{format_bytes, OllamaClient, OllamaError, PulledModel, RunningModel};

const POLL_INTERVAL_SECS: u64 = 2;
const SECTION_HEADING_FONT_SIZE: f32 = 12.0;
const ITEM_FONT_SIZE: f32 = 13.0;
const ITEM_DETAIL_FONT_SIZE: f32 = 11.0;
const PANEL_HORIZONTAL_PADDING: f32 = 12.0;
const SECTION_VERTICAL_SPACING: f32 = 12.0;
const ITEM_VERTICAL_SPACING: f32 = 4.0;

#[derive(Clone, Debug)]
pub enum OllamaPanelAction {
    /// Manual refresh (the toolbar button). Cancels nothing — the in-flight
    /// poll completes normally; this just kicks off an extra one immediately.
    Refresh,
}

pub struct OllamaPanel {
    client: OllamaClient,
    state: OllamaState,
}

#[derive(Debug, Default)]
struct OllamaState {
    running: Vec<RunningModel>,
    pulled: Vec<PulledModel>,
    /// Most recent error from either endpoint. Cleared on a successful poll.
    last_error: Option<OllamaError>,
    /// Have we ever completed a poll? Distinguishes "loading" from "empty".
    has_polled_once: bool,
}

impl OllamaPanel {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let base_url = UnwarpSettings::as_ref(ctx)
            .embed_base_url
            .value()
            .to_owned();
        let mut panel = Self {
            client: OllamaClient::new(base_url),
            state: OllamaState::default(),
        };
        // First poll fires immediately; the callback chains the next one
        // after POLL_INTERVAL_SECS.
        panel.spawn_poll(ctx, /* delay = */ Duration::ZERO);
        panel
    }

    fn spawn_poll(&mut self, ctx: &mut ViewContext<Self>, delay: Duration) {
        let client = self.client.clone();
        let _ = ctx.spawn(
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let (running, pulled) =
                    tokio::join!(client.list_running(), client.list_pulled());
                (running, pulled)
            },
            |panel, (running, pulled), ctx| {
                panel.apply_poll_result(running, pulled);
                ctx.notify();
                // Chain: schedule the next poll.
                panel.spawn_poll(ctx, Duration::from_secs(POLL_INTERVAL_SECS));
            },
        );
    }

    fn apply_poll_result(
        &mut self,
        running: Result<Vec<RunningModel>, OllamaError>,
        pulled: Result<Vec<PulledModel>, OllamaError>,
    ) {
        self.state.has_polled_once = true;
        match (running, pulled) {
            (Ok(r), Ok(p)) => {
                self.state.running = r;
                self.state.pulled = p;
                self.state.last_error = None;
            }
            // If either endpoint fails we treat the poll as failed; both
            // endpoints share the same Ollama, so a failure on one is almost
            // certainly a failure on both.
            (Err(e), _) | (_, Err(e)) => {
                self.state.last_error = Some(e);
            }
        }
    }

    fn render_section_heading(
        &self,
        text: &str,
        appearance: &warp_core::ui::appearance::Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        Container::new(
            Text::new_inline(
                text.to_owned(),
                appearance.ui_font_family(),
                SECTION_HEADING_FONT_SIZE,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_padding_bottom(ITEM_VERTICAL_SPACING)
        .finish()
    }

    fn render_running_row(
        &self,
        m: &RunningModel,
        appearance: &warp_core::ui::appearance::Appearance,
    ) -> Box<dyn Element> {
        let location = if m.size_vram > 0 && m.size_vram == m.size {
            "GPU"
        } else if m.size_vram > 0 {
            "GPU+CPU"
        } else {
            "CPU"
        };
        let detail = format!("{} · {location}", format_bytes(m.size));
        self.render_two_line_row(&m.name, &detail, appearance)
    }

    fn render_pulled_row(
        &self,
        m: &PulledModel,
        appearance: &warp_core::ui::appearance::Appearance,
    ) -> Box<dyn Element> {
        let detail_parts: Vec<String> = [
            (!m.details.parameter_size.is_empty()).then(|| m.details.parameter_size.clone()),
            (!m.details.quantization_level.is_empty())
                .then(|| m.details.quantization_level.clone()),
            Some(format_bytes(m.size)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let detail = detail_parts.join(" · ");
        self.render_two_line_row(&m.name, &detail, appearance)
    }

    fn render_two_line_row(
        &self,
        primary: &str,
        secondary: &str,
        appearance: &warp_core::ui::appearance::Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let primary_text = Text::new_inline(
            primary.to_owned(),
            appearance.ui_font_family(),
            ITEM_FONT_SIZE,
        )
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();
        let secondary_text = Text::new_inline(
            secondary.to_owned(),
            appearance.ui_font_family(),
            ITEM_DETAIL_FONT_SIZE,
        )
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();

        let column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(primary_text)
            .with_child(secondary_text)
            .finish();

        Container::new(column)
            .with_padding_bottom(ITEM_VERTICAL_SPACING)
            .finish()
    }

    fn render_empty_message(
        &self,
        text: &str,
        appearance: &warp_core::ui::appearance::Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        Container::new(
            Text::new_inline(
                text.to_owned(),
                appearance.ui_font_family(),
                ITEM_DETAIL_FONT_SIZE,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_padding_bottom(ITEM_VERTICAL_SPACING)
        .finish()
    }
}

impl Entity for OllamaPanel {
    type Event = ();
}

impl TypedActionView for OllamaPanel {
    type Action = OllamaPanelAction;

    fn handle_action(&mut self, action: &OllamaPanelAction, ctx: &mut ViewContext<Self>) {
        match action {
            OllamaPanelAction::Refresh => {
                self.spawn_poll(ctx, Duration::ZERO);
            }
        }
    }
}

impl View for OllamaPanel {
    fn ui_name() -> &'static str {
        "OllamaPanel"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, _ctx: &mut ViewContext<Self>) {}

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = warp_core::ui::appearance::Appearance::as_ref(app);

        // Build section content. If we have an error, surface it first; then
        // the running/pulled lists or their empty states.
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        if let Some(err) = &self.state.last_error {
            column = column.with_child(self.render_empty_message(
                &format!("⚠ {err}\nStart Ollama: `ollama serve`"),
                appearance,
            ));
        }

        // Running section
        column = column.with_child(self.render_section_heading("Running", appearance));
        if self.state.running.is_empty() {
            column = column.with_child(self.render_empty_message(
                if !self.state.has_polled_once {
                    "Loading…"
                } else if self.state.last_error.is_some() {
                    ""
                } else {
                    "No models loaded. Send a chat or run `/chat` to load one."
                },
                appearance,
            ));
        } else {
            for m in &self.state.running {
                column = column.with_child(self.render_running_row(m, appearance));
            }
        }

        // Spacer between sections.
        column = column.with_child(
            Container::new(Empty::new().finish())
                .with_padding_top(SECTION_VERTICAL_SPACING)
                .finish(),
        );

        // Pulled section
        column = column.with_child(self.render_section_heading("Pulled", appearance));
        if self.state.pulled.is_empty() {
            column = column.with_child(self.render_empty_message(
                if !self.state.has_polled_once {
                    "Loading…"
                } else if self.state.last_error.is_some() {
                    ""
                } else {
                    "No models pulled. Run `ollama pull gemma3` in a terminal."
                },
                appearance,
            ));
        } else {
            for m in &self.state.pulled {
                column = column.with_child(self.render_pulled_row(m, appearance));
            }
        }

        Container::new(column.with_main_axis_size(MainAxisSize::Min).finish())
            .with_padding_left(PANEL_HORIZONTAL_PADDING)
            .with_padding_right(PANEL_HORIZONTAL_PADDING)
            .with_padding_top(SECTION_VERTICAL_SPACING)
            .finish()
    }
}
