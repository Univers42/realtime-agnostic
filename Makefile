.PHONY: help build test up down logs clean seed status dev audit

.DEFAULT_GOAL := help

COMPOSE := docker compose -f sandbox/docker-compose.yml
PROJECT := realtime-agnostic

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build: ## Build the Rust workspace (release)
	cargo build --release --workspace

test: ## Run all tests (78 unit + integration)
	cargo test --workspace

check: ## Check compilation with zero warnings
	cargo check --workspace 2>&1 | grep -v "Compiling\|Checking\|Finished"

audit: ## Run full code audit (fmt, clippy, tests)
	@echo "Formatting..."
	cargo fmt --all -- --check
	@echo "Running clippy..."
	cargo clippy --all-targets --all-features
	@echo "Running tests..."
	cargo test --workspace

up: ## Start databases + server via Docker Compose
	$(COMPOSE) up --build -d
	@echo ""
	@echo "  ┌──────────────────────────────────────────────┐"
	@echo "  │  SyncSpace is starting...                    │"
	@echo "  │                                              │"
	@echo "  │  Web UI:    http://localhost:4002             │"
	@echo "  │  Health:    http://localhost:4002/v1/health   │"
	@echo "  │  WebSocket: ws://localhost:4002/ws            │"
	@echo "  │                                              │"
	@echo "  │  PostgreSQL: localhost:5434                   │"
	@echo "  │  MongoDB:    localhost:27019                  │"
	@echo "  │                                              │"
	@echo "  │  Logs:  make logs                            │"
	@echo "  │  Stop:  make down                            │"
	@echo "  └──────────────────────────────────────────────┘"
	@echo ""

down: ## Stop all containers
	$(COMPOSE) down

clean: ## Stop containers AND remove volumes (fresh start)
	$(COMPOSE) down -v --remove-orphans
	@echo "Cleaned all containers and volumes."

logs: ## Tail all container logs
	$(COMPOSE) logs -f

logs-server: ## Tail only the Rust server logs
	$(COMPOSE) logs -f realtime-server

logs-pg: ## Tail PostgreSQL logs
	$(COMPOSE) logs -f postgres

logs-mongo: ## Tail MongoDB logs
	$(COMPOSE) logs -f mongo

status: ## Show running containers
	$(COMPOSE) ps

restart: ## Restart the server (rebuild)
	$(COMPOSE) up --build -d realtime-server

seed: ## Re-seed databases (requires running containers)
	@echo "Re-seeding PostgreSQL..."
	$(COMPOSE) exec -T postgres psql -U syncspace syncspace < sandbox/db/postgres-seed.sql
	@echo "Re-seeding MongoDB..."
	$(COMPOSE) exec -T mongo mongosh syncspace /seed.js || \
		docker run --rm --network realtime-agnostic_default \
			-v $(PWD)/sandbox/db/mongo-seed.js:/seed.js:ro \
			mongo:7 mongosh --host mongo syncspace /seed.js
	@echo "Seed complete."

dev: ## Run Rust server locally and open browser (expects PG:5432, Mongo:27017)
	@echo "Stopping existing local servers..."
	-@pkill -x realtime-server || true
	@echo "Starting Rust server on http://localhost:4001 and opening browser..."
	@sleep 2 && (xdg-open http://localhost:4001 || open http://localhost:4001) >/dev/null 2>&1 &
	RUST_LOG=info,realtime_gateway=debug,realtime_server=debug \
	REALTIME_HOST=0.0.0.0 \
	REALTIME_PORT=4001 \
	REALTIME_STATIC_DIR=sandbox/static \
	cargo run --bin realtime-server

psql: ## Open psql shell to the database
	$(COMPOSE) exec postgres psql -U syncspace syncspace

mongo-shell: ## Open mongosh to the database
	$(COMPOSE) exec mongo mongosh syncspace

health: ## Check server health endpoint
	@curl -s http://localhost:4002/v1/health | python3 -m json.tool 2>/dev/null || \
		echo "Server not reachable at localhost:4002"

test-publish: ## Publish a test event via REST API
	@curl -s -X POST http://localhost:4002/v1/publish \
		-H "Content-Type: application/json" \
		-d '{"topic":"board:board-roadmap/card.created","event_type":"card.created","payload":{"id":"test-card","list_id":"list-backlog","title":"Test Card from Makefile","position":99,"assignee_id":"user-alice","label_color":"#ef4444","created_by":"user-alice"}}' \
		| python3 -m json.tool 2>/dev/null || echo "Publish failed"

test-ws: ## Quick WebSocket test (requires websocat)
	@echo '{"type":"AUTH","token":"test"}' | \
		timeout 3 websocat ws://localhost:4002/ws 2>/dev/null || \
		echo "Install websocat: cargo install websocat"
