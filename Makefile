LOCAL_AGENT_DIR := tools/local-multi-agent
LOCAL_AGENT_NPM_PATH := /opt/homebrew/bin:/usr/local/bin:$(PATH)

.DEFAULT_GOAL := help

.PHONY: help local-agent-install local-agent-dev local-agent-build local-agent-start local-agent-test local-agent-proto warp-local-signing-identity warp-signing-status warp-grant-keychain-access warp-trash-local-settings warp-check warp-build warp-build-optimized warp-build-oss

help:
	@echo "Slipstream local development targets:"
	@echo "  make local-agent-install  Install local multi-agent service dependencies"
	@echo "  make local-agent-dev      Run the local multi-agent service in watch mode"
	@echo "  make local-agent-build    Build the local multi-agent service"
	@echo "  make local-agent-start    Run the built local multi-agent service"
	@echo "  make local-agent-test     Build and test the local multi-agent service"
	@echo "  make local-agent-proto    Regenerate local multi-agent TypeScript protobuf bindings"
	@echo "  make warp-local-signing-identity  Create a stable local macOS signing identity"
	@echo "  make warp-signing-status  Show available macOS code-signing identities"
	@echo "  make warp-grant-keychain-access  Grant existing Slipstream keychain items to the signed app Team ID"
	@echo "  make warp-trash-local-settings  Move local Slipstream settings/state to the macOS Trash"
	@echo "  make warp-check           Run Rust formatting and Slipstream app check"
	@echo "  make warp-build-oss       Build the Slipstream macOS app bundle"
	@echo "  make warp-build-optimized Build an optimized Slipstream macOS app bundle"

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

warp-signing-status:
	./script/macos/signing_status

warp-grant-keychain-access:
	./script/macos/grant_keychain_access_to_team

warp-trash-local-settings:
	@bash -eu -o pipefail -c '\
		if [ "$$(uname -s)" != "Darwin" ]; then \
			echo "warp-trash-local-settings currently supports macOS only."; \
			exit 1; \
		fi; \
		ts=$$(date +%Y%m%d-%H%M%S); \
		trash_dir="$$HOME/.Trash/slipstream-local-settings-$$ts"; \
		mkdir -p "$$trash_dir"; \
		paths=( \
			"$$HOME/.slipstream" \
			"$$HOME/Library/Application Support/com.slipstream.app" \
			"$$HOME/Library/Caches/com.slipstream.app" \
			"$$HOME/Library/Preferences/com.slipstream.app.plist" \
			"$$HOME/Library/Saved Application State/com.slipstream.app.savedState" \
		); \
		services=( "com.slipstream.app" ); \
		if [ -n "$${WARP_DATA_PROFILE:-}" ]; then \
			paths+=( \
				"$$HOME/.slipstream-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Application Support/com.slipstream.app-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Caches/com.slipstream.app-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Preferences/com.slipstream.app-$${WARP_DATA_PROFILE}.plist" \
				"$$HOME/Library/Saved Application State/com.slipstream.app-$${WARP_DATA_PROFILE}.savedState" \
			); \
			services+=( "com.slipstream.app-$${WARP_DATA_PROFILE}" ); \
		fi; \
		if [ -n "$${SLIPSTREAM_APP_GROUP_ID:-}" ]; then \
			paths+=( "$$HOME/Library/Group Containers/$${SLIPSTREAM_APP_GROUP_ID}/Library/Application Support/com.slipstream.app" ); \
		fi; \
		moved=0; \
		for path in "$${paths[@]}"; do \
			if [ -e "$$path" ]; then \
				dest="$$trash_dir$${path#$$HOME}"; \
				mkdir -p "$$(dirname "$$dest")"; \
				echo "Moving $$path"; \
				mv "$$path" "$$dest"; \
				moved=1; \
			fi; \
		done; \
		for service in "$${services[@]}"; do \
			for key in User AiApiKeys McpCredentials TemplatableMcpCredentials FileBasedMcpCredentials; do \
				security delete-generic-password -s "$$service" -a "$$key" >/dev/null 2>&1 || true; \
			done; \
		done; \
		defaults delete com.slipstream.app >/dev/null 2>&1 || true; \
		killall cfprefsd >/dev/null 2>&1 || true; \
		if [ "$$moved" -eq 0 ]; then \
			rmdir "$$trash_dir"; \
			echo "No local Slipstream settings/state paths found."; \
		else \
			echo "Moved local Slipstream settings/state to $$trash_dir"; \
		fi; \
		echo "Deleted Slipstream Keychain entries for user, API key, and MCP credentials if present."; \
	'

warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

warp-build:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/macos/run --dont-open

warp-build-optimized:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=unpacked ./script/macos/run --dont-open --profile release-lto

warp-build-oss: warp-build
