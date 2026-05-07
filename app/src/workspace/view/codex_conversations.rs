#![cfg(not(target_family = "wasm"))]

use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use settings::Setting as _;
use warpui::{
    elements::{
        Border, ChildView, Clipped, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
        Element, Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, Padding,
        ParentElement, Radius, Shrinkable, Text,
    },
    fonts::{Properties, Weight},
    platform::Cursor,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
    },
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

use crate::{
    ai::active_agent_views_model::ActiveAgentViewsModel,
    appearance::Appearance,
    codex_app_server::{
        codex_start_command, codex_thread_updated_at_utc, CodexAppServerModel,
        CodexAppServerModelEvent, CodexAppServerStatus, CodexApprovalDecision,
        CodexPendingApproval, CodexThreadSummary,
    },
    editor::{
        EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions,
        TextOptions,
    },
    settings::CodexAppServerSettings,
    ui_components::icons::Icon,
    util::time_format::format_approx_duration_from_now_utc,
    workspace::{RestoreConversationLayout, WorkspaceAction},
};

#[derive(Clone, Debug)]
pub enum CodexConversationsAction {
    Refresh,
    NewConversation,
    OpenThread(String),
    ResolveApproval(CodexApprovalDecision),
}

pub struct CodexConversationsView {
    model: ModelHandle<CodexAppServerModel>,
    query_editor: ViewHandle<EditorView>,
    refresh_button: MouseStateHandle,
    new_conversation_button: MouseStateHandle,
    thread_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    approval_buttons: RefCell<HashMap<CodexApprovalDecision, MouseStateHandle>>,
}

impl CodexConversationsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let model = CodexAppServerModel::handle(ctx);
        ctx.subscribe_to_model(&model, |_, _, event, ctx| match event {
            CodexAppServerModelEvent::OpenConversation {
                conversation_id, ..
            } => {
                if let Some(terminal_view_id) = ActiveAgentViewsModel::as_ref(ctx)
                    .get_terminal_view_id_for_conversation(*conversation_id, ctx)
                {
                    ctx.dispatch_typed_action(&WorkspaceAction::FocusTerminalViewInWorkspace {
                        terminal_view_id,
                    });
                } else {
                    ctx.dispatch_typed_action(&WorkspaceAction::RestoreOrNavigateToConversation {
                        conversation_id: *conversation_id,
                        window_id: None,
                        pane_view_locator: None,
                        terminal_view_id: None,
                        restore_layout: Some(RestoreConversationLayout::SplitPane),
                    });
                }
                ctx.notify();
            }
            CodexAppServerModelEvent::StatusChanged
            | CodexAppServerModelEvent::ThreadsChanged
            | CodexAppServerModelEvent::ActiveThreadChanged
            | CodexAppServerModelEvent::ModelsChanged => {
                ctx.notify();
            }
        });

        let query_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(Some(13.), appearance),
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::Always,
                    select_all_on_focus: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Search Codex conversations", ctx);
            editor
        });
        ctx.subscribe_to_view(&query_editor, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                ctx.notify();
            }
        });

        Self {
            model,
            query_editor,
            refresh_button: Default::default(),
            new_conversation_button: Default::default(),
            thread_buttons: Default::default(),
            approval_buttons: Default::default(),
        }
    }

    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.query_editor);
    }

    fn render_status(&self, app: &AppContext, appearance: &Appearance) -> Box<dyn Element> {
        let settings = CodexAppServerSettings::as_ref(app);
        let status = self.model.as_ref(app).status().clone();
        let status_text = match &status {
            CodexAppServerStatus::Disconnected { message } => format!("Disconnected: {message}"),
            _ => status.label(),
        };
        let mut column = Flex::column().with_spacing(8.).with_child(
            Text::new_inline(
                status_text,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 1.,
            )
            .with_color(
                appearance
                    .theme()
                    .sub_text_color(appearance.theme().background())
                    .into(),
            )
            .finish(),
        );

        if matches!(status, CodexAppServerStatus::Disconnected { .. }) {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text(
                        format!(
                            "Start server: {}",
                            codex_start_command(settings.server_url.value())
                        ),
                        true,
                    )
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(
                            appearance
                                .theme()
                                .sub_text_color(appearance.theme().background())
                                .into_solid(),
                        ),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            );
        }

        column.finish()
    }

    fn render_search(&self, appearance: &Appearance) -> Box<dyn Element> {
        Container::new(
            ConstrainedBox::new(Clipped::new(ChildView::new(&self.query_editor).finish()).finish())
                .with_height(28.)
                .finish(),
        )
        .with_background_color(appearance.theme().surface_2().into())
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
        .with_uniform_padding(6.)
        .finish()
    }

    fn render_refresh_button(&self, appearance: &Appearance) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, self.refresh_button.clone())
            .with_text_label("Refresh".to_string())
            .with_style(UiComponentStyles {
                padding: Some(Coords::default().top(6.).bottom(6.).left(12.).right(12.)),
                ..Default::default()
            })
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(CodexConversationsAction::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn render_new_conversation_button(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mouse_state = self.new_conversation_button.clone();

        Hoverable::new(mouse_state, move |state| {
            let label = Text::new_inline(
                "New conversation",
                appearance.ui_font_family(),
                appearance.ui_font_size() + 1.,
            )
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();

            let icon = ConstrainedBox::new(
                Icon::Plus
                    .to_warpui_icon(theme.main_text_color(theme.background()))
                    .finish(),
            )
            .with_width(appearance.ui_font_size())
            .with_height(appearance.ui_font_size())
            .finish();

            let row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(icon)
                .with_child(label)
                .finish();

            let mut container = Container::new(row)
                .with_padding(Padding::uniform(0.).with_left(12.).with_right(12.))
                .with_border(Border::all(1.).with_border_fill(theme.outline()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
            if state.is_hovered() {
                container = container.with_background(theme.surface_2());
            }

            ConstrainedBox::new(container.finish())
                .with_min_height(34.)
                .finish()
        })
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(CodexConversationsAction::NewConversation);
        })
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn render_pending_approval(
        &self,
        approval: &CodexPendingApproval,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut column = Flex::column().with_spacing(8.).with_child(
            Text::new_inline(
                "Codex needs approval".to_string(),
                appearance.ui_font_family(),
                appearance.ui_font_size() + 1.,
            )
            .with_style(Properties::default().weight(Weight::Bold))
            .with_color(theme.main_text_color(theme.background()).into())
            .finish(),
        );

        column.add_child(
            appearance
                .ui_builder()
                .wrappable_text(approval.reason.clone(), true)
                .with_style(UiComponentStyles {
                    font_size: Some(12.),
                    font_color: Some(theme.sub_text_color(theme.background()).into_solid()),
                    ..Default::default()
                })
                .build()
                .finish(),
        );

        if let Some(command) = approval.command.as_ref() {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text(command.clone(), true)
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(theme.main_text_color(theme.background()).into_solid()),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            );
        }

        if let Some(cwd) = approval.cwd.as_ref() {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text(cwd.clone(), true)
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(theme.sub_text_color(theme.background()).into_solid()),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            );
        }

        let mut button_row = Flex::row().with_spacing(8.);
        let mut buttons = self.approval_buttons.borrow_mut();
        for decision in &approval.available_decisions {
            let mouse_state = buttons.entry(*decision).or_default().clone();
            let variant = if *decision == CodexApprovalDecision::Accept {
                ButtonVariant::Accent
            } else {
                ButtonVariant::Secondary
            };
            let action = CodexConversationsAction::ResolveApproval(*decision);
            button_row.add_child(
                appearance
                    .ui_builder()
                    .button(variant, mouse_state)
                    .with_text_label(decision.label().to_string())
                    .with_style(UiComponentStyles {
                        padding: Some(Coords::default().top(6.).bottom(6.).left(10.).right(10.)),
                        ..Default::default()
                    })
                    .build()
                    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
                    .with_cursor(Cursor::PointingHand)
                    .finish(),
            );
        }
        column.add_child(button_row.finish());

        Container::new(column.finish())
            .with_background_color(theme.surface_2().into())
            .with_border(Border::all(1.).with_border_fill(theme.outline()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_uniform_padding(10.)
            .finish()
    }

    fn render_thread(
        &self,
        thread: &CodexThreadSummary,
        is_active: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut buttons = self.thread_buttons.borrow_mut();
        let mouse_state = buttons.entry(thread.id.clone()).or_default().clone();
        let action = CodexConversationsAction::OpenThread(thread.id.clone());
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let title = if thread.title.trim().is_empty() {
            thread.id.as_str()
        } else {
            thread.title.as_str()
        };
        let metadata = thread
            .cwd
            .as_ref()
            .and_then(|cwd| cwd.to_str())
            .map(shorten_project_path);
        let age = codex_thread_updated_at_utc(thread).map(format_approx_duration_from_now_utc);

        let title = Text::new_inline(title.to_string(), font_family, font_size + 2.)
            .with_style(Properties::default().weight(Weight::Bold))
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();
        let mut bottom_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::End);
        if let Some(metadata) = metadata {
            bottom_row.add_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(metadata, font_family, font_size - 1.)
                        .with_color(theme.sub_text_color(theme.background()).into())
                        .finish(),
                )
                .finish(),
            );
        }
        if let Some(age) = age {
            bottom_row.add_child(
                Text::new_inline(age, font_family, font_size - 2.)
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
            );
        }

        let row = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(title)
            .with_child(bottom_row.finish())
            .finish();

        Hoverable::new(mouse_state, move |_| {
            let container = Container::new(row)
                .with_horizontal_padding(12.)
                .with_padding_top(8.)
                .with_padding_bottom(8.);
            let container = if is_active {
                container.with_background(theme.surface_overlay_1())
            } else {
                container
            };
            container.finish()
        })
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn visible_threads<'a>(&self, app: &'a AppContext) -> Vec<&'a CodexThreadSummary> {
        let query = self
            .query_editor
            .as_ref(app)
            .buffer_text(app)
            .trim()
            .to_ascii_lowercase();
        self.model
            .as_ref(app)
            .threads()
            .iter()
            .filter(|thread| {
                query.is_empty()
                    || thread.title.to_ascii_lowercase().contains(&query)
                    || thread.id.to_ascii_lowercase().contains(&query)
                    || thread
                        .cwd
                        .as_ref()
                        .and_then(|cwd| cwd.to_str())
                        .is_some_and(|cwd| cwd.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    fn render_threads(&self, app: &AppContext, appearance: &Appearance) -> Box<dyn Element> {
        let active_thread_id = self
            .model
            .as_ref(app)
            .opening_thread_id()
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.model
                    .as_ref(app)
                    .active_thread()
                    .map(|thread| thread.summary.id.clone())
            });
        let mut column = Flex::column().with_spacing(4.);
        let threads = self.visible_threads(app);
        if threads.is_empty() {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text("No Codex conversations found for this project.", true)
                    .build()
                    .finish(),
            );
        } else {
            for thread in threads {
                column.add_child(self.render_thread(
                    thread,
                    active_thread_id.as_deref() == Some(thread.id.as_str()),
                    appearance,
                ));
            }
        }
        column.finish()
    }

}

fn shorten_project_path(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.to_string())
}

impl Entity for CodexConversationsView {
    type Event = ();
}

impl TypedActionView for CodexConversationsView {
    type Action = CodexConversationsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CodexConversationsAction::Refresh => {
                self.model.update(ctx, |model, ctx| model.refresh(ctx));
            }
            CodexConversationsAction::NewConversation => {
                self.model
                    .update(ctx, |model, ctx| model.start_new_conversation(ctx));
            }
            CodexConversationsAction::OpenThread(thread_id) => {
                self.model.update(ctx, |model, ctx| {
                    model.open_thread_as_conversation(thread_id.clone(), ctx);
                });
            }
            CodexConversationsAction::ResolveApproval(decision) => {
                self.model
                    .update(ctx, |model, ctx| model.resolve_pending_approval(*decision, ctx));
            }
        }
    }
}

impl View for CodexConversationsView {
    fn ui_name() -> &'static str {
        "CodexConversationsView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.query_editor);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let column = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(10.)
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., self.render_status(app, appearance)).finish())
                    .with_child(self.render_refresh_button(appearance))
                    .finish(),
            )
            .with_child(self.render_search(appearance))
            .with_child(self.render_new_conversation_button(app))
            .with_child(self.render_threads(app, appearance));
        let column = if let Some(approval) = self.model.as_ref(app).pending_approval() {
            column.with_child(self.render_pending_approval(approval, appearance))
        } else {
            column
        };
        let column = column.finish();

        Container::new(column).with_uniform_padding(10.).finish()
    }
}
