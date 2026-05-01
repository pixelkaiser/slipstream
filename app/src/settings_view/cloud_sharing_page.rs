use super::{
    editor_text_colors,
    settings_page::{
        MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
        SettingsWidget, CONTENT_FONT_SIZE, SUBHEADER_FONT_SIZE,
    },
    SettingsSection,
};
use crate::{
    appearance::Appearance,
    editor::{
        EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions,
        TextOptions,
    },
    terminal::shared_session::settings::{
        apply_session_sharing_server_url, normalize_session_sharing_server_url,
        SharedSessionSettings, SESSION_SHARING_SERVER_URL_PLACEHOLDER,
    },
    view_components::DismissibleToast,
    ToastStack,
};
use settings::Setting;
use warp_core::channel::ChannelState;
use warpui::{
    elements::{
        Border, ChildView, Clipped, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex,
        MouseStateHandle, ParentElement, Text,
    },
    fonts::{Properties, Weight},
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
    },
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

#[derive(Clone, Debug, PartialEq)]
pub enum CloudSharingPageAction {
    SaveRelayUrl,
    ClearRelayUrl,
}

pub struct CloudSharingPageView {
    page: PageType<Self>,
    relay_url_editor: ViewHandle<EditorView>,
    validation_message: Option<String>,
}

impl CloudSharingPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&Appearance::handle(ctx), |view, _, _, ctx| {
            view.update_editor_text_colors(ctx);
        });

        let relay_url_editor = Self::create_relay_url_editor(ctx);
        relay_url_editor.update(ctx, |editor, ctx| {
            let configured_url = SharedSessionSettings::as_ref(ctx)
                .configured_session_sharing_server_url()
                .map(str::to_owned);
            if let Some(url) = configured_url {
                editor.set_buffer_text(&url, ctx);
            }
        });
        ctx.subscribe_to_view(&relay_url_editor, |view, _, event, ctx| {
            view.handle_relay_url_editor_event(event, ctx);
        });

        Self {
            page: PageType::new_monolith(CloudSharingPageWidget::default(), Some("Sharing"), true),
            relay_url_editor,
            validation_message: None,
        }
    }

    fn create_relay_url_editor(ctx: &mut ViewContext<Self>) -> ViewHandle<EditorView> {
        ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    text_colors_override: Some(editor_text_colors(appearance)),
                    ..Default::default()
                },
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(SESSION_SHARING_SERVER_URL_PLACEHOLDER, ctx);
            editor
        })
    }

    fn update_editor_text_colors(&mut self, ctx: &mut ViewContext<Self>) {
        let appearance = Appearance::as_ref(ctx);
        let text_colors = editor_text_colors(appearance);
        self.relay_url_editor.update(ctx, |editor, ctx| {
            editor.set_text_colors(text_colors, ctx);
        });
    }

    fn handle_relay_url_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Edited(_) => {
                self.validation_message = None;
                ctx.notify();
            }
            EditorEvent::Enter => self.save_relay_url(ctx),
            EditorEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
            _ => {}
        }
    }

    fn save_relay_url(&mut self, ctx: &mut ViewContext<Self>) {
        let raw_url = self.relay_url_editor.as_ref(ctx).buffer_text(ctx);
        let normalized = match normalize_session_sharing_server_url(&raw_url) {
            Ok(normalized) => normalized,
            Err(err) => {
                self.validation_message = Some(err.to_string());
                ctx.notify();
                return;
            }
        };

        let stored_value = normalized.clone().unwrap_or_default();
        if let Err(err) = SharedSessionSettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .session_sharing_server_url
                .set_value(stored_value, ctx)
        }) {
            self.validation_message = Some(format!("Failed to save relay URL: {err:#}"));
            ctx.notify();
            return;
        }

        if let Err(err) = apply_session_sharing_server_url(normalized.as_deref(), true) {
            self.validation_message = Some(format!("Failed to apply relay URL: {err:#}"));
            ctx.notify();
            return;
        }

        self.validation_message = None;
        self.show_success_toast("Sharing settings saved", ctx);
        ctx.notify();
    }

    fn clear_relay_url(&mut self, ctx: &mut ViewContext<Self>) {
        self.relay_url_editor
            .update(ctx, |editor, ctx| editor.set_buffer_text("", ctx));

        if let Err(err) = SharedSessionSettings::handle(ctx).update(ctx, |settings, ctx| {
            settings
                .session_sharing_server_url
                .set_value(String::new(), ctx)
        }) {
            self.validation_message = Some(format!("Failed to clear relay URL: {err:#}"));
            ctx.notify();
            return;
        }

        if let Err(err) = apply_session_sharing_server_url(None, true) {
            self.validation_message = Some(format!("Failed to clear relay URL: {err:#}"));
            ctx.notify();
            return;
        }

        self.validation_message = None;
        self.show_success_toast("Sharing settings cleared", ctx);
        ctx.notify();
    }

    fn show_success_toast(&self, message: &str, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            toast_stack.add_ephemeral_toast(
                DismissibleToast::success(message.to_owned()),
                window_id,
                ctx,
            );
        });
    }
}

impl Entity for CloudSharingPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for CloudSharingPageView {
    type Action = CloudSharingPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CloudSharingPageAction::SaveRelayUrl => self.save_relay_url(ctx),
            CloudSharingPageAction::ClearRelayUrl => self.clear_relay_url(ctx),
        }
    }
}

impl View for CloudSharingPageView {
    fn ui_name() -> &'static str {
        "CloudSharingPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for CloudSharingPageView {
    fn section() -> SettingsSection {
        SettingsSection::CloudSharing
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<CloudSharingPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<CloudSharingPageView>) -> Self {
        SettingsPageViewHandle::CloudSharing(view_handle)
    }
}

#[derive(Default)]
struct CloudSharingPageWidget {
    save_button_mouse_state: MouseStateHandle,
    clear_button_mouse_state: MouseStateHandle,
}

impl SettingsWidget for CloudSharingPageWidget {
    type View = CloudSharingPageView;

    fn search_terms(&self) -> &str {
        "cloud platform sharing session relay websocket no cloud"
    }

    fn render(
        &self,
        view: &CloudSharingPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let is_no_cloud = crate::server::server_api::no_cloud_mode_enabled();
        let active_relay = ChannelState::session_sharing_server_url();
        let configured_relay = SharedSessionSettings::as_ref(app)
            .configured_session_sharing_server_url()
            .map(ToOwned::to_owned);

        let status_text = match (
            is_no_cloud,
            active_relay.as_ref(),
            configured_relay.as_deref(),
        ) {
            (true, Some(active), _) => format!("Active relay: {active}"),
            (true, None, Some(configured)) => format!("Configured relay: {configured}"),
            (true, None, None) => "No relay configured".to_owned(),
            (false, _, Some(configured)) => {
                format!("Saved for no-cloud mode: {configured}")
            }
            (false, _, None) => "No-cloud mode is off".to_owned(),
        };

        let editor = Container::new(
            ConstrainedBox::new(
                Clipped::new(ChildView::new(&view.relay_url_editor).finish()).finish(),
            )
            .with_height(28.)
            .finish(),
        )
        .with_margin_top(8.)
        .with_background_color(theme.surface_1().into())
        .with_border(Border::all(1.).with_border_fill(theme.outline()))
        .with_corner_radius(warpui::elements::CornerRadius::with_all(
            warpui::elements::Radius::Pixels(4.),
        ))
        .with_uniform_padding(6.)
        .finish();

        let mut save_button = appearance
            .ui_builder()
            .button(ButtonVariant::Accent, self.save_button_mouse_state.clone())
            .with_text_label("Save".to_owned())
            .with_style(UiComponentStyles {
                padding: Some(Coords::default().top(6.).bottom(6.).left(12.).right(12.)),
                ..Default::default()
            });

        let input = view.relay_url_editor.as_ref(app).buffer_text(app);
        if !input.trim().is_empty() && normalize_session_sharing_server_url(&input).is_err() {
            save_button = save_button.disabled();
        }

        let buttons = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                save_button
                    .build()
                    .on_click(|ctx, _, _| {
                        ctx.dispatch_typed_action(CloudSharingPageAction::SaveRelayUrl);
                    })
                    .finish(),
            )
            .with_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .button(
                            ButtonVariant::Secondary,
                            self.clear_button_mouse_state.clone(),
                        )
                        .with_text_label("Clear".to_owned())
                        .with_style(UiComponentStyles {
                            padding: Some(
                                Coords::default().top(6.).bottom(6.).left(12.).right(12.),
                            ),
                            ..Default::default()
                        })
                        .build()
                        .on_click(|ctx, _, _| {
                            ctx.dispatch_typed_action(CloudSharingPageAction::ClearRelayUrl);
                        })
                        .finish(),
                )
                .with_margin_left(8.)
                .finish(),
            )
            .finish();

        let mut column = Flex::column()
            .with_child(
                Text::new_inline(
                    "Session sharing relay",
                    appearance.ui_font_family(),
                    SUBHEADER_FONT_SIZE,
                )
                .with_style(Properties::default().weight(Weight::Bold))
                .with_color(theme.active_ui_text_color().into())
                .finish(),
            )
            .with_child(
                Container::new(
                    Text::new(
                        "Use a self-hosted websocket relay for shared terminal sessions.",
                        appearance.ui_font_family(),
                        CONTENT_FONT_SIZE,
                    )
                    .with_color(theme.nonactive_ui_text_color().into())
                    .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            )
            .with_child(
                Container::new(
                    Text::new_inline("Relay URL", appearance.ui_font_family(), CONTENT_FONT_SIZE)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(theme.active_ui_text_color().into())
                        .finish(),
                )
                .with_margin_top(18.)
                .finish(),
            )
            .with_child(editor)
            .with_child(
                Container::new(
                    Text::new(status_text, appearance.ui_font_family(), CONTENT_FONT_SIZE)
                        .with_color(theme.nonactive_ui_text_color().into())
                        .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            );

        if let Some(message) = &view.validation_message {
            column = column.with_child(
                Container::new(
                    Text::new(
                        message.clone(),
                        appearance.ui_font_family(),
                        CONTENT_FONT_SIZE,
                    )
                    .with_color(crate::themes::theme::Fill::error().into())
                    .finish(),
                )
                .with_margin_top(8.)
                .finish(),
            );
        }

        column
            .with_child(Container::new(buttons).with_margin_top(16.).finish())
            .finish()
    }
}
