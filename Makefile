LOCAL_AGENT_DIR := tools/local-multi-agent
LOCAL_AGENT_NPM_PATH := /opt/homebrew/bin:/usr/local/bin:$(PATH)

.DEFAULT_GOAL := help

.PHONY: help local-agent-install local-agent-dev local-agent-build local-agent-start local-agent-test local-agent-proto warp-local-signing-identity warp-check warp-build warp-build-optimized warp-build-oss

help:
	@echo "Warp BYOK local development targets:"
	@echo "  make local-agent-install  Install local multi-agent service dependencies"
	@echo "  make local-agent-dev      Run the local multi-agent service in watch mode"
	@echo "  make local-agent-build    Build the local multi-agent service"
	@echo "  make local-agent-start    Run the built local multi-agent service"
	@echo "  make local-agent-test     Build and test the local multi-agent service"
	@echo "  make local-agent-proto    Regenerate local multi-agent TypeScript protobuf bindings"
	@echo "  make warp-local-signing-identity  Create a stable local macOS signing identity"
	@echo "  make warp-check           Run Rust formatting and Warp OSS app check"
	@echo "  make warp-build-oss       Build the Warp OSS macOS app bundle"
	@echo "  make warp-build-optimized Build an optimized Warp OSS macOS app bundle"

local-agent-install:
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm install

local-agent-dev:
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm run dev

local-agent-build:
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm run build

local-agent-start: local-agent-build
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm start

local-agent-test:
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm test

local-agent-proto:
	cd $(LOCAL_AGENT_DIR) && PATH="$(LOCAL_AGENT_NPM_PATH)" npm run proto:generate

warp-local-signing-identity:
	./script/macos/create_local_codesign_identity

warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

warp-build:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/macos/run --dont-open

warp-build-optimized:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=unpacked ./script/macos/run --dont-open --profile release-lto

warp-build-oss: warp-build
