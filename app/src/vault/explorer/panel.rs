//! Vault explorer panel — B-1 stub.
//!
//! Owns no state in this commit beyond a handle to the [`VaultManager`]
//! singleton. Renders the appropriate empty state based on
//! [`VaultState`]:
//! - `Uninitialized` → "Set up your knowledge vault" CTA (handled by
//!   the workspace banner today; we render a hint here)
//! - `Ready` → "Vault tree loading…" placeholder (real tree lands B-2)
//! - `Locked { by_pid }` → "Owned by another instance" message
//! - `Error(msg)` → the message
//!
//! Subscribes to `VaultManager` so empty states track state changes.

use warpui::{
    elements::{
        Container, CrossAxisAlignment, Element, Flex, MainAxisSize, ParentElement, Text,
    },
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext,
};

use crate::appearance::Appearance;
use crate::vault::manager::{VaultManager, VaultManagerEvent, VaultState};

const PANEL_HORIZONTAL_PADDING: f32 = 16.0;
const PANEL_VERTICAL_PADDING: f32 = 16.0;
const HEADING_FONT_SIZE: f32 = 13.0;
const BODY_FONT_SIZE: f32 = 12.0;
const SECTION_SPACING: f32 = 8.0;

#[derive(Clone, Debug)]
pub enum VaultPanelAction {
    /// User clicked the "Set up vault" CTA in the empty state.
    /// The workspace handles the actual setup flow (file picker etc.).
    RequestSetup,
}

/// Vault explorer panel view.
pub struct VaultPanel {
    vault: ModelHandle<VaultManager>,
}

impl VaultPanel {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let vault = VaultManager::handle(ctx);
        // Re-render whenever the vault state changes.
        ctx.subscribe_to_model(&vault, |_me, _, event, ctx| match event {
            VaultManagerEvent::StateChanged { .. } => {
                ctx.notify();
            }
        });
        Self { vault }
    }
}

impl Entity for VaultPanel {
    type Event = ();
}

impl TypedActionView for VaultPanel {
    type Action = VaultPanelAction;

    fn handle_action(&mut self, action: &VaultPanelAction, _ctx: &mut ViewContext<Self>) {
        match action {
            // Workspace will surface the actual setup affordance once the
            // first-run flow lands. For now this is a no-op placeholder.
            VaultPanelAction::RequestSetup => {}
        }
    }
}

impl View for VaultPanel {
    fn ui_name() -> &'static str {
        "VaultPanel"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, _ctx: &mut ViewContext<Self>) {}

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let manager = self.vault.as_ref(app);

        let (heading, body) = match manager.state() {
            VaultState::Uninitialized => (
                "Vault not set up",
                "Open Settings → Vault to point unwarp at an Obsidian vault, or create a new one at ~/Documents/unwarp-vault.",
            ),
            VaultState::Ready => (
                "Vault",
                "Tree view coming soon. (B-2 lands the file walker.)",
            ),
            VaultState::Locked { by_pid } => {
                // Need an owned String for the body; we lose the static-str
                // optimization for this branch.
                return self.render_message(
                    "Vault locked",
                    &format!(
                        "Another unwarp instance (pid {by_pid}) owns this vault. Quit it to take over."
                    ),
                    appearance,
                );
            }
            VaultState::Error(msg) => {
                return self.render_message("Vault error", msg, appearance);
            }
        };

        self.render_message(heading, body, appearance)
    }
}

impl VaultPanel {
    fn render_message(
        &self,
        heading: &str,
        body: &str,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let heading_text = Text::new_inline(
            heading.to_owned(),
            appearance.ui_font_family(),
            HEADING_FONT_SIZE,
        )
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();

        let body_text = Text::new_inline(
            body.to_owned(),
            appearance.ui_font_family(),
            BODY_FONT_SIZE,
        )
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();

        let column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(heading_text)
            .with_child(
                Container::new(body_text)
                    .with_padding_top(SECTION_SPACING)
                    .finish(),
            )
            .finish();

        Container::new(column)
            .with_padding_left(PANEL_HORIZONTAL_PADDING)
            .with_padding_right(PANEL_HORIZONTAL_PADDING)
            .with_padding_top(PANEL_VERTICAL_PADDING)
            .with_padding_bottom(PANEL_VERTICAL_PADDING)
            .finish()
    }
}
