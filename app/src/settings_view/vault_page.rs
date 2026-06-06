//! Settings page for unwarp vault configuration and maintenance.
//!
//! §5.5: Exposes an "Update brew docs" action that runs `BrewJob` on demand
//! from within the settings UI, generating one `.md` per installed Homebrew
//! formula and updating `brew/_index.md` in the vault.

use super::{
    settings_page::{
        MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
        SettingsWidget,
    },
    SettingsSection,
};
use crate::{
    appearance::Appearance,
    vault::{
        brew::job::{BrewError, BrewJob},
        manager::VaultManager,
    },
};
use warpui::{
    elements::{
        Container, CrossAxisAlignment, Element, Flex, MainAxisSize, MouseStateHandle,
        ParentElement, Text,
    },
    ui_components::{button::ButtonVariant, components::UiComponent},
    AppContext, Entity, EventContext, FocusContext, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

#[derive(Debug, Clone)]
pub enum VaultSettingsPageAction {
    UpdateBrewDocs,
}

pub struct VaultSettingsPageView {
    /// True while a brew job is in flight.
    brew_running: bool,
    /// Status line shown beneath the button after a run.
    brew_result: Option<String>,
    page: PageType<Self>,
}

impl VaultSettingsPageView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            brew_running: false,
            brew_result: None,
            page: PageType::new_monolith(VaultSettingsWidget::default(), Some("Vault"), false),
        }
    }
}

impl Entity for VaultSettingsPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for VaultSettingsPageView {
    type Action = VaultSettingsPageAction;

    fn handle_action(&mut self, action: &VaultSettingsPageAction, ctx: &mut ViewContext<Self>) {
        match action {
            VaultSettingsPageAction::UpdateBrewDocs => {
                let vault = VaultManager::handle(ctx);
                let Some(config) = vault.as_ref(ctx).config().cloned() else {
                    self.brew_result = Some("Vault is not ready.".to_owned());
                    ctx.notify();
                    return;
                };
                self.brew_running = true;
                self.brew_result = Some("Running\u{2026}".to_owned());
                ctx.notify();

                let brew_job = BrewJob::new(config.root);
                let _ = ctx.spawn(
                    async move { brew_job.run().await },
                    |me, result, ctx| {
                        me.brew_running = false;
                        me.brew_result = Some(match result {
                            Ok(r) if r.errors.is_empty() => {
                                format!("Done \u{2014} {} formulae written.", r.written.len())
                            }
                            Ok(r) => format!(
                                "Done \u{2014} {} formulae written ({} errors).",
                                r.written.len(),
                                r.errors.len()
                            ),
                            Err(BrewError::NotInstalled) => {
                                "Homebrew is not installed.".to_owned()
                            }
                            Err(e) => format!("Error: {e}"),
                        });
                        ctx.notify();
                    },
                );
            }
        }
    }
}

impl View for VaultSettingsPageView {
    fn ui_name() -> &'static str {
        "VaultSettingsPage"
    }

    fn on_focus(&mut self, _focus_ctx: &FocusContext, _ctx: &mut ViewContext<Self>) {}

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for VaultSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Vault
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id);
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<VaultSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<VaultSettingsPageView>) -> Self {
        SettingsPageViewHandle::VaultSettings(view_handle)
    }
}

// ── Widget ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct VaultSettingsWidget {
    button_state: MouseStateHandle,
}

impl SettingsWidget for VaultSettingsWidget {
    type View = VaultSettingsPageView;

    fn search_terms(&self) -> &str {
        "vault brew homebrew docs update formulae"
    }

    fn render(
        &self,
        view: &VaultSettingsPageView,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let ui_builder = appearance.ui_builder();

        let label = if view.brew_running {
            "Updating\u{2026}"
        } else {
            "Update brew docs"
        };

        let variant = if view.brew_running {
            ButtonVariant::Text
        } else {
            ButtonVariant::Accent
        };

        let button_hoverable = ui_builder
            .button(variant, self.button_state.clone())
            .with_text_label(label.to_string())
            .build();

        let button = if view.brew_running {
            button_hoverable.finish()
        } else {
            button_hoverable
                .on_click(|ctx: &mut EventContext, _, _| {
                    ctx.dispatch_typed_action(VaultSettingsPageAction::UpdateBrewDocs);
                })
                .finish()
        };

        let mut col = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(button);

        if let Some(result) = &view.brew_result {
            let theme = appearance.theme();
            let status = Text::new_inline(
                result.clone(),
                appearance.ui_font_family(),
                13.,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish();
            col = col.with_child(Container::new(status).with_margin_top(8.).finish());
        }

        col.finish()
    }
}
