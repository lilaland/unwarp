//! Vault explorer panel.
//!
//! Subscribes to [`VaultManager`] and rebuilds an in-memory tree on each
//! `StateChanged` event.
//!
//! When the manager is `Uninitialized`, the panel offers a "Create new
//! vault" affordance. When `Ready`, the panel renders the file tree and a
//! "Run jobs" button that triggers both the mirror and brew jobs on demand.
//!
//! Background mirror scans also run automatically every [`JOB_SCAN_INTERVAL_SECS`]
//! seconds once the vault transitions to `Ready`. Brew runs are manual-only
//! (they shell out to `brew info` which takes a few seconds).

use std::io;
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
use crate::vault::brew::job::{BrewError, BrewJob, BrewJobReport};
use crate::vault::manager::{VaultManager, VaultManagerEvent, VaultState};
use crate::vault::mirror::job::{MirrorError, MirrorJob, MirrorJobReport};

use super::tree::{walk_vault, VaultEntry, VaultEntryKind};

/// How often the tree polls the filesystem for changes when the panel
/// is open.
const REFRESH_INTERVAL_SECS: u64 = 2;
/// Interval between automatic background mirror scans.
const JOB_SCAN_INTERVAL_SECS: u64 = 300; // 5 minutes

const PANEL_HORIZONTAL_PADDING: f32 = 12.0;
const PANEL_VERTICAL_PADDING: f32 = 12.0;
const HEADING_FONT_SIZE: f32 = 13.0;
const BODY_FONT_SIZE: f32 = 12.0;
const STATUS_FONT_SIZE: f32 = 11.0;
const ROW_FONT_SIZE: f32 = 13.0;
const ROW_VERTICAL_SPACING: f32 = 3.0;
const ROW_HORIZONTAL_PADDING: f32 = 6.0;
const INDENT_PER_DEPTH: f32 = 14.0;
const SECTION_SPACING: f32 = 8.0;
const CTA_VERTICAL_PADDING: f32 = 6.0;

#[derive(Clone, Debug)]
pub enum VaultPanelAction {
    /// Initialize a fresh vault at the path stored in settings.
    CreateNew,
    /// User clicked an entry row. Directory clicks are no-ops; file clicks
    /// emit `VaultPanelEvent::OpenFile` for the workspace to route.
    OpenEntry(PathBuf),
    /// User clicked "Run jobs" — triggers MirrorJob + BrewJob in the
    /// background. Disabled while already running.
    RunJobs,
}

/// Events VaultPanel emits to its parent (the LeftPanelView).
#[derive(Clone, Debug)]
pub enum VaultPanelEvent {
    /// User clicked a file row. `is_read_only` is derived from [`VaultCategory`]
    /// and tells the workspace whether to open the file in the code editor
    /// (editable) or the markdown viewer (read-only).
    OpenFile { path: PathBuf, is_read_only: bool },
}

pub struct VaultPanel {
    vault: ModelHandle<VaultManager>,
    /// Cached tree contents. Rebuilt on every `StateChanged` to `Ready`
    /// and on each 2s poll.
    entries: Vec<VaultEntry>,
    cta_mouse_state: MouseStateHandle,
    run_jobs_button_state: MouseStateHandle,
    /// True while the manual "Run jobs" pair is in flight.
    jobs_running: bool,
    /// Short summary shown beneath the button after a manual run completes.
    last_run_summary: Option<String>,
}

impl VaultPanel {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let vault = VaultManager::handle(ctx);
        let entries = if vault.as_ref(ctx).is_ready() {
            if let Some(root) = vault.as_ref(ctx).vault_root() {
                walk_vault(root)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let already_ready = vault.as_ref(ctx).is_ready();

        ctx.subscribe_to_model(&vault, |me, _, event, ctx| match event {
            VaultManagerEvent::StateChanged { new } => {
                if matches!(new, VaultState::Ready) {
                    if let Some(root) = me.vault.as_ref(ctx).vault_root() {
                        me.entries = walk_vault(root);
                    }
                    // Start the periodic background mirror scanner now that
                    // the vault is ready. If the user re-initializes the vault
                    // later this starts a second loop, but MirrorJob is
                    // idempotent so the extra scans are harmless.
                    me.schedule_mirror_scan(ctx);
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
            run_jobs_button_state: MouseStateHandle::default(),
            jobs_running: false,
            last_run_summary: None,
        };
        panel.schedule_refresh(ctx);
        if already_ready {
            panel.schedule_mirror_scan(ctx);
        }
        panel
    }

    // ── Polling loops ─────────────────────────────────────────────────────────

    /// 2-second tree refresh loop.
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

    /// 5-minute mirror-only background scan loop. Starts automatically when
    /// the vault becomes `Ready`.
    fn schedule_mirror_scan(&self, ctx: &mut ViewContext<Self>) {
        let _ = ctx.spawn(
            async move {
                tokio::time::sleep(Duration::from_secs(JOB_SCAN_INTERVAL_SECS)).await;
            },
            |panel, _, ctx| {
                panel.run_silent_mirror(ctx);
                panel.schedule_mirror_scan(ctx);
            },
        );
    }

    // ── Job runners ───────────────────────────────────────────────────────────

    /// Silent background mirror scan (no UI state update).
    fn run_silent_mirror(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(config) = self.vault.as_ref(ctx).config().cloned() else {
            return;
        };
        let mirror_job = MirrorJob::new(
            config.root.clone(),
            config.mirror_source_root.clone(),
            config.mirror_max_depth,
        );
        let _ = ctx.spawn(
            async move {
                let _ =
                    tokio::task::spawn_blocking(move || mirror_job.scan_once()).await;
            },
            |_, _, _| {},
        );
    }

    /// Manual "Run jobs" — both MirrorJob (blocking) and BrewJob (async).
    /// Updates `jobs_running` and `last_run_summary` when done.
    fn run_jobs(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(config) = self.vault.as_ref(ctx).config().cloned() else {
            return;
        };
        self.jobs_running = true;
        self.last_run_summary = Some("Running\u{2026}".to_owned());
        ctx.notify();

        let vault_root = config.root.clone();
        let mirror_job = MirrorJob::new(
            vault_root.clone(),
            config.mirror_source_root.clone(),
            config.mirror_max_depth,
        );
        let brew_job = BrewJob::new(vault_root);

        let _ = ctx.spawn(
            async move {
                let mirror = tokio::task::spawn_blocking(move || mirror_job.scan_once())
                    .await
                    .unwrap_or_else(|e| {
                        Err(MirrorError::Io {
                            path: "<task>".to_owned(),
                            source: io::Error::new(io::ErrorKind::Other, e.to_string()),
                        })
                    });
                let brew = brew_job.run().await;
                (mirror, brew)
            },
            |panel, (mirror_result, brew_result), ctx| {
                panel.jobs_running = false;
                panel.last_run_summary =
                    Some(format_run_summary(&mirror_result, &brew_result));
                ctx.notify();
            },
        );
    }

    // ── Vault creation ────────────────────────────────────────────────────────

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
        self.vault.update(ctx, |m, ctx| {
            if let Err(e) = m.initialize(config, ctx) {
                log::warn!("vault explorer: initialization failed: {e}");
            }
        });
    }

    // ── File open ─────────────────────────────────────────────────────────────

    fn open_entry(&mut self, path: PathBuf, ctx: &mut ViewContext<Self>) {
        if let Some(entry) = self.entries.iter().find(|e| e.path == path) {
            if matches!(entry.kind, VaultEntryKind::Directory) {
                return;
            }
            let is_read_only = entry.is_read_only;
            ctx.emit(VaultPanelEvent::OpenFile { path, is_read_only });
        }
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
            VaultPanelAction::RunJobs => {
                if !self.jobs_running {
                    self.run_jobs(ctx);
                }
            }
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

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
        let mut col = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min);

        col = col.with_child(self.render_jobs_header(appearance, app));

        if self.entries.is_empty() {
            let empty_msg = Text::new_inline(
                "No files yet. Add markdown files or click \"Run jobs\".".to_owned(),
                appearance.ui_font_family(),
                BODY_FONT_SIZE,
            )
            .with_color(
                appearance
                    .theme()
                    .sub_text_color(appearance.theme().background())
                    .into(),
            )
            .finish();
            col = col.with_child(empty_msg);
        } else {
            for entry in &self.entries {
                col = col.with_child(self.render_row(entry, appearance));
            }
        }

        col.finish()
    }

    /// Header row: vault name, "Run jobs" button, and optional last-run status.
    fn render_jobs_header(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let theme = appearance.theme();

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
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();

        // "Run jobs" button — greyed out and non-interactive while running.
        let jobs_running = self.jobs_running;
        let button_label = if jobs_running {
            "Running\u{2026}"
        } else {
            "Run jobs"
        };
        let button_color = if jobs_running {
            theme.sub_text_color(theme.background())
        } else {
            theme.accent()
        };
        let button_text = Text::new_inline(
            button_label.to_owned(),
            appearance.ui_font_family(),
            BODY_FONT_SIZE,
        )
        .with_color(button_color.into())
        .finish();

        let button_body = Container::new(button_text)
            .with_padding_top(CTA_VERTICAL_PADDING)
            .with_padding_bottom(CTA_VERTICAL_PADDING)
            .finish();

        let hoverable =
            Hoverable::new(self.run_jobs_button_state.clone(), move |_| button_body);
        let button = if jobs_running {
            hoverable.with_cursor(Cursor::Arrow).finish()
        } else {
            hoverable
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(VaultPanelAction::RunJobs);
                })
                .with_cursor(Cursor::PointingHand)
                .finish()
        };

        let mut header = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(Container::new(heading).with_padding_bottom(4.0).finish())
            .with_child(button);

        if let Some(summary) = &self.last_run_summary {
            let status = Text::new_inline(
                summary.clone(),
                appearance.ui_font_family(),
                STATUS_FONT_SIZE,
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish();
            header = header.with_child(status);
        }

        Container::new(header.finish())
            .with_padding_bottom(SECTION_SPACING)
            .finish()
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_run_summary(
    mirror: &Result<MirrorJobReport, MirrorError>,
    brew: &Result<BrewJobReport, BrewError>,
) -> String {
    let mirror_part = match mirror {
        Ok(r) if r.errors.is_empty() => format!("Mirror: {} written", r.written.len()),
        Ok(r) => format!(
            "Mirror: {} written, {} errors",
            r.written.len(),
            r.errors.len()
        ),
        Err(e) => format!("Mirror error: {e}"),
    };
    let brew_part = match brew {
        Ok(r) if r.errors.is_empty() => format!("Brew: {} written", r.written.len()),
        Ok(r) => format!(
            "Brew: {} written, {} errors",
            r.written.len(),
            r.errors.len()
        ),
        Err(BrewError::NotInstalled) => "Brew: not installed".to_owned(),
        Err(e) => format!("Brew error: {e}"),
    };
    format!("{mirror_part} \u{00b7} {brew_part}")
}
