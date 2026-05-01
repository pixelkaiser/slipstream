use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use settings::{
    macros::define_settings_group, RespectUserSyncSetting, Setting, SupportedPlatforms, SyncToCloud,
};
use url::Url;
use warp_core::{channel::ChannelState, features::FeatureFlag};
use warpui::{AppContext, SingletonEntity};

pub const SESSION_SHARING_SERVER_URL_PLACEHOLDER: &str = "ws://127.0.0.1:8788";

define_settings_group!(SharedSessionSettings, settings: [
    onboarding_block_shown: SessionSharingOnboardingBlockShown {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    inactivity_period_before_ending_session: InactivityPeriodBeforeEndingSession {
        type: Duration,
        // After a total of 30 min of inactivity, we will end the session
        default: Duration::from_secs(1800),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    inactivity_period_before_warning: InactivityPeriodBeforeWarning {
        type: Duration,
        // After a total of 25 min of inactivity, we will show a warning modal
        default: Duration::from_secs(1500),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    inactivity_period_before_revoking_roles: InactivityPeriodBeforeRevokingRoles {
        type: Duration,
        // After a total of 10 min of inactivity, we will revoke all executor roles
        default: Duration::from_secs(600),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    // Killswitch: when false, the sharer ignores viewer terminal size reports.
    viewer_driven_sizing_enabled: ViewerDrivenSizingEnabled {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: true,
    },
    session_sharing_server_url: SessionSharingServerUrl {
        type: String,
        default: String::new(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        toml_path: "cloud_platform.sharing.session_sharing_server_url",
        description: "Self-hosted session sharing relay WebSocket URL used in no-cloud mode.",
    },
]);

impl SharedSessionSettings {
    /// Returns time between showing the inactivity warning modal and ending the session.
    pub fn inactivity_period_between_warning_and_ending_session(&self) -> Duration {
        *self.inactivity_period_before_ending_session.value()
            - *self.inactivity_period_before_warning.value()
    }

    /// Returns time between revoking roles and showing the inactivity warning modal.
    pub fn inactivity_period_between_revoking_roles_and_warning(&self) -> Duration {
        *self.inactivity_period_before_warning.value()
            - *self.inactivity_period_before_revoking_roles.value()
    }

    pub fn configured_session_sharing_server_url(&self) -> Option<&str> {
        let url = self.session_sharing_server_url.value().trim();
        (!url.is_empty()).then_some(url)
    }
}

pub fn normalize_session_sharing_server_url(value: &str) -> anyhow::Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let parsed = Url::parse(trimmed).with_context(|| {
        format!("Session sharing relay URL must be a websocket URL, for example {SESSION_SHARING_SERVER_URL_PLACEHOLDER}")
    })?;

    match parsed.scheme() {
        "ws" | "wss" => Ok(Some(trimmed.to_owned())),
        scheme => bail!("Session sharing relay URL must use ws:// or wss://, not {scheme}://"),
    }
}

pub fn apply_session_sharing_server_url(
    relay_url: Option<&str>,
    update_feature_flag: bool,
) -> anyhow::Result<()> {
    if !crate::server::server_api::no_cloud_mode_enabled() {
        return Ok(());
    }

    match relay_url {
        Some(relay_url) => {
            let normalized = normalize_session_sharing_server_url(relay_url)?
                .ok_or_else(|| anyhow!("Session sharing relay URL cannot be empty"))?;
            ChannelState::override_session_sharing_server_url(normalized)?;
            if update_feature_flag {
                FeatureFlag::CreatingSharedSessions.set_enabled(true);
            }
        }
        None => {
            ChannelState::clear_session_sharing_server_url();
            if update_feature_flag {
                FeatureFlag::CreatingSharedSessions.set_enabled(false);
            }
        }
    }

    Ok(())
}

pub fn apply_session_sharing_server_url_setting_on_startup(ctx: &AppContext) {
    if !crate::server::server_api::no_cloud_mode_enabled()
        || ChannelState::session_sharing_server_url().is_some()
    {
        return;
    }

    let Some(relay_url) =
        SharedSessionSettings::as_ref(ctx).configured_session_sharing_server_url()
    else {
        return;
    };

    if let Err(err) = apply_session_sharing_server_url(Some(relay_url), true) {
        log::error!("Failed to apply configured session sharing relay URL: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_session_sharing_server_url;

    #[test]
    fn normalize_session_sharing_server_url_accepts_ws_urls() {
        assert_eq!(
            normalize_session_sharing_server_url(" ws://127.0.0.1:8788 ").unwrap(),
            Some("ws://127.0.0.1:8788".to_owned())
        );
        assert_eq!(
            normalize_session_sharing_server_url("wss://relay.example.com").unwrap(),
            Some("wss://relay.example.com".to_owned())
        );
    }

    #[test]
    fn normalize_session_sharing_server_url_allows_empty_values() {
        assert_eq!(normalize_session_sharing_server_url("   ").unwrap(), None);
    }

    #[test]
    fn normalize_session_sharing_server_url_rejects_non_websocket_urls() {
        assert!(normalize_session_sharing_server_url("https://127.0.0.1:8788").is_err());
    }
}
