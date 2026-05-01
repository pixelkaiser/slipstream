use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::auth::auth_view_modal::AuthRedirectPayload;
use crate::auth::auth_view_shared_helpers::{
    render_privacy_settings_toggles, PrivacySettingsActions, PrivacySettingsHandles,
};
use crate::auth::login_failure_notification::{self, LoginFailureReason};
use crate::editor::{EditorView, SingleLineEditorOptions, TextColors, TextOptions};
#[cfg(not(target_family = "wasm"))]
use crate::local_multi_agent::{
    LocalMultiAgentManager, LocalMultiAgentManagerEvent, LocalMultiAgentTestStatus,
};
use crate::server::telemetry::{LoginEventSource, TelemetryEvent};
use crate::settings::PrivacySettings;
use crate::themes::theme::Fill as ThemeFill;
use crate::util::bindings::CustomAction;
use crate::view_components::{DropdownItem, FilterableDropdown};
use crate::{send_telemetry_from_ctx, send_telemetry_sync_from_ctx, ChannelState};
#[cfg(not(target_family = "wasm"))]
use ::ai::api_keys::ApiKeyManager;

use onboarding::slides::{layout, slide_content};
use onboarding::{ai_features, drive_features, drive_name, OnboardingIntention};
use pathfinder_color::ColorU;
use ui_components::{button, Component as _, Options as _};
use warp_core::features::FeatureFlag;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::Icon;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Align, Border, CacheOption, ChildView, ClippedScrollStateHandle, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, Dismiss, Fill, Flex, FormattedTextElement,
    HighlightedHyperlink, Image, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    OffsetPositioning, ParentElement, Radius, Shrinkable, Stack,
};
use warpui::fonts::Weight;
use warpui::keymap::{FixedBinding, Keystroke};
use warpui::text_layout::TextAlignment;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    actions::StandardAction, AppContext, Element, Entity, FocusContext, SingletonEntity,
    TypedActionView, UpdateModel, View, ViewContext, ViewHandle,
};

use std::cell::Cell;

use pathfinder_geometry::vector::vec2f;
use warpui::elements::{ChildAnchor, ParentAnchor, ParentOffsetBounds};

const TOS_URL: &str = "https://www.warp.dev/terms-of-service";

// ---------------------------------------------------------------------------
// Init (keybindings)
// ---------------------------------------------------------------------------

pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;

    app.register_fixed_bindings([
        FixedBinding::new(
            "enter",
            LoginSlideAction::Enter,
            id!(LoginSlideView::ui_name()),
        ),
        FixedBinding::new(
            "cmdorctrl-enter",
            LoginSlideAction::ShowSkipDialog,
            id!(LoginSlideView::ui_name()),
        ),
        FixedBinding::new(
            "escape",
            LoginSlideAction::DismissOverlayOrBack,
            id!(LoginSlideView::ui_name()),
        ),
        FixedBinding::custom(
            CustomAction::Paste,
            LoginSlideAction::PasteAuthUrl,
            "Paste",
            id!(LoginSlideView::ui_name()),
        ),
        FixedBinding::standard(
            StandardAction::Paste,
            LoginSlideAction::PasteAuthUrl,
            id!(LoginSlideView::ui_name()),
        ),
    ]);

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
    app.register_fixed_bindings([FixedBinding::new(
        "cmdorctrl-v",
        LoginSlideAction::PasteAuthUrl,
        id!(LoginSlideView::ui_name()),
    )]);
}

// ---------------------------------------------------------------------------
// Actions & Events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum LoginSlideAction {
    Enter,
    ShowSkipDialog,
    ConfirmSkip,
    DismissDialog,
    DismissOverlayOrBack,
    Back,
    BackToSelectAuthPathway,
    CopyLoginUrl,
    EnterToken,
    ShowPrivacySettings,
    HideOverlay,
    ToggleTelemetry,
    ToggleCrashReporting,
    ToggleCloudConversationStorage,
    DismissNotification,
    PasteAuthUrl,
    TestProviderConnection,
    SetProviderDefaultModel(String),
}

#[derive(Clone, Debug)]
pub enum LoginSlideEvent {
    BackToOnboarding,
    LoginLaterConfirmed,
}

/// How the user arrived at the login slide. Controls which step is shown first
/// and how "Back" is routed when the user backs out of the privacy-settings step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginSlideSource {
    /// Reached via the normal onboarding flow (e.g. agent intention requires an account).
    OnboardingFlow,
    /// Reached via the "Log in" link on the intro / welcome slide.
    LoginExistingUserFromWelcome,
    /// Reached via the "Privacy Settings" link on the terminal-intention theme slide.
    /// Starts directly in the privacy settings step and routes Back to onboarding.
    PrivacySettingsFromTerminalIntentionTheme,
}

// ---------------------------------------------------------------------------
// Login step
// ---------------------------------------------------------------------------

enum LoginStep {
    SelectAuthPathway,
    BrowserOpen,
    PrivacySettings,
}

// ---------------------------------------------------------------------------
// Overlay
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
enum LoginSlideOverlay {
    SkipDialog,
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

const AUTH_TOKEN_INPUT_BORDER_RADIUS: Radius = Radius::Pixels(4.);

pub struct LoginSlideView {
    /// Whether AI will be enabled once onboarding is applied. Used to hide the
    /// cloud-conversation-storage toggle in the privacy settings step when the
    /// user has disabled Warp Agent during onboarding (or is on the terminal
    /// intention path, which disables AI). The actual `AISettings` value may
    /// not have been written yet at this point, since onboarding settings are
    /// applied after login.
    ai_enabled: bool,
    /// Onboarding intention selected by the user, used to render Drive-focused
    /// copy on the Terminal+Drive path. On the login slide, `intention ==
    /// OnboardingIntention::Terminal` is equivalent to "Terminal+Drive":
    /// `RootView` only routes Terminal-intent users here when Warp Drive is
    /// enabled.
    intention: OnboardingIntention,
    theme_visual_path: &'static str,
    step: LoginStep,
    active_overlay: Option<LoginSlideOverlay>,
    last_login_failure_reason: Option<LoginFailureReason>,
    source: LoginSlideSource,

    // Auth token input (browser-open step)
    auth_token_input: ViewHandle<EditorView>,
    show_auth_token_input: bool,
    #[cfg(not(target_family = "wasm"))]
    openai_base_url_input: ViewHandle<EditorView>,
    #[cfg(not(target_family = "wasm"))]
    openai_api_key_input: ViewHandle<EditorView>,
    #[cfg(not(target_family = "wasm"))]
    provider_model_dropdown: ViewHandle<FilterableDropdown<LoginSlideAction>>,
    provider_setup_error: Option<String>,

    // Buttons
    back_button: button::Button,
    skip_button: button::Button,
    login_button: button::Button,
    test_connection_button: button::Button,
    browser_back_button: button::Button,
    done_button: button::Button,
    dialog_login_button: button::Button,
    dialog_skip_button: button::Button,
    dialog_close_button: button::Button,

    // Mouse states for links
    tos_mouse_state: MouseStateHandle,
    privacy_settings_mouse_state: MouseStateHandle,
    copy_url_mouse_state: MouseStateHandle,
    enter_token_mouse_state: MouseStateHandle,

    // Privacy settings overlay (shared with AuthViewBody)
    privacy_settings_handles: PrivacySettingsHandles,

    scroll_state: ClippedScrollStateHandle,
    close_login_notification_mouse_state: MouseStateHandle,
    highlighted_hyperlink_state: HighlightedHyperlink,
}

/// All image paths used by the login slide visual. These mirror the set in
/// `ThemePickerSlide::VISUAL_IMAGE_PATHS` so the login slide can keep showing
/// the same themed right panel the user was looking at on the theme slide.
const VISUAL_IMAGE_PATHS: &[&str] = &[
    // Terminal intention
    "async/png/onboarding/terminal_intention/theme/theme_phenomenon_vertical.png",
    "async/png/onboarding/terminal_intention/theme/theme_phenomenon_horizontal.png",
    "async/png/onboarding/terminal_intention/theme/theme_dark_vertical.png",
    "async/png/onboarding/terminal_intention/theme/theme_dark_horizontal.png",
    "async/png/onboarding/terminal_intention/theme/theme_light_vertical.png",
    "async/png/onboarding/terminal_intention/theme/theme_light_horizontal.png",
    "async/png/onboarding/terminal_intention/theme/theme_adeberry_vertical.png",
    "async/png/onboarding/terminal_intention/theme/theme_adeberry_horizontal.png",
    // Agent intention
    "async/png/onboarding/agent_intention/theme/theme_phenomenon_vertical.png",
    "async/png/onboarding/agent_intention/theme/theme_phenomenon_horizontal.png",
    "async/png/onboarding/agent_intention/theme/theme_dark_vertical.png",
    "async/png/onboarding/agent_intention/theme/theme_dark_horizontal.png",
    "async/png/onboarding/agent_intention/theme/theme_light_vertical.png",
    "async/png/onboarding/agent_intention/theme/theme_light_horizontal.png",
    "async/png/onboarding/agent_intention/theme/theme_adeberry_vertical.png",
    "async/png/onboarding/agent_intention/theme/theme_adeberry_horizontal.png",
];

fn resolve_visual_path(
    intention: OnboardingIntention,
    theme_name: &str,
    use_vertical_tabs: bool,
) -> &'static str {
    let intention_dir = match intention {
        OnboardingIntention::AgentDrivenDevelopment => "agent_intention",
        OnboardingIntention::Terminal => "terminal_intention",
    };
    let name_key = match theme_name {
        "Phenomenon" => "phenomenon",
        "Dark" => "dark",
        "Light" => "light",
        "Adeberry" => "adeberry",
        _ => "dark",
    };
    let orientation = if use_vertical_tabs {
        "vertical"
    } else {
        "horizontal"
    };
    VISUAL_IMAGE_PATHS
        .iter()
        .find(|p| p.contains(intention_dir) && p.contains(name_key) && p.contains(orientation))
        .unwrap_or(&VISUAL_IMAGE_PATHS[0])
}

impl LoginSlideView {
    /// Whether the auth token input editor is currently rendered and should be focusable.
    /// This is only true on the BrowserOpen step after the user clicks to paste their token.
    pub fn is_auth_token_input_visible(&self) -> bool {
        matches!(self.step, LoginStep::BrowserOpen) && self.show_auth_token_input
    }

    pub fn should_allow_text_input_focus(&self) -> bool {
        self.is_auth_token_input_visible()
            || (matches!(self.step, LoginStep::SelectAuthPathway) && {
                #[cfg(not(target_family = "wasm"))]
                {
                    self.should_render_provider_setup()
                }
                #[cfg(target_family = "wasm")]
                {
                    false
                }
            })
    }

    pub fn new(
        ai_enabled: bool,
        theme_name: &str,
        use_vertical_tabs: bool,
        intention: OnboardingIntention,
        source: LoginSlideSource,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let auth_manager = AuthManager::handle(ctx);
        ctx.subscribe_to_model(&auth_manager, |me, _, event, ctx| {
            me.handle_auth_manager_event(event, ctx);
        });

        let auth_token_input = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let text_color = ThemeFill::Solid(ColorU::black());
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(12.),
                        font_family_override: Some(appearance.ui_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color: text_color,
                            disabled_color: text_color.with_opacity(20),
                            hint_color: text_color.with_opacity(40),
                        }),
                        ..Default::default()
                    },
                    soft_wrap: false,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Auth Token", ctx);
            editor
        });

        ctx.subscribe_to_view(&auth_token_input, |me, _, event, ctx| {
            use crate::editor::Event::{AltEnter, CmdEnter, Enter, Paste, ShiftEnter};
            match event {
                AltEnter | CmdEnter | Enter | Paste | ShiftEnter => {
                    let text = me.auth_token_input.as_ref(ctx).buffer_text(ctx);
                    me.handle_pasted_auth_url(text, ctx);
                }
                _ => {}
            };
            ctx.notify();
        });

        #[cfg(not(target_family = "wasm"))]
        let openai_base_url_input = {
            let initial_value = LocalMultiAgentManager::as_ref(ctx)
                .config()
                .openai_base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            ctx.add_typed_action_view(move |ctx| {
                let appearance = Appearance::as_ref(ctx);
                let options = SingleLineEditorOptions {
                    text: TextOptions {
                        font_size_override: Some(appearance.ui_font_size()),
                        font_family_override: Some(appearance.monospace_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color: appearance.theme().active_ui_text_color(),
                            disabled_color: appearance.theme().disabled_ui_text_color(),
                            hint_color: appearance.theme().disabled_ui_text_color(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut editor = EditorView::single_line(options, ctx);
                editor.set_placeholder_text("https://api.openai.com/v1", ctx);
                editor.set_buffer_text(&initial_value, ctx);
                editor
            })
        };

        #[cfg(not(target_family = "wasm"))]
        let openai_api_key_input = {
            let initial_value = ApiKeyManager::as_ref(ctx)
                .keys()
                .openai
                .clone()
                .unwrap_or_default();
            ctx.add_typed_action_view(move |ctx| {
                let appearance = Appearance::as_ref(ctx);
                let options = SingleLineEditorOptions {
                    is_password: true,
                    text: TextOptions {
                        font_size_override: Some(appearance.ui_font_size()),
                        font_family_override: Some(appearance.monospace_font_family()),
                        text_colors_override: Some(TextColors {
                            default_color: appearance.theme().active_ui_text_color(),
                            disabled_color: appearance.theme().disabled_ui_text_color(),
                            hint_color: appearance.theme().disabled_ui_text_color(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut editor = EditorView::single_line(options, ctx);
                editor.set_placeholder_text("sk-...", ctx);
                if !initial_value.is_empty() {
                    editor.set_buffer_text(&initial_value, ctx);
                }
                editor
            })
        };

        #[cfg(not(target_family = "wasm"))]
        let provider_model_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(420.);
            dropdown.set_menu_width(420., ctx);
            dropdown.set_disabled(ctx);
            dropdown
        });

        #[cfg(not(target_family = "wasm"))]
        {
            Self::refresh_provider_model_dropdown(&provider_model_dropdown, ctx);
            let provider_model_dropdown = provider_model_dropdown.clone();
            ctx.subscribe_to_model(
                &LocalMultiAgentManager::handle(ctx),
                move |me, _model, event, ctx| {
                    if matches!(
                        event,
                        LocalMultiAgentManagerEvent::StatusChanged
                            | LocalMultiAgentManagerEvent::TestStatusChanged
                    ) {
                        me.provider_setup_error = None;
                        Self::refresh_provider_model_dropdown(&provider_model_dropdown, ctx);
                        ctx.notify();
                    }
                },
            );

            ctx.subscribe_to_view(&openai_base_url_input, |me, _, event, ctx| {
                use crate::editor::Event::{Edited, Paste};
                match event {
                    Edited(_) | Paste => {
                        me.provider_setup_error = None;
                        ctx.notify();
                    }
                    _ => {}
                }
            });
            ctx.subscribe_to_view(&openai_api_key_input, |me, _, event, ctx| {
                use crate::editor::Event::{Edited, Paste};
                match event {
                    Edited(_) | Paste => {
                        me.provider_setup_error = None;
                        ctx.notify();
                    }
                    _ => {}
                }
            });
        }

        Self {
            ai_enabled,
            intention,
            theme_visual_path: resolve_visual_path(intention, theme_name, use_vertical_tabs),
            step: match source {
                LoginSlideSource::OnboardingFlow => LoginStep::SelectAuthPathway,
                LoginSlideSource::LoginExistingUserFromWelcome => LoginStep::BrowserOpen,
                LoginSlideSource::PrivacySettingsFromTerminalIntentionTheme => {
                    LoginStep::PrivacySettings
                }
            },
            active_overlay: None,
            last_login_failure_reason: None,
            source,
            auth_token_input,
            show_auth_token_input: false,
            #[cfg(not(target_family = "wasm"))]
            openai_base_url_input,
            #[cfg(not(target_family = "wasm"))]
            openai_api_key_input,
            #[cfg(not(target_family = "wasm"))]
            provider_model_dropdown,
            provider_setup_error: None,
            back_button: button::Button::default(),
            skip_button: button::Button::default(),
            login_button: button::Button::default(),
            test_connection_button: button::Button::default(),
            browser_back_button: button::Button::default(),
            done_button: button::Button::default(),
            dialog_login_button: button::Button::default(),
            dialog_skip_button: button::Button::default(),
            dialog_close_button: button::Button::default(),
            tos_mouse_state: MouseStateHandle::default(),
            privacy_settings_mouse_state: MouseStateHandle::default(),
            copy_url_mouse_state: MouseStateHandle::default(),
            enter_token_mouse_state: MouseStateHandle::default(),
            privacy_settings_handles: PrivacySettingsHandles::default(),
            scroll_state: ClippedScrollStateHandle::new(),
            close_login_notification_mouse_state: MouseStateHandle::default(),
            highlighted_hyperlink_state: HighlightedHyperlink::default(),
        }
    }

    // ------------------------------------------------------------------
    // Auth manager
    // ------------------------------------------------------------------

    fn handle_auth_manager_event(&mut self, event: &AuthManagerEvent, ctx: &mut ViewContext<Self>) {
        match event {
            AuthManagerEvent::AuthFailed(err) => {
                use crate::server::server_api::auth::UserAuthenticationError;
                if let UserAuthenticationError::InvalidStateParameter = err {
                    self.last_login_failure_reason =
                        Some(LoginFailureReason::InvalidStateParameter);
                } else if let UserAuthenticationError::MissingStateParameter = err {
                    self.last_login_failure_reason =
                        Some(LoginFailureReason::MissingStateParameter);
                } else {
                    self.last_login_failure_reason =
                        Some(LoginFailureReason::FailedUserAuthentication);
                }
            }
            AuthManagerEvent::CreateAnonymousUserFailed => {
                self.last_login_failure_reason = Some(LoginFailureReason::FailedUserAuthentication);
            }
            AuthManagerEvent::MintCustomTokenFailed(_) => {
                self.last_login_failure_reason = Some(LoginFailureReason::FailedMintCustomToken);
            }
            _ => {}
        }
        ctx.notify();
    }

    fn handle_pasted_auth_url(&mut self, pasted_url: String, ctx: &mut ViewContext<Self>) {
        match AuthRedirectPayload::from_raw_url(pasted_url) {
            Ok(redirect_payload) => {
                AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                    auth_manager.initialize_user_from_auth_payload(redirect_payload, true, ctx);
                });
            }
            Err(error) => {
                log::error!("Failed to parse AuthRedirectPayload from redirect URL: {error:#}");
                self.last_login_failure_reason =
                    Some(LoginFailureReason::InvalidRedirectUrl { was_pasted: true });
            }
        }
        ctx.notify();
    }

    fn handle_login_later(&mut self, ctx: &mut ViewContext<Self>) {
        // Send synchronously since this is an important event in the sign up funnel and we
        // don't want to lose events if the user quits before the event queue is flushed.
        send_telemetry_sync_from_ctx!(
            TelemetryEvent::LoginLaterConfirmationButtonClicked {
                source: LoginEventSource::OnboardingSlide,
            },
            ctx
        );
        if FeatureFlag::SkipFirebaseAnonymousUser.is_enabled() {
            AuthManager::handle(ctx).update(ctx, |_, ctx| {
                ctx.emit(AuthManagerEvent::SkippedLogin);
            });
        } else {
            AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                auth_manager.create_anonymous_user(None, ctx);
            });
        }
        ctx.emit(LoginSlideEvent::LoginLaterConfirmed);
    }

    #[cfg(not(target_family = "wasm"))]
    fn should_render_provider_setup(&self) -> bool {
        self.ai_enabled && matches!(self.source, LoginSlideSource::OnboardingFlow)
    }

    #[cfg(not(target_family = "wasm"))]
    fn provider_setup_values(&self, ctx: &AppContext) -> (Option<String>, Option<String>) {
        let base_url = self
            .openai_base_url_input
            .as_ref(ctx)
            .buffer_text(ctx)
            .trim()
            .trim_end_matches('/')
            .to_string();
        let api_key = self
            .openai_api_key_input
            .as_ref(ctx)
            .buffer_text(ctx)
            .trim()
            .to_string();

        (
            (!base_url.is_empty()).then_some(base_url),
            (!api_key.is_empty()).then_some(api_key),
        )
    }

    #[cfg(not(target_family = "wasm"))]
    fn refresh_provider_model_dropdown(
        dropdown: &ViewHandle<FilterableDropdown<LoginSlideAction>>,
        ctx: &mut AppContext,
    ) {
        let manager = LocalMultiAgentManager::as_ref(ctx);
        let config = manager.config();
        let choices = config.model_choices(manager.discovered_models());
        let selected = config
            .openai_model
            .as_ref()
            .filter(|model| choices.iter().any(|choice| choice == *model))
            .or_else(|| choices.first())
            .cloned();
        let is_enabled = matches!(
            manager.test_status(),
            LocalMultiAgentTestStatus::Passed { .. }
        ) && !choices.is_empty();

        dropdown.update(ctx, |dropdown, ctx| {
            if is_enabled {
                dropdown.set_enabled(ctx);
            } else {
                dropdown.set_disabled(ctx);
            }
            dropdown.set_items(
                choices
                    .into_iter()
                    .map(|model| {
                        DropdownItem::new(
                            model.clone(),
                            LoginSlideAction::SetProviderDefaultModel(model),
                        )
                    })
                    .collect(),
                ctx,
            );
            if let Some(selected) = selected {
                dropdown.set_selected_by_action(
                    LoginSlideAction::SetProviderDefaultModel(selected),
                    ctx,
                );
            }
        });
    }

    #[cfg(not(target_family = "wasm"))]
    fn persist_provider_setup(
        &mut self,
        require_api_key: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Result<(), String> {
        let (base_url, api_key) = self.provider_setup_values(ctx);

        let Some(base_url) = base_url else {
            self.provider_setup_error = Some("OpenAI Base URL is required.".to_string());
            return Err("OpenAI Base URL is required.".to_string());
        };
        if require_api_key && api_key.is_none() {
            self.provider_setup_error = Some("OpenAI API key is required.".to_string());
            return Err("OpenAI API key is required.".to_string());
        }

        ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
            manager.set_openai_key(api_key.clone(), ctx);
            manager.set_openai_base_url(Some(base_url.clone()), ctx);
        });

        LocalMultiAgentManager::handle(ctx).update(ctx, |manager, ctx| {
            manager.update_provider_config(api_key, ctx);
            let mut config = manager.config().clone();
            config.openai_base_url = Some(base_url);
            manager
                .set_config(config, ctx)
                .map_err(|error| error.to_string())
        })?;

        self.provider_setup_error = None;
        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    fn is_provider_connection_verified(&self, app: &AppContext) -> bool {
        if !self.should_render_provider_setup()
            || !matches!(
                LocalMultiAgentManager::as_ref(app).test_status(),
                LocalMultiAgentTestStatus::Passed { .. }
            )
        {
            return false;
        }

        let (base_url, api_key) = self.provider_setup_values(app);
        let manager = LocalMultiAgentManager::as_ref(app);
        let stored_base_url = manager
            .config()
            .openai_base_url
            .as_deref()
            .map(|url| url.trim().trim_end_matches('/'));
        let stored_api_key = ApiKeyManager::as_ref(app).keys().openai.as_deref();
        let has_selected_model = manager
            .config()
            .openai_model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty());

        has_selected_model
            && base_url.as_deref() == stored_base_url
            && api_key.as_deref() == stored_api_key
    }

    // ------------------------------------------------------------------
    // Rendering — main layout
    // ------------------------------------------------------------------

    fn render_content(
        &self,
        appearance: &Appearance,
        app: &AppContext,
        editor_rendered: &Cell<bool>,
    ) -> Box<dyn Element> {
        match self.step {
            LoginStep::SelectAuthPathway => {
                let children = self.render_select_auth_content(appearance, app, editor_rendered);
                let bottom_nav = self.render_select_auth_bottom_nav(appearance, app);
                slide_content::onboarding_slide_content(
                    children,
                    bottom_nav,
                    self.scroll_state.clone(),
                    appearance,
                )
            }
            LoginStep::BrowserOpen => {
                let children = self.render_browser_open_content(appearance, editor_rendered);
                let bottom_nav = self.render_browser_open_bottom_nav(appearance);
                slide_content::onboarding_slide_content(
                    children,
                    bottom_nav,
                    self.scroll_state.clone(),
                    appearance,
                )
            }
            LoginStep::PrivacySettings => {
                let children = self.render_privacy_settings_content(appearance, app);
                let bottom_nav = self.render_privacy_settings_bottom_nav(appearance);
                slide_content::onboarding_slide_content(
                    children,
                    bottom_nav,
                    self.scroll_state.clone(),
                    appearance,
                )
            }
        }
    }

    // ------------------------------------------------------------------
    // Step 1: Select auth pathway
    // ------------------------------------------------------------------

    /// Disclaimer prefix shown before the "Privacy Settings" link. AI is
    /// dropped from the wording on paths that don't enable AI (e.g.
    /// Terminal+Drive), since there are no AI features to opt out of there.
    fn privacy_disclaimer_prefix(&self) -> &'static str {
        if self.ai_enabled {
            "If you'd like to opt out of analytics and AI features, you can adjust your "
        } else {
            "If you'd like to opt out of analytics, you can adjust your "
        }
    }

    fn render_select_auth_content(
        &self,
        appearance: &Appearance,
        app: &AppContext,
        editor_rendered: &Cell<bool>,
    ) -> Vec<Box<dyn Element>> {
        let theme = appearance.theme();
        let sub_text_color = internal_colors::text_sub(theme, theme.background().into_solid());
        let ui_builder = appearance.ui_builder();

        let is_terminal = matches!(self.intention, OnboardingIntention::Terminal);
        #[cfg(not(target_family = "wasm"))]
        let provider_setup = self.should_render_provider_setup();
        #[cfg(target_family = "wasm")]
        let provider_setup = false;

        let product_name = ChannelState::product_name();
        let title_text = if provider_setup {
            "Connect your AI provider".to_string()
        } else if is_terminal {
            format!("Get started with {product_name} Drive")
        } else {
            "Get started with AI".to_string()
        };
        let title = FormattedTextElement::from_str(title_text, appearance.ui_font_family(), 36.)
            .with_color(internal_colors::text_main(
                theme,
                theme.background().into_solid(),
            ))
            .with_weight(Weight::Medium)
            .with_alignment(TextAlignment::Left)
            .finish();

        let subtitle_text = if provider_setup {
            format!(
                "Enter an OpenAI-compatible Base URL and API key. {product_name} uses this connection locally for AI features."
            )
        } else if is_terminal {
            "Connect your account to save and share notebooks, workflows, and more across devices."
                .to_string()
        } else {
            "Connect your account to enable AI-powered planning, coding, and automation."
                .to_string()
        };
        let subtitle =
            FormattedTextElement::from_str(subtitle_text, appearance.ui_font_family(), 16.)
                .with_color(sub_text_color)
                .with_weight(Weight::Normal)
                .with_alignment(TextAlignment::Left)
                .with_line_height_ratio(1.0)
                .finish();

        // TOS and Privacy links
        let disclaimer_styles = UiComponentStyles {
            font_color: Some(sub_text_color),
            font_size: Some(12.),
            ..Default::default()
        };

        let tos_line = if provider_setup {
            ui_builder
                .span("Your API key is stored locally and sent only to your configured provider.")
                .with_style(disclaimer_styles)
                .build()
                .finish()
        } else {
            Flex::row()
                .with_child(
                    ui_builder
                        .span(format!("By continuing, you agree to {product_name}'s "))
                        .with_style(disclaimer_styles)
                        .build()
                        .finish(),
                )
                .with_child(
                    ui_builder
                        .link(
                            "Terms of Service".into(),
                            Some(TOS_URL.into()),
                            None,
                            self.tos_mouse_state.clone(),
                        )
                        .soft_wrap(false)
                        .with_style(UiComponentStyles {
                            font_size: Some(12.),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .finish()
        };

        let show_privacy_settings_link = ChannelState::product_name() != "Slipstream";
        let mut disclaimer_column = Flex::column();
        if show_privacy_settings_link {
            let privacy_line = Flex::row()
                .with_child(
                    ui_builder
                        .span(self.privacy_disclaimer_prefix())
                        .with_style(disclaimer_styles)
                        .build()
                        .finish(),
                )
                .with_child(
                    ui_builder
                        .link(
                            "Privacy Settings".into(),
                            None,
                            Some(Box::new(|ctx| {
                                ctx.dispatch_typed_action(LoginSlideAction::ShowPrivacySettings);
                            })),
                            self.privacy_settings_mouse_state.clone(),
                        )
                        .soft_wrap(false)
                        .with_style(UiComponentStyles {
                            font_size: Some(12.),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .finish();
            disclaimer_column = disclaimer_column.with_child(privacy_line);
        }
        let disclaimers = Container::new(
            disclaimer_column
                .with_child(
                    Container::new(tos_line)
                        .with_margin_top(if show_privacy_settings_link { 8. } else { 0. })
                        .finish(),
                )
                .finish(),
        )
        .with_margin_top(24.)
        .finish();

        #[cfg(not(target_family = "wasm"))]
        let provider_form = provider_setup.then(|| {
            let editor_style = UiComponentStyles {
                padding: Some(Coords {
                    top: 10.,
                    bottom: 10.,
                    left: 16.,
                    right: 16.,
                }),
                background: Some(theme.surface_2().into()),
                ..Default::default()
            };

            let render_real_editors = if editor_rendered.get() {
                true
            } else {
                editor_rendered.set(true);
                false
            };

            let render_input =
                |label: &'static str, editor: ViewHandle<EditorView>| -> Box<dyn Element> {
                    let label = ui_builder
                        .span(label)
                        .with_style(UiComponentStyles {
                            font_color: Some(internal_colors::text_main(
                                theme,
                                theme.background().into_solid(),
                            )),
                            font_size: Some(12.),
                            ..Default::default()
                        })
                        .build()
                        .finish();

                    let input = if render_real_editors {
                        ui_builder
                            .text_input(editor)
                            .with_style(editor_style)
                            .build()
                            .finish()
                    } else {
                        ConstrainedBox::new(
                            Container::new(warpui::elements::Empty::new().finish())
                                .with_background(theme.surface_2())
                                .finish(),
                        )
                        .with_height(40.)
                        .finish()
                    };

                    Flex::column()
                        .with_child(label)
                        .with_child(Container::new(input).with_margin_top(8.).finish())
                        .finish()
                };

            let render_model_selector = || -> Box<dyn Element> {
                let label = ui_builder
                    .span("Default model")
                    .with_style(UiComponentStyles {
                        font_color: Some(internal_colors::text_main(
                            theme,
                            theme.background().into_solid(),
                        )),
                        font_size: Some(12.),
                        ..Default::default()
                    })
                    .build()
                    .finish();

                Flex::column()
                    .with_child(label)
                    .with_child(
                        Container::new(ChildView::new(&self.provider_model_dropdown).finish())
                            .with_margin_top(8.)
                            .finish(),
                    )
                    .finish()
            };

            let provider_test_passed = matches!(
                LocalMultiAgentManager::as_ref(app).test_status(),
                LocalMultiAgentTestStatus::Passed { .. }
            );

            let status = match (
                self.provider_setup_error.as_ref(),
                LocalMultiAgentManager::as_ref(app).test_status(),
            ) {
                (Some(error), _) => Some((error.clone(), theme.ansi_fg_red())),
                (None, LocalMultiAgentTestStatus::Testing) => {
                    Some(("Testing connection...".to_string(), sub_text_color))
                }
                (None, LocalMultiAgentTestStatus::Passed { model_count }) => Some((
                    format!("Connection verified. Found {model_count} models."),
                    theme.ansi_fg_green(),
                )),
                (None, LocalMultiAgentTestStatus::Failed { message }) => {
                    Some((format!("Connection failed: {message}"), theme.ansi_fg_red()))
                }
                (None, LocalMultiAgentTestStatus::NotRun) => None,
            };

            let mut form = Flex::column()
                .with_child(render_input(
                    "OpenAI Base URL",
                    self.openai_base_url_input.clone(),
                ))
                .with_child(
                    Container::new(render_input(
                        "OpenAI API Key",
                        self.openai_api_key_input.clone(),
                    ))
                    .with_margin_top(14.)
                    .finish(),
                );

            if provider_test_passed {
                form = form.with_child(
                    Container::new(render_model_selector())
                        .with_margin_top(14.)
                        .finish(),
                );
            }

            if let Some((text, color)) = status {
                form = form.with_child(
                    Container::new(
                        FormattedTextElement::from_str(text, appearance.ui_font_family(), 12.)
                            .with_color(color)
                            .with_weight(Weight::Normal)
                            .with_alignment(TextAlignment::Left)
                            .with_line_height_ratio(1.2)
                            .finish(),
                    )
                    .with_margin_top(12.)
                    .finish(),
                );
            }

            Container::new(form.finish()).with_margin_top(24.).finish()
        });

        let header = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(title)
            .with_child(Container::new(subtitle).with_margin_top(16.).finish())
            .with_child(disclaimers);

        #[cfg(not(target_family = "wasm"))]
        let header = if let Some(provider_form) = provider_form {
            header.with_child(provider_form)
        } else {
            header
        };

        let header = header.finish();

        vec![header]
    }

    fn render_select_auth_bottom_nav(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let back_button = self.back_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Back".into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::Back);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        #[cfg(not(target_family = "wasm"))]
        if self.should_render_provider_setup() {
            let test_status = LocalMultiAgentManager::as_ref(app).test_status();
            let is_testing = matches!(test_status, LocalMultiAgentTestStatus::Testing);
            let (base_url, _) = self.provider_setup_values(app);
            let has_provider_base_url = base_url.is_some();
            let connection_verified = self.is_provider_connection_verified(app);

            let test_label = match test_status {
                LocalMultiAgentTestStatus::NotRun => "Test connection",
                LocalMultiAgentTestStatus::Testing => "Testing...",
                LocalMultiAgentTestStatus::Passed { .. } => "Retest connection",
                LocalMultiAgentTestStatus::Failed { .. } => "Retry connection",
            };
            let test_button = self.test_connection_button.render(
                appearance,
                button::Params {
                    content: button::Content::Label(test_label.into()),
                    theme: &button::themes::Secondary,
                    options: button::Options {
                        disabled: is_testing || !has_provider_base_url,
                        on_click: Some(Box::new(|ctx, _app, _pos| {
                            ctx.dispatch_typed_action(LoginSlideAction::TestProviderConnection);
                        })),
                        ..button::Options::default(appearance)
                    },
                },
            );

            let enter = Keystroke::parse("enter").unwrap_or_default();
            let continue_button = self.login_button.render(
                appearance,
                button::Params {
                    content: button::Content::Label("Continue".into()),
                    theme: &button::themes::Primary,
                    options: button::Options {
                        disabled: !connection_verified,
                        keystroke: connection_verified.then_some(enter),
                        on_click: Some(Box::new(|ctx, _app, _pos| {
                            ctx.dispatch_typed_action(LoginSlideAction::Enter);
                        })),
                        ..button::Options::default(appearance)
                    },
                },
            );

            let right_buttons = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(test_button)
                .with_child(
                    Container::new(continue_button)
                        .with_margin_left(8.)
                        .finish(),
                )
                .finish();

            return Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(back_button)
                .with_child(right_buttons)
                .finish();
        }

        let cmd_enter = Keystroke::parse("cmdorctrl-enter").unwrap_or_default();
        let skip_label = if matches!(self.intention, OnboardingIntention::Terminal) {
            format!("Disable {}", drive_name())
        } else {
            "Disable AI features".to_string()
        };
        let skip_button = self.skip_button.render(
            appearance,
            button::Params {
                content: button::Content::Label(skip_label.into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    keystroke: Some(cmd_enter),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::ShowSkipDialog);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let enter = Keystroke::parse("enter").unwrap_or_default();
        let login_button = self.login_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Continue".into()),
                theme: &button::themes::Primary,
                options: button::Options {
                    keystroke: Some(enter),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::Enter);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let right_buttons = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(skip_button)
            .with_child(Container::new(login_button).with_margin_left(4.).finish())
            .finish();

        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(back_button)
            .with_child(right_buttons)
            .finish()
    }

    // ------------------------------------------------------------------
    // Step 2: Browser open
    // ------------------------------------------------------------------

    fn render_browser_open_content(
        &self,
        appearance: &Appearance,
        editor_rendered: &Cell<bool>,
    ) -> Vec<Box<dyn Element>> {
        let theme = appearance.theme();
        let sub_text_color = internal_colors::text_sub(theme, theme.background().into_solid());
        let ui_builder = appearance.ui_builder();

        let sub_text_styles = UiComponentStyles {
            font_color: Some(sub_text_color),
            ..Default::default()
        };

        let title = FormattedTextElement::from_str(
            "Sign in on your browser to continue",
            appearance.ui_font_family(),
            36.,
        )
        .with_color(internal_colors::text_main(
            theme,
            theme.background().into_solid(),
        ))
        .with_weight(Weight::Medium)
        .with_alignment(TextAlignment::Left)
        .finish();

        let hint = Flex::column()
            .with_child(
                Flex::row()
                    .with_child(
                        ui_builder
                            .span("If your browser hasn't launched, ")
                            .with_style(sub_text_styles)
                            .build()
                            .finish(),
                    )
                    .with_child(
                        ui_builder
                            .link(
                                "copy the URL".into(),
                                None,
                                Some(Box::new(|ctx| {
                                    ctx.dispatch_typed_action(LoginSlideAction::CopyLoginUrl);
                                })),
                                self.copy_url_mouse_state.clone(),
                            )
                            .soft_wrap(false)
                            .build()
                            .finish(),
                    )
                    .with_child(
                        ui_builder
                            .span(" and open")
                            .with_style(sub_text_styles)
                            .build()
                            .finish(),
                    )
                    .finish(),
            )
            .with_child(
                ui_builder
                    .span("the page manually.")
                    .with_style(sub_text_styles)
                    .build()
                    .finish(),
            )
            .finish();

        // Auth token: show either the "Click here" link or the input box.
        // When showing the input, we use `editor_rendered` (a Cell<bool> passed
        // from render()) so the ChildView is only created on the FIRST call of
        // this closure. static_left calls the left-content closure twice (for
        // narrow and wide layouts); creating two ChildViews for the same editor
        // breaks focus/event dispatch.
        let auth_token: Box<dyn Element> = if self.show_auth_token_input {
            if editor_rendered.get() {
                // Second call (two-column layout, the default): render the real editor.
                ui_builder
                    .text_input(self.auth_token_input.clone())
                    .with_style(UiComponentStyles {
                        background: Some(Fill::Solid(ColorU::white())),
                        border_width: Some(0.),
                        border_radius: Some(CornerRadius::with_all(AUTH_TOKEN_INPUT_BORDER_RADIUS)),
                        padding: Some(Coords {
                            top: 12.,
                            bottom: 12.,
                            left: 16.,
                            right: 16.,
                        }),
                        margin: Some(Coords {
                            top: 8.,
                            bottom: 0.,
                            left: 0.,
                            right: 0.,
                        }),
                        ..Default::default()
                    })
                    .build()
                    .finish()
            } else {
                // First call (narrow layout fallback): placeholder.
                editor_rendered.set(true);
                Container::new(warpui::elements::Empty::new().finish())
                    .with_padding_top(12.)
                    .with_padding_bottom(12.)
                    .with_padding_left(16.)
                    .with_padding_right(16.)
                    .with_margin_top(8.)
                    .finish()
            }
        } else {
            Flex::row()
                .with_child(
                    ui_builder
                        .link(
                            "Click here to paste your token from the browser".into(),
                            None,
                            Some(Box::new(|ctx| {
                                ctx.dispatch_typed_action(LoginSlideAction::EnterToken);
                            })),
                            self.enter_token_mouse_state.clone(),
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .finish()
        };

        let header = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(title)
            .with_child(Container::new(hint).with_margin_top(16.).finish())
            .with_child(Container::new(auth_token).with_margin_top(16.).finish())
            .finish();

        vec![header]
    }

    fn render_browser_open_bottom_nav(&self, appearance: &Appearance) -> Box<dyn Element> {
        let back_button = self.browser_back_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Back".into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::BackToSelectAuthPathway);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(back_button)
            .finish()
    }

    // ------------------------------------------------------------------
    // Step 3: Privacy settings (inline in left column)
    // ------------------------------------------------------------------

    fn render_privacy_settings_content(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Vec<Box<dyn Element>> {
        let theme = appearance.theme();

        let title =
            FormattedTextElement::from_str("Privacy Settings", appearance.ui_font_family(), 36.)
                .with_color(internal_colors::text_main(
                    theme,
                    theme.background().into_solid(),
                ))
                .with_weight(Weight::Medium)
                .with_alignment(TextAlignment::Left)
                .finish();

        let actions = PrivacySettingsActions {
            toggle_telemetry: LoginSlideAction::ToggleTelemetry,
            toggle_crash_reporting: LoginSlideAction::ToggleCrashReporting,
            toggle_cloud_conversation_storage: LoginSlideAction::ToggleCloudConversationStorage,
            hide_overlay: LoginSlideAction::HideOverlay,
        };

        let toggles = render_privacy_settings_toggles(
            appearance,
            app,
            &self.privacy_settings_handles,
            &actions,
            self.ai_enabled,
        );

        vec![title, Container::new(toggles).with_margin_top(24.).finish()]
    }

    fn render_privacy_settings_bottom_nav(&self, appearance: &Appearance) -> Box<dyn Element> {
        let back_button = self.done_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Back".into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::HideOverlay);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(back_button)
            .finish()
    }

    // ------------------------------------------------------------------
    // Visual
    // ------------------------------------------------------------------

    fn render_visual(&self) -> Box<dyn Element> {
        let path = self.theme_visual_path;
        layout::onboarding_right_panel_with_bg(path, layout::FOREGROUND_LAYOUT_DEFAULT)
    }

    // ------------------------------------------------------------------
    // Rendering — skip confirmation dialog
    // ------------------------------------------------------------------

    fn render_skip_dialog(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let dialog_surface = theme.surface_1();
        let dialog_surface_solid = dialog_surface.into_solid();
        let border_color = internal_colors::neutral_4(theme);

        let is_terminal = matches!(self.intention, OnboardingIntention::Terminal);
        let title_text = if is_terminal {
            format!("Are you sure you want to disable {}?", drive_name())
        } else {
            "Are you sure you want to disable AI features?".to_string()
        };
        let title = FormattedTextElement::from_str(title_text, appearance.ui_font_family(), 16.)
            .with_color(internal_colors::text_main(theme, dialog_surface_solid))
            .with_weight(Weight::Bold)
            .with_line_height_ratio(1.25)
            .finish();

        // Close button with ESC keyboard-shortcut badge.
        let escape = Keystroke::parse("escape").unwrap_or_default();
        let close_button = self.dialog_close_button.render(
            appearance,
            button::Params {
                content: button::Content::Icon(Icon::X),
                theme: &button::themes::Naked,
                options: button::Options {
                    keystroke: Some(escape),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::DismissDialog);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let title_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(Shrinkable::new(1., title).finish())
            .with_child(close_button)
            .finish();

        let body_text_str = if is_terminal {
            format!(
                "{} lets you save workflows and knowledge across devices and share them with your team. By continuing, you won't have access to the following features:",
                drive_name()
            )
        } else {
            format!(
                "{} is better with AI. By continuing, you won't have access to any of the following features:",
                ChannelState::product_name()
            )
        };
        let body_text =
            FormattedTextElement::from_str(body_text_str, appearance.ui_font_family(), 14.)
                .with_color(internal_colors::text_main(theme, dialog_surface_solid))
                .with_weight(Weight::Normal)
                .with_line_height_ratio(1.2)
                .finish();

        let feature_row_color: ColorU = theme.foreground().into();
        let feature_x_fill: ThemeFill = ThemeFill::Solid(theme.ansi_fg_red());
        let mut feature_list =
            Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        let feature_items: &[&str] = if is_terminal {
            drive_features()
        } else {
            ai_features()
        };
        for &item in feature_items {
            let icon_el = ConstrainedBox::new(Icon::X.to_warpui_icon(feature_x_fill).finish())
                .with_width(16.)
                .with_height(16.)
                .finish();
            let text_el = FormattedTextElement::from_str(item, appearance.ui_font_family(), 14.)
                .with_color(feature_row_color)
                .with_weight(Weight::Normal)
                .with_alignment(TextAlignment::Left)
                .with_line_height_ratio(1.0)
                .finish();
            let row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(icon_el)
                .with_child(Container::new(text_el).with_margin_left(4.).finish())
                .finish();
            feature_list = feature_list.with_child(
                Container::new(row)
                    .with_padding_top(4.)
                    .with_padding_bottom(4.)
                    .finish(),
            );
        }

        let body_section = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(body_text)
            .with_child(
                Container::new(feature_list.finish())
                    .with_margin_top(12.)
                    .finish(),
            )
            .finish();

        let cancel_label = if is_terminal {
            format!("Enable {}", drive_name())
        } else {
            "Enable AI features".to_string()
        };
        let login_button = self.dialog_login_button.render(
            appearance,
            button::Params {
                content: button::Content::Label(cancel_label.into()),
                theme: &button::themes::Naked,
                options: button::Options {
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::DismissDialog);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let dialog_enter = Keystroke::parse("enter").unwrap_or_default();
        let skip_confirm_button = self.dialog_skip_button.render(
            appearance,
            button::Params {
                content: button::Content::Label("Skip for now".into()),
                theme: &button::themes::Primary,
                options: button::Options {
                    keystroke: Some(dialog_enter),
                    on_click: Some(Box::new(|ctx, _app, _pos| {
                        ctx.dispatch_typed_action(LoginSlideAction::ConfirmSkip);
                    })),
                    ..button::Options::default(appearance)
                },
            },
        );

        let footer = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::End)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(login_button)
                .with_child(
                    Container::new(skip_confirm_button)
                        .with_margin_left(8.)
                        .finish(),
                )
                .finish(),
        )
        .with_border(Border::top(1.).with_border_color(border_color))
        .with_horizontal_padding(24.)
        .with_vertical_padding(12.)
        .finish();

        let dialog = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(title_row)
                    .with_horizontal_padding(24.)
                    .with_padding_top(24.)
                    .with_padding_bottom(12.)
                    .finish(),
            )
            .with_child(
                Container::new(body_section)
                    .with_horizontal_padding(24.)
                    .with_padding_bottom(16.)
                    .finish(),
            )
            .with_child(footer)
            .finish();

        ConstrainedBox::new(
            Container::new(dialog)
                .with_background(dialog_surface)
                .with_border(Border::all(1.).with_border_color(border_color))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                .finish(),
        )
        .with_width(460.)
        .finish()
    }
}

// ---------------------------------------------------------------------------
// Entity / View / TypedActionView
// ---------------------------------------------------------------------------

impl Entity for LoginSlideView {
    type Event = LoginSlideEvent;
}

impl View for LoginSlideView {
    fn ui_name() -> &'static str {
        "LoginSlideView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.notify();
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut stack = Stack::new();

        // Background (same as onboarding parent)
        if let Some(img) = theme.background_image() {
            stack.add_child(
                Shrinkable::new(
                    1.,
                    Image::new(img.source(), CacheOption::Original)
                        .cover()
                        .finish(),
                )
                .finish(),
            );
            let overlay_opacity = (100u8).saturating_sub(img.opacity);
            stack.add_child(
                warpui::elements::Rect::new()
                    .with_background(theme.background().with_opacity(overlay_opacity))
                    .finish(),
            );
        } else {
            stack.add_child(
                Container::new(warpui::elements::Empty::new().finish())
                    .with_background(theme.background())
                    .finish(),
            );
        }

        #[cfg(not(target_family = "wasm"))]
        let slide = if self.should_render_provider_setup()
            && matches!(self.step, LoginStep::SelectAuthPathway)
        {
            let editor_rendered = Cell::new(true);
            Align::new(
                ConstrainedBox::new(self.render_content(appearance, app, &editor_rendered))
                    .with_max_width(940.)
                    .finish(),
            )
            .finish()
        } else {
            // static_left calls the left closure twice (narrow + wide). We use
            // a Cell<bool> so the editor ChildView is only created once.
            let editor_rendered = Cell::new(false);
            layout::static_left(
                || self.render_content(appearance, app, &editor_rendered),
                || self.render_visual(),
            )
        };
        #[cfg(target_family = "wasm")]
        let slide = {
            // static_left calls the left closure twice (narrow + wide). We use
            // a Cell<bool> so the editor ChildView is only created once.
            let editor_rendered = Cell::new(false);
            layout::static_left(
                || self.render_content(appearance, app, &editor_rendered),
                || self.render_visual(),
            )
        };
        stack.add_child(slide);

        // Skip dialog overlay
        if matches!(self.active_overlay, Some(LoginSlideOverlay::SkipDialog)) {
            let dialog = self.render_skip_dialog(appearance);
            let centered = Align::new(dialog).finish();
            stack.add_child(
                Dismiss::new(centered)
                    .on_dismiss(|ctx, _app| {
                        ctx.dispatch_typed_action(LoginSlideAction::DismissDialog);
                    })
                    .finish(),
            );
        }

        // Login failure notification
        if let Some(login_failure_reason) = &self.last_login_failure_reason {
            let notification = login_failure_notification::render(
                login_failure_reason,
                self.close_login_notification_mouse_state.clone(),
                self.highlighted_hyperlink_state.clone(),
                LoginSlideAction::DismissNotification,
                app,
            );
            stack.add_positioned_overlay_child(
                notification,
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 40.),
                    ParentOffsetBounds::ParentBySize,
                    ParentAnchor::TopMiddle,
                    ChildAnchor::TopMiddle,
                ),
            );
        }

        stack.finish()
    }
}

impl TypedActionView for LoginSlideView {
    type Action = LoginSlideAction;

    fn handle_action(&mut self, action: &LoginSlideAction, ctx: &mut ViewContext<Self>) {
        match action {
            LoginSlideAction::Enter => {
                // When the skip dialog is open, Enter should confirm skip instead.
                if self.active_overlay.is_some() {
                    self.active_overlay = None;
                    self.handle_login_later(ctx);
                    return;
                }
                #[cfg(not(target_family = "wasm"))]
                if self.should_render_provider_setup() {
                    if self.is_provider_connection_verified(ctx) {
                        if self.persist_provider_setup(false, ctx).is_ok() {
                            ctx.emit(LoginSlideEvent::LoginLaterConfirmed);
                        }
                    } else if self.persist_provider_setup(false, ctx).is_ok() {
                        LocalMultiAgentManager::handle(ctx).update(ctx, |manager, ctx| {
                            manager.test_backend(ctx);
                        });
                    }
                    ctx.notify();
                    return;
                }
                // Otherwise Enter is log in
                send_telemetry_from_ctx!(
                    TelemetryEvent::LoginButtonClicked {
                        source: LoginEventSource::OnboardingSlide,
                    },
                    ctx
                );
                self.last_login_failure_reason = None;
                self.step = LoginStep::BrowserOpen;
                AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                    let sign_up_url = auth_manager.sign_up_url();
                    ctx.open_url(&sign_up_url);
                });
                ctx.notify();
            }
            LoginSlideAction::ShowSkipDialog => {
                send_telemetry_from_ctx!(
                    TelemetryEvent::LoginLaterButtonClicked {
                        source: LoginEventSource::OnboardingSlide,
                    },
                    ctx
                );
                self.active_overlay = Some(LoginSlideOverlay::SkipDialog);
                ctx.notify();
            }
            LoginSlideAction::ConfirmSkip => {
                self.active_overlay = None;
                self.handle_login_later(ctx);
            }
            LoginSlideAction::DismissDialog => {
                self.active_overlay = None;
                ctx.notify();
            }
            LoginSlideAction::DismissOverlayOrBack => {
                if self.active_overlay.is_some() {
                    self.active_overlay = None;
                    ctx.notify();
                } else if matches!(self.step, LoginStep::PrivacySettings) {
                    match self.source {
                        LoginSlideSource::PrivacySettingsFromTerminalIntentionTheme => {
                            ctx.emit(LoginSlideEvent::BackToOnboarding);
                        }
                        LoginSlideSource::OnboardingFlow
                        | LoginSlideSource::LoginExistingUserFromWelcome => {
                            self.step = LoginStep::SelectAuthPathway;
                            ctx.focus_self();
                            ctx.notify();
                        }
                    }
                } else if matches!(self.step, LoginStep::BrowserOpen) {
                    // PrivacySettingsFromTerminalIntentionTheme starts on the
                    // privacy-settings step and should never transition into the
                    // select-auth-pathway step. If this branch is ever reached
                    // for that source, route back to onboarding instead.
                    match self.source {
                        LoginSlideSource::LoginExistingUserFromWelcome
                        | LoginSlideSource::PrivacySettingsFromTerminalIntentionTheme => {
                            ctx.emit(LoginSlideEvent::BackToOnboarding);
                        }
                        LoginSlideSource::OnboardingFlow => {
                            self.step = LoginStep::SelectAuthPathway;
                            ctx.focus_self();
                            ctx.notify();
                        }
                    }
                } else {
                    ctx.emit(LoginSlideEvent::BackToOnboarding);
                }
            }
            LoginSlideAction::Back => {
                ctx.emit(LoginSlideEvent::BackToOnboarding);
            }
            LoginSlideAction::BackToSelectAuthPathway => match self.source {
                // PrivacySettingsFromTerminalIntentionTheme only ever shows the
                // privacy-settings step; treat "back" the same as login-from-
                // welcome and return to onboarding rather than falling through
                // to a step this source was designed to skip.
                LoginSlideSource::LoginExistingUserFromWelcome
                | LoginSlideSource::PrivacySettingsFromTerminalIntentionTheme => {
                    ctx.emit(LoginSlideEvent::BackToOnboarding);
                }
                LoginSlideSource::OnboardingFlow => {
                    self.step = LoginStep::SelectAuthPathway;
                    ctx.focus_self();
                    ctx.notify();
                }
            },
            LoginSlideAction::CopyLoginUrl => {
                AuthManager::handle(ctx).update(ctx, |auth_manager, inner_ctx| {
                    let sign_in_url = auth_manager.sign_in_url();
                    inner_ctx.clipboard().write(ClipboardContent {
                        plain_text: sign_in_url.clone(),
                        paths: Some(vec![sign_in_url]),
                        ..Default::default()
                    });
                });
            }
            LoginSlideAction::EnterToken => {
                self.auth_token_input
                    .update(ctx, |editor, ctx| editor.paste(ctx));
                self.show_auth_token_input = true;
                ctx.notify();
            }
            LoginSlideAction::ShowPrivacySettings => {
                if ChannelState::product_name() == "Slipstream" {
                    return;
                }
                send_telemetry_sync_from_ctx!(
                    TelemetryEvent::OpenAuthPrivacySettings {
                        source: LoginEventSource::OnboardingSlide,
                    },
                    ctx
                );
                self.step = LoginStep::PrivacySettings;
                ctx.notify();
            }
            LoginSlideAction::HideOverlay => {
                // "Done" button in privacy settings returns to the auth pathway step,
                // except when the user entered the slide via the terminal-intention theme slide's
                // Privacy Settings link — in that case Back returns to the onboarding view.
                self.active_overlay = None;
                match self.source {
                    LoginSlideSource::PrivacySettingsFromTerminalIntentionTheme => {
                        ctx.emit(LoginSlideEvent::BackToOnboarding);
                    }
                    LoginSlideSource::OnboardingFlow
                    | LoginSlideSource::LoginExistingUserFromWelcome => {
                        self.step = LoginStep::SelectAuthPathway;
                        ctx.focus_self();
                        ctx.notify();
                    }
                }
            }
            LoginSlideAction::ToggleTelemetry => {
                let handle = PrivacySettings::handle(ctx);
                ctx.update_model(&handle, |settings, ctx| {
                    settings.set_is_telemetry_enabled(!settings.is_telemetry_enabled, ctx);
                });
                ctx.notify();
            }
            LoginSlideAction::ToggleCrashReporting => {
                let handle = PrivacySettings::handle(ctx);
                ctx.update_model(&handle, |settings, ctx| {
                    settings
                        .set_is_crash_reporting_enabled(!settings.is_crash_reporting_enabled, ctx);
                });
                ctx.notify();
            }
            LoginSlideAction::ToggleCloudConversationStorage => {
                let handle = PrivacySettings::handle(ctx);
                ctx.update_model(&handle, |settings, ctx| {
                    settings.set_is_cloud_conversation_storage_enabled(
                        !settings.is_cloud_conversation_storage_enabled,
                        ctx,
                    );
                });
                ctx.notify();
            }
            LoginSlideAction::DismissNotification => {
                self.last_login_failure_reason = None;
                ctx.notify();
            }
            LoginSlideAction::PasteAuthUrl => {
                self.last_login_failure_reason = None;
                let clipboard_content = ctx.clipboard().read();
                if !clipboard_content.plain_text.is_empty() {
                    self.handle_pasted_auth_url(clipboard_content.plain_text, ctx);
                }
            }
            LoginSlideAction::TestProviderConnection => {
                #[cfg(not(target_family = "wasm"))]
                {
                    if self.persist_provider_setup(false, ctx).is_ok() {
                        LocalMultiAgentManager::handle(ctx).update(ctx, |manager, ctx| {
                            manager.test_backend(ctx);
                        });
                    }
                    ctx.notify();
                }
            }
            LoginSlideAction::SetProviderDefaultModel(model) => {
                #[cfg(not(target_family = "wasm"))]
                {
                    LocalMultiAgentManager::handle(ctx).update(ctx, |manager, ctx| {
                        if let Err(error) = manager.set_openai_model(Some(model.clone()), ctx) {
                            manager.record_config_error(error.to_string(), ctx);
                        }
                    });
                    self.provider_setup_error = None;
                    ctx.notify();
                }
            }
        }
    }
}
