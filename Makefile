.DEFAULT_GOAL := help
CHROME_DIR := $(HOME)/.aibrowse/chrome

.PHONY: help
help: ## List targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

.PHONY: chrome
chrome: ## Install the pinned Chrome for Testing into ~/.aibrowse/chrome
	npx --yes @puppeteer/browsers install chrome@stable --path $(CHROME_DIR)

.PHONY: build
build: ## Build the release binary
	cargo build --release -p aibrowse

# The installed copy is what MCP clients run. Replacing it atomically means a rebuild
# never corrupts a running server, which keeps executing the old inode.
INSTALL_DIR := $(HOME)/.aibrowse/bin

.PHONY: install
install: build ## Install aibrowse to ~/.aibrowse/bin (safe while it is running)
	@mkdir -p $(INSTALL_DIR)
	@install -m 755 target/release/aibrowse $(INSTALL_DIR)/aibrowse.new
	@mv -f $(INSTALL_DIR)/aibrowse.new $(INSTALL_DIR)/aibrowse
	@# Secrets for the installed server (the Jev key), readable only by this user.
	@if [ -f .env ]; then install -m 600 .env $(HOME)/.aibrowse/env; fi
	@echo "installed $(INSTALL_DIR)/aibrowse"

# Registers the installed binary as a user-scope MCP server for Claude Code (the
# configuration CLAUDE_CONFIG_DIR names, when set), in reload mode: new builds from
# `make install` go live on the next tool call, without restarting the agent session.
# The agent gets its own browser profile (PROFILE, default "claude"), separate from
# "default".
CLAUDE_BIN ?= claude
PROFILE ?= claude

.PHONY: register
register: install ## Register aibrowse as a user-scope MCP server for Claude Code
	-$(CLAUDE_BIN) mcp remove --scope user aibrowse >/dev/null 2>&1
	$(CLAUDE_BIN) mcp add --scope user aibrowse \
		-e AIBROWSE_PROFILE=$(PROFILE) -e AIBROWSE_HEADLESS=$(or $(HEADLESS),1) \
		-- $(INSTALL_DIR)/aibrowse reload

.PHONY: test
test: ## Unit and golden tests (no browser needed)
	cargo test --workspace

.PHONY: test-live
test-live: ## Tests against a real Chrome (opens a window; AIBROWSE_TEST_HEADLESS=1 to hide it)
	cargo test --workspace -- --ignored

.PHONY: lint
lint: ## Format check and clippy, warnings as errors
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings

.PHONY: fmt
fmt: ## Format the code
	cargo fmt

.PHONY: snapshots
snapshots: ## Review changed Page Model golden snapshots
	cargo insta review

.PHONY: fixture
fixture: ## Record a Page Model fixture: make fixture URL=https://… NAME=site-page
	@test -n "$(URL)" -a -n "$(NAME)" || (echo "usage: make fixture URL=… NAME=…" && exit 1)
	cargo run -q -p aibrowse -- fixture "$(URL)" "$(NAME)"

.PHONY: observe
observe: ## Print the Page Model for a URL: make observe URL=https://… [FULL=1]
	@test -n "$(URL)" || (echo "usage: make observe URL=…" && exit 1)
	cargo run -q -p aibrowse -- observe "$(URL)" $(if $(FULL),--full,)

# The container (Linux, headful Chromium under Xvfb): its targets live with it.
-include docker/docker.mk
