SCHEMA_OUTPUT ?= schemas/curated
SUPERUSER_CSV ?= schemas/command-lists/linux-superuser.csv
DEVOPS_CSV ?= schemas/command-lists/dev-devops-toolchains.csv
CUSTOM_CSV ?= schemas/command-lists/custom-tools.csv

.PHONY: schema-extract-superuser schema-extract-devops schema-extract-custom schema-extract-all
.PHONY: schema-extract-superuser-installed schema-extract-devops-installed schema-extract-custom-installed schema-extract-all-installed
.PHONY: schema-validate schema-snapshot-test

schema-extract-superuser:
	@commands="$$(tr -d '\r\n' < "$(SUPERUSER_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-devops:
	@commands="$$(tr -d '\r\n' < "$(DEVOPS_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-custom:
	@commands="$$(tr -d '\r\n' < "$(CUSTOM_CSV)")"; \
	if [ -z "$$commands" ]; then echo "custom-tools.csv is empty"; exit 1; fi; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-all:
	@commands="$$(cat "$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)" | tr ',' '\n' | sed 's/\r//g' | sed '/^\s*$$/d' | awk '!seen[$$0]++' | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --output "$(SCHEMA_OUTPUT)"

schema-extract-superuser-installed:
	@commands="$$(tr -d '\r\n' < "$(SUPERUSER_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-devops-installed:
	@commands="$$(tr -d '\r\n' < "$(DEVOPS_CSV)")"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-custom-installed:
	@commands="$$(tr -d '\r\n' < "$(CUSTOM_CSV)")"; \
	if [ -z "$$commands" ]; then echo "custom-tools.csv is empty"; exit 1; fi; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-extract-all-installed:
	@commands="$$(cat "$(SUPERUSER_CSV)" "$(DEVOPS_CSV)" "$(CUSTOM_CSV)" | tr ',' '\n' | sed 's/\r//g' | sed '/^\s*$$/d' | awk '!seen[$$0]++' | paste -sd, -)"; \
	cargo run -p command-schema-discovery -- extract --commands "$$commands" --installed-only --output "$(SCHEMA_OUTPUT)"

schema-validate:
	cargo run -p command-schema-discovery -- validate "$(SCHEMA_OUTPUT)"

schema-snapshot-test:
	@echo "Running snapshot extraction test..."
	@cargo run -p command-schema-discovery -- extract \
		--commands "git,cargo,apt,ls,tar,stty" \
		--output "schemas/test-snapshot"
	@echo "Comparing with curated schemas..."
	@diff -r schemas/curated schemas/test-snapshot || \
		(echo "Schema differences detected. Review changes."; exit 1)
