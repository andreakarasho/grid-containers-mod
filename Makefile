# ClassicUO checkout that receives the built mod (and whose SDK it builds against).
CUO_REPO ?= ../cuo-agents
NAME := grid-containers
RAW := target/wasm32-wasip1/release/grid_containers.wasm

.PHONY: build wasm check test

wasm:
	cargo build --release --target wasm32-wasip1

build: wasm
	mkdir -p $(CUO_REPO)/ecs-mods/$(NAME)
	cp $(RAW) $(CUO_REPO)/ecs-mods/$(NAME)/mod.wasm
	printf '{\n  "name": "$(NAME)",\n  "version": "0.1.0",\n  "wasm": "mod.wasm",\n  "ruleset": {}\n}\n' > $(CUO_REPO)/ecs-mods/$(NAME)/mod.json
	@echo ">> $(CUO_REPO)/ecs-mods/$(NAME)/mod.wasm"

check:
	cargo check --target wasm32-wasip1

test:
	cargo test
