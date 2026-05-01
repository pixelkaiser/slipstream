use anyhow::Result;
use objc2_foundation::NSBundle;

/// Apple Developer Team ID used for code signing and validation.
pub fn apple_team_id() -> &'static str {
    option_env!("WARP_APPLE_TEAM_ID").unwrap_or("2BBY89MBSN")
}

/// Get the path to the macOS `.app` bundle.
pub fn get_bundle_path() -> Result<String> {
    let bundle = NSBundle::mainBundle();
    let path = bundle.bundlePath();
    Ok(path.to_string())
}
