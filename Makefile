# ─────────────────────────────────────────────────────────────────────────────
# GIAP — Jetson Orin Nano Build Targets
#
# Cross-compilation (from macOS or x86 Linux):
#   Prerequisites: cargo install cross --locked  +  Docker running
#
# Native build (on the Jetson itself):
#   Run:  bash scripts/build-jetson-native.sh [--cuda]
# ─────────────────────────────────────────────────────────────────────────────

JETSON_TARGET := aarch64-unknown-linux-gnu
JETSON_HOST   ?= jetson@192.168.1.100
DEPLOY_DIR    ?= /opt/giap

SERVER_BIN    := target/$(JETSON_TARGET)/release/pond-server

.PHONY: server server-cuda desktop deploy help

## server        Cross-compile pond-server for Jetson (CPU only, no CUDA)
server:
	SQLX_OFFLINE=true cross build -p pond-server \
	  --target $(JETSON_TARGET) \
	  --release
	@echo ""
	@echo "Binary: $(SERVER_BIN)"

## server-cuda   Show native CUDA build command (run on the Jetson, not here)
server-cuda:
	@echo ""
	@echo "CUDA cross-compilation requires the CUDA ARM64 sysroot and is not"
	@echo "supported via the cross tool. Build natively on the Jetson instead:"
	@echo ""
	@echo "  ssh $(JETSON_HOST)"
	@echo "  cd /path/to/giap"
	@echo "  SQLX_OFFLINE=true cargo build -p pond-server \\"
	@echo "    --features pond-adapters-local-inference/cuda \\"
	@echo "    --release"
	@echo ""
	@echo "Or run the helper script:"
	@echo "  bash scripts/build-jetson-native.sh --cuda"
	@echo ""

## desktop       Cross-compile Tauri desktop for ARM64 Linux (deb bundle)
desktop:
	@echo "Building frontend..."
	cd pond-desktop && npm run build
	@echo "Building Tauri app for $(JETSON_TARGET)..."
	cd pond-desktop && cargo tauri build \
	  --target $(JETSON_TARGET) \
	  --bundles deb
	@echo ""
	@echo "Bundle: pond-desktop/src-tauri/target/$(JETSON_TARGET)/release/bundle/"

## deploy        Cross-compile + scp server binary to Jetson
##               Override host: make deploy JETSON_HOST=user@ip
deploy: server
	@echo "Deploying to $(JETSON_HOST):$(DEPLOY_DIR)..."
	ssh $(JETSON_HOST) "mkdir -p $(DEPLOY_DIR)"
	scp $(SERVER_BIN) $(JETSON_HOST):$(DEPLOY_DIR)/pond-server
	@echo "Done. Run on device:"
	@echo "  ssh $(JETSON_HOST) '$(DEPLOY_DIR)/pond-server'"

## help          Show this help
help:
	@grep -E '^## ' Makefile | sed 's/## /  /'
