//! Vault explorer panel.
//!
//! Subscribes to [`VaultManager`] and rebuilds an in-memory tree on each
//! `StateChanged` event. v1 renders the full vault flat (no collapse /
//! expand); B-3 will hook a filesystem watcher and add click-to-open.
//!
//! When the manager is `Uninitialized`, the panel offers a "Create new
//! vault" affordance — clicking it dispatches `VaultPanelAction::CreateNew`
//! which builds a `VaultConfig` from `[unwarp.vault]` settings and calls
//! `VaultManager::create_new`. This is a stop-gap until the proper
//! workspace first-run banner lands.

use std::path::PathBuf;
use std::time::Duration;

use warpui::{
    elements::{
        Container, CrossAxisAlignment, Element, Flex, Hoverable, MainAxisSize, MouseStateHandle,
        ParentElement, Text,
    },
    platform::Cursor,
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext,
};

use crate::appearance::Appearance;
use crate::settings::UnwarpSettings;
use crate::vault::manager::{VaultManager, VaultManagerEvent, VaultState};

use super::tree::{walk_vault, VaultEntry, VaultEntryKind};

/// How often the tree polls the filesystem for changes when the panel
/// is open. Cheap (~ms for a vault-sized walk) so the interval can be
/// short without measurable cost. Promoting to notify-based watching is
/// a separate follow-up.
const REFRESH_INTERVAL_SECS: u64 = 2;

const PANEL_HORIZONTAL_PADDING: f32 = 12.0;
const PANEL_VERTICAL_PADDING: f32 = 12.0;
const HEADING_FONT_SIZE: f32 = 13.0;
const BODY_FONT_SIZE: f32 = 12.0;
const ROW_FONT_SIZE: f32 = 13.0;
const ROW_VERTICAL_SPACING: f32 = 3.0;
const ROW_HORIZONTAL_PADDING: f32 = 6.0;
const INDENT_PER_DEPTH: f32 = 14.0;
const SECTION_SPACING: f32 = 8.0;
const CTA_VERTICAL_PADDING: f32 = 6.0;

#[derive(Clone, Debug)]
pub enum VaultPanelAction {
    /// Initialize a fresh vault at the path stored in settings (default:
    /// `~/Documents/unwarp-vault/`). Stop-gap until the proper first-run
    /// banner lands.
    CreateNew,
    /// User clicked an entry row. Directory clicks are no-ops in v1
    /// (no collapse/expand); file clicks emit `VaultPanelEvent::OpenFile`
    /// for the workspace to route to the appropriate viewer.
    OpenEntry(PathBuf),
}

/// Events VaultPanel emits to its parent (the LeftPanelView).
#[derive(Clone, Debug)]
pub enum VaultPanelEvent {
    /// User clicked a file row. The path is absolute. The workspace
    /// decides which viewer/editor to open based on file type.
    OpenFile { path: PathBuf },
}

pub struct VaultPanel {
    vault: ModelHandle<VaultManager>,
    /// Cached tree contents. Rebuilt on every `StateChanged` to `Ready`
    /// and on each periodic poll. Empty when state != Ready.
    entries: Vec<VaultEntry>,
    cta_mouse_state: MouseStateHandle,
}

impl VaultPanel {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let vault = VaultManager::handle(ctx);
        // Populate the tree if the manager is already Ready by the time the
        // panel is constructed (e.g., workspace pre-initialized it).
        let entries = if vault.as_ref(ctx).is_ready() {
            if let Some(root) = vault.as_ref(ctx).vault_root() {
                walk_vault(root)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        ctx.subscribe_to_model(&vault, |me, _, event, ctx| match event {
            VaultManagerEvent::StateChanged { new } => {
                if matches!(new, VaultState::Ready) {
                    if let Some(root) = me.vault.as_ref(ctx).vault_root() {
                        me.entries = walk_vault(root);
                    }
                } else {
                    me.entries.clear();
                }
                ctx.notify();
            }
        });
        let panel = Self {
            vault,
            entries,
            cta_mouse_state: MouseStateHandle::default(),
        };
        panel.schedule_refresh(ctx);
        panel
    }

    /// Polling loop. Runs forever; on each tick re-walks the vault if the
    /// manager is Ready and updates state when the entries change.
    fn schedule_refresh(&self, ctx: &mut ViewContext<Self>) {
        let _ = ctx.spawn(
            async move {
                tokio::time::sleep(Duration::from_secs(REFRESH_INTERVAL_SECS)).await;
            },
            |panel, _, ctx| {
                if panel.vault.as_ref(ctx).is_ready() {
                    if let Some(root) = panel.vault.as_ref(ctx).vault_root() {
                        let next = walk_vault(root);
                        if next != panel.entries {
                            panel.entries = next;
                            ctx.notify();
                        }
                    }
                }
                panel.schedule_refresh(ctx);
            },
        );
    }

    fn try_create_new(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(home_dir) = dirs::home_dir() else {
            log::warn!("vault explorer: home directory not available; cannot create vault");
            return;
        };
        let config = match UnwarpSettings::vault_config(ctx, &home_dir) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("vault explorer: invalid vault config in settings: {e}");
                return;
            }
        };
        // `initialize` routes to create_new for unrecognized paths and
        // adopt_existing for recognized ones, so this affordance works
        // whether the path is fresh or already a vault.
        self.vault.update(ctx, |m, ctx| {
            if let Err(e) = m.initialize(config, ctx) {
                log::warn!("vault explorer: initialization failed: {e}");
            }
        });
    }
}

impl Entity for VaultPanel {
    type Event = VaultPanelEvent;
}

impl TypedActionView for VaultPanel {
    type Action = VaultPanelAction;

    fn handle_action(&mut self, action: &VaultPanelAction, ctx: &mut ViewContext<Self>) {
        match action {
            VaultPanelAction::CreateNew => self.try_create_new(ctx),
            VaultPanelAction::OpenEntry(path) => self.open_entry(path.clone(), ctx),
        }
    }
}

impl VaultPanel {
    fn open_entry(&mut self, path: PathBuf, ctx: &mut ViewContext<Self>) {
        // Directory clicks are a no-op in v1; expansion state would live
        // here in a follow-up. Only files emit OpenFile.
        if let Some(entry) = self.entries.iter().find(|e| e.path == path) {
            if matches!(entry.kind, VaultEntryKind::Directory) {
                return;
            }
        }
        ctx.emit(VaultPanelEvent::OpenFile { path });
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

        let body = match manager.state() {
            VaultState::Uninitialized => self.render_uninitialized(appearance),
            VaultState::Ready => self.render_tree(appearance, app),
            VaultState::Locked { by_pid } => self.render_message(
                "Vault locked",
                &format!(
                    "Another unwarp instance (pid {by_pid}) owns this vault. Quit it to take over."
                ),
                appearance,
            ),
            VaultState::Error(msg) => self.render_message("Vault error", msg, appearance),
        };

        Container::new(body)
            .with_padding_left(PANEL_HORIZONTAL_PADDING)
            .with_padding_right(PANEL_HORIZONTAL_PADDING)
            .with_padding_top(PANEL_VERTICAL_PADDING)
            .with_padding_bottom(PANEL_VERTICAL_PADDING)
            .finish()
    }
}

impl VaultPanel {
    fn render_uninitialized(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();

        let heading = Text::new_inline(
            "Vault not set up".to_owned(),
            appearance.ui_font_family(),
            HEADING_FONT_SIZE,
        )
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();

        let body = Text::new_inline(
            "Create a vault at the path in [unwarp.vault.path] (default ~/Documents/unwarp-vault/).".to_owned(),
            appearance.ui_font_family(),
            BODY_FONT_SIZE,
        )
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();

        let cta_label = Text::new_inline(
            "Create new vault".to_owned(),
            appearance.ui_font_family(),
            BODY_FONT_SIZE,
        )
        .with_color(theme.accent().into())
        .finish();
        let cta = Hoverable::new(self.cta_mouse_state.clone(), |_| {
            Container::new(cta_label)
                .with_padding_top(CTA_VERTICAL_PADDING)
                .with_padding_bottom(CTA_VERTICAL_PADDING)
                .finish()
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(VaultPanelAction::CreateNew);
        })
        .with_cursor(Cursor::PointingHand)
        .finish();

        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(heading)
            .with_child(
                Container::new(body)
                    .with_padding_top(SECTION_SPACING)
                    .finish(),
            )
            .with_child(cta)
            .finish()
    }

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

        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(heading_text)
            .with_child(
                Container::new(body_text)
                    .with_padding_top(SECTION_SPACING)
                    .finish(),
            )
            .finish()
    }

    fn render_tree(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        if self.entries.is_empty() {
            return self.render_message(
                "Vault is empty",
                "No files in the vault yet. Run the brew docs job or drop a markdown file in ~/Documents/unwarp-vault/notes/.",
                appearance,
            );
        }

        let mut col = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min);

        let vault_root_label = self
            .vault
            .as_ref(app)
            .vault_root()
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "Vault".to_owned());
        let heading = Text::new_inline(
            vault_root_label,
            appearance.ui_font_family(),
            HEADING_FONT_SIZE,
        )
        .with_color(
            appearance
                .theme()
                .sub_text_color(appearance.theme().background())
                .into(),
        )
        .finish();
        col = col.with_child(
            Container::new(heading)
                .with_padding_bottom(SECTION_SPACING)
                .finish(),
        );

        for entry in &self.entries {
            col = col.with_child(self.render_row(entry, appearance));
        }
        col.finish()
    }

    fn render_row(&self, entry: &VaultEntry, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let indent = entry.depth as f32 * INDENT_PER_DEPTH;
        let prefix = match entry.kind {
            VaultEntryKind::Directory => "▸ ",
            VaultEntryKind::File => "  ",
        };
        let suffix = if entry.is_read_only { " 🔒" } else { "" };

        let label = format!("{prefix}{}{suffix}", entry.name);
        let label_color = if entry.is_read_only {
            theme.sub_text_color(theme.background())
        } else {
            theme.main_text_color(theme.background())
        };

        let text = Text::new_inline(label, appearance.ui_font_family(), ROW_FONT_SIZE)
            .with_color(label_color.into())
            .finish();

        let body = Container::new(text)
            .with_padding_left(ROW_HORIZONTAL_PADDING + indent)
            .with_padding_right(ROW_HORIZONTAL_PADDING)
            .with_padding_top(ROW_VERTICAL_SPACING)
            .with_padding_bottom(ROW_VERTICAL_SPACING)
            .finish();

        // File rows are clickable; directory rows are not (no expand state
        // in v1 — clicking does nothing). Use the entry's full path as the
        // action payload.
        let click_path = entry.path.clone();
        let is_clickable = matches!(entry.kind, VaultEntryKind::File);
        let mouse_state = MouseStateHandle::default();
        let hoverable = Hoverable::new(mouse_state, move |_| body).with_cursor(if is_clickable {
            Cursor::PointingHand
        } else {
            Cursor::Arrow
        });

        if is_clickable {
            hoverable
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(VaultPanelAction::OpenEntry(click_path.clone()));
                })
                .finish()
        } else {
            hoverable.finish()
        }
    }

}
