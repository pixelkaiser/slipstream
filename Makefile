LOCAL_AGENT_DIR := tools/local-multi-agent

.PHONY: local-agent-install local-agent-dev local-agent-build local-agent-start local-agent-test warp-check warp-build warp-build-oss

local-agent-install:
	cd $(LOCAL_AGENT_DIR) && npm install

local-agent-dev:
	cd $(LOCAL_AGENT_DIR) && npm run dev

local-agent-build:
	cd $(LOCAL_AGENT_DIR) && npm run build

local-agent-start:
	cd $(LOCAL_AGENT_DIR) && npm start

local-agent-test:
	cd $(LOCAL_AGENT_DIR) && npm test

warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

warp-build:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/macos/run --dont-open

warp-build-oss: warp-build
