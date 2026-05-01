# Slipstream

Slipstream is a privacy- and performance-focused fork of [Warp](https://github.com/warpdotdev/warp). It keeps the Warp terminal and agent experience, but adapts it for local and self-hosted inference with no calls to Warp-hosted cloud services by default.

The goal is simple: use a modern terminal with agentic workflows while keeping your prompts, files, terminal context, conversations, integrations, and model traffic under your control.

Slipstream is based on the upstream Warp open-source project:

- Upstream project: [warpdotdev/warp](https://github.com/warpdotdev/warp)
- Upstream README: [warpdotdev/warp README](https://github.com/warpdotdev/warp#readme)

## Installation

macOS users can download the latest `.dmg` from the [Slipstream GitHub Releases page](https://github.com/pixelkaiser/slipstream/releases).

You do not need to build the project from source to use Slipstream on macOS.

## Local Inference

Slipstream is designed to work out of the box with any OpenAI-compatible API, including LM Studio, vLLM, LocalAI, Ollama-compatible OpenAI endpoints, and private or self-hosted gateways.

To get started:

1. Install Slipstream from the macOS `.dmg`.
2. Start your local or self-hosted OpenAI-compatible provider.
3. In Slipstream, enter the provider base URL, model, and API key if your provider requires one.

For true zero-cloud use, point Slipstream at a model provider running on your own machine, local network, or self-hosted infrastructure. If you point it at an external hosted API, your model traffic goes to that provider instead.

## What Changed From Warp

Slipstream tracks Warp, but changes the default operating model:

- **No-cloud defaults:** Slipstream starts in local mode and avoids Warp-hosted services by default.
- **Local inference:** The Warp agent experience connects to OpenAI-compatible local or self-hosted model APIs.
- **Local state:** Conversations, integrations, tool connector configuration, and diagnostics are stored locally.
- **Local tools and MCP:** Agent tool calls and MCP tool connectors are handled through the local backend.
- **Privacy-oriented app config:** Telemetry, crash reporting, hosted autoupdate configuration, and promotional cloud UI are disabled or removed for the Slipstream OSS app.
- **Slipstream packaging:** The app is rebranded and packaged separately from upstream Warp, with macOS release artifacts published through GitHub Releases.
- **Self-hosted sharing:** Session sharing can be routed through self-hosted infrastructure instead of Warp cloud services.

## Bugs And Issues

General Warp bugs, terminal behavior issues, UI issues, and upstream feature requests should be filed against [warpdotdev/warp issues](https://github.com/warpdotdev/warp/issues).

This fork only tracks Slipstream-specific local inference and no-cloud issues, such as:

- local backend startup problems
- provider configuration or model discovery problems
- local agent streaming issues
- local tool-call or MCP behavior
- no-cloud routing regressions

## Maintainers Wanted

We are looking for a primary Linux maintainer. If you care about a strong Linux build and packaging story for Slipstream, this is the most useful place to help.

## Developer Notes

End users should start with the macOS release download. Build-from-source instructions, local backend commands, environment variables, and contributor troubleshooting live in [LOCAL_DEVELOPMENT.md](LOCAL_DEVELOPMENT.md).

## Licensing

Warp's UI framework, the `warpui_core` and `warpui` crates, are licensed under the [MIT license](LICENSE-MIT).

The rest of the code in this repository is licensed under the [AGPL v3](LICENSE-AGPL).

## Open Source Dependencies

Slipstream inherits Warp's open-source foundation. Notable dependencies include:

- [Tokio](https://github.com/tokio-rs/tokio)
- [NuShell](https://github.com/nushell/nushell)
- [Fig Completion Specs](https://github.com/withfig/autocomplete)
- [Warp Server Framework](https://github.com/seanmonstar/warp)
- [Alacritty](https://github.com/alacritty/alacritty)
- [Hyper HTTP library](https://github.com/hyperium/hyper)
- [FontKit](https://github.com/servo/font-kit)
- [Core-foundation](https://github.com/servo/core-foundation-rs)
- [Smol](https://github.com/smol-rs/smol)
