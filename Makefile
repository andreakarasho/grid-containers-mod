# ClassicUO checkout that receives the built mod (and whose SDK it builds against).
CUO_REPO ?= ../cuo-agents
NAME := grid-containers
RAW := target/wasm32-wasip2/release/grid_containers.wasm

.PHONY: build wasm check test tests

wasm:
	cargo build --release --target wasm32-wasip2

build: wasm
	mkdir -p $(CUO_REPO)/ecs-mods/$(NAME)
	cp $(RAW) $(CUO_REPO)/ecs-mods/$(NAME)/mod.wasm
	printf '{\n  "name": "$(NAME)",\n  "version": "0.1.0",\n  "wasm": "mod.wasm",\n  "ruleset": {}\n}\n' > $(CUO_REPO)/ecs-mods/$(NAME)/mod.json
	@echo ">> $(CUO_REPO)/ecs-mods/$(NAME)/mod.wasm"

check:
	cargo check --target wasm32-wasip2

test:
	cargo test

# Host-driven tests (tests/*.cs) run inside the ClassicUO test project: install the include,
# then `make build` and `dotnet test tests/ClassicUO.Ecs.Tests` in $(CUO_REPO).
tests:
	mkdir -p $(CUO_REPO)/tests/ClassicUO.Ecs.Tests/ModTests
	cp tests/ModTests.targets $(CUO_REPO)/tests/ClassicUO.Ecs.Tests/ModTests/$(NAME).targets
	@echo ">> installed ModTests/$(NAME).targets; run 'dotnet test tests/ClassicUO.Ecs.Tests' in $(CUO_REPO)"
