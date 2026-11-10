.PHONY: help build run run-dev tui install auth logout sync today today-json add fmt clippy test perf check clean

.DEFAULT_GOAL := help

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

build: ## Build release binary (target/release/todoist-tui)
	cargo build --release

run: ## Build (if needed) and launch the interactive TUI (release)
	cargo run --release

run-dev: ## Build (if needed) and launch the interactive TUI (debug)
	cargo run

tui: ## Launch the interactive TUI via the explicit `tui` subcommand
	cargo run --release -- tui

install: ## Install `todoist-tui` into Cargo bin path (--locked)
	cargo install --path . --locked

auth: ## Prompt for and store Todoist API token
	cargo run --release -- auth

logout: ## Remove stored token and wipe local cache
	cargo run --release -- logout

sync: ## Flush offline queue and sync cache with Todoist
	cargo run --release -- sync

today: ## Show tasks due today and overdue
	cargo run --release -- today

today-json: ## Show tasks due today as JSON
	cargo run --release -- today --json

add: ## Quick-add a task (usage: make add TEXT="Buy milk tom")
	@test -n "$(TEXT)" || (echo 'Usage: make add TEXT="Buy milk tom"' && exit 1)
	cargo run --release -- add -- "$(TEXT)"

fmt: ## Check Rust formatting
	cargo fmt --all -- --check

clippy: ## Run strict clippy lints across all targets
	cargo clippy --all-targets -- -D warnings

test: ## Run full workspace test suite
	cargo test --all

perf: ## Run release 5k-item performance gates
	cargo test --release --test perf_gates -- --ignored --nocapture
	cargo test --release --test tui_perf _5k_items_under_ -- --nocapture

check: fmt clippy test ## Run fmt, clippy, and workspace tests

clean: ## Remove build artifacts
	cargo clean

