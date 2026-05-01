// Onboarding library crate

mod agent_onboarding_view;
pub mod callout;
mod model;
pub mod slides;
pub mod telemetry;

/// The user's intention selected during onboarding slides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingIntention {
    Terminal,
    AgentDrivenDevelopment,
}

impl std::fmt::Display for OnboardingIntention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OnboardingIntention::AgentDrivenDevelopment => write!(f, "agent_driven"),
            OnboardingIntention::Terminal => write!(f, "terminal"),
        }
    }
}

pub use callout::{OnboardingCalloutView, OnboardingKeybindings};

/// User-facing descriptions of the AI features enabled when the agent intention is selected.
/// Shared by the intention slide's agent card checklist and the login slide's
/// skip-login confirmation dialog so the two always stay in sync.
const WARP_AI_FEATURES: &[&str] = &[
    "Use frontier and open-weight models with Warp Agent",
    "Hand off agent work to cloud agents",
    "Automatically diagnose and fix terminal errors",
    "Agentic control of long-running commands and TUIs",
    "Review code diffs and send comments directly to agents",
    "Remote control for Claude Code, Codex, and other agents",
];

const SLIPSTREAM_AI_FEATURES: &[&str] = &[
    "Use frontier and open-weight models with Slipstream Agent",
    "Hand off agent work to cloud agents",
    "Automatically diagnose and fix terminal errors",
    "Agentic control of long-running commands and TUIs",
    "Review code diffs and send comments directly to agents",
    "Remote control with Claude Code, Codex, and other agents",
];

/// User-facing names of the Drive features enabled when the terminal
/// intention is selected with Warp Drive turned on. Shared by the login slide's
/// skip-login confirmation dialog so the list stays in sync with any future
/// surfaces that need it.
const WARP_DRIVE_FEATURES: &[&str] = &["Warp Drive", "Session Sharing"];
const SLIPSTREAM_DRIVE_FEATURES: &[&str] = &["Slipstream Drive", "Session Sharing"];

pub fn ai_features() -> &'static [&'static str] {
    if warp_core::channel::ChannelState::product_name() == "Slipstream" {
        SLIPSTREAM_AI_FEATURES
    } else {
        WARP_AI_FEATURES
    }
}

pub fn drive_features() -> &'static [&'static str] {
    if warp_core::channel::ChannelState::product_name() == "Slipstream" {
        SLIPSTREAM_DRIVE_FEATURES
    } else {
        WARP_DRIVE_FEATURES
    }
}

pub fn drive_name() -> &'static str {
    if warp_core::channel::ChannelState::product_name() == "Slipstream" {
        "Slipstream Drive"
    } else {
        "Warp Drive"
    }
}

pub fn final_cta_label() -> &'static str {
    if warp_core::channel::ChannelState::product_name() == "Slipstream" {
        "Get Slipstreaming"
    } else {
        "Get Warping"
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "bin")] {
        mod telemetry_provider;
        pub use telemetry_provider::MockTelemetryContextProvider;
    }
}

pub mod components;
mod visuals;

/// The default mode for new sessions, chosen during onboarding.
/// Mapped to `DefaultSessionMode` at the application boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionDefault {
    #[default]
    Agent,
    Terminal,
}

impl std::fmt::Display for SessionDefault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionDefault::Agent => write!(f, "agent"),
            SessionDefault::Terminal => write!(f, "terminal"),
        }
    }
}

pub use agent_onboarding_view::{AgentOnboardingAction, AgentOnboardingEvent, AgentOnboardingView};
pub use model::{OnboardingAuthState, SelectedSettings, UICustomizationSettings};
pub use slides::ProjectOnboardingSettings;
pub use telemetry::OnboardingEvent;

pub fn init(app: &mut warpui_core::AppContext) {
    agent_onboarding_view::init(app);
    callout::init(app);
}
